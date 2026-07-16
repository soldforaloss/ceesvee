import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { readDir, readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import { useState } from "react";

import { DELIMITED_EXTENSIONS, delimitedFilesInDir } from "../lib/append";
import { describeRecipeStep, parseRecipeJson, RECIPE_VERSION } from "../lib/recipes";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { BatchOptions, Recipe, RecipeStep, TransformSpec } from "../types";
import { Modal } from "./Modal";

type StepKind = RecipeStep["type"];

const STEP_LABELS: Record<StepKind, string> = {
  reparse: "Parse settings",
  validateProfile: "Validate against profile",
  filter: "Filter (from the active document)",
  transform: "Transform",
  deduplicate: "Deduplicate",
  selectColumns: "Select columns",
  sort: "Sort",
  export: "Export",
};

const SIMPLE_TRANSFORMS: [string, string][] = [
  ["trim", "Trim whitespace"],
  ["collapseWhitespace", "Collapse whitespace"],
  ["uppercase", "UPPERCASE"],
  ["lowercase", "lowercase"],
  ["titleCase", "Title Case"],
];

/**
 * Batch recipes (F25): a versioned, DECLARATIVE step sequence from a closed
 * set — no scripting, no shell, no network. Inputs are explicitly selected
 * files/folders; outputs go only into the chosen folder; nothing is
 * overwritten by default; dry runs write nothing at all.
 */
export function RecipeDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const settings = useStore((s) => s.settings);
  const filterSpec = useStore((s) => s.filter.spec);
  const batch = useStore((s) => s.batch);
  const trackBatch = useStore((s) => s.trackBatch);
  const cancelBatch = useStore((s) => s.cancelBatch);
  const clearBatch = useStore((s) => s.clearBatch);

  const [name, setName] = useState("my-recipe");
  const [steps, setSteps] = useState<RecipeStep[]>([
    {
      type: "export",
      options: {
        delimiter: ",",
        encoding: "UTF-8",
        quoteStyle: "minimal",
        lineEnding: "lf",
        bom: false,
        includeHeaders: true,
      },
    },
  ]);
  const [stepKind, setStepKind] = useState<StepKind>("transform");
  const [transformKind, setTransformKind] = useState("trim");
  const [transformColumns, setTransformColumns] = useState("");
  const [profileId, setProfileId] = useState("");
  const [selectCols, setSelectCols] = useState("");
  const [sortColumn, setSortColumn] = useState("");
  const [files, setFiles] = useState<string[]>([]);
  const [outputDir, setOutputDir] = useState("");
  const [template, setTemplate] = useState("{name}_clean.{ext}");
  const [overwrite, setOverwrite] = useState(false);
  const [continueOnError, setContinueOnError] = useState(true);
  const [concurrency, setConcurrency] = useState(2);
  const [error, setError] = useState<string | null>(null);

  const running = batch !== null && batch.report === null && batch.error === null;
  const report = batch?.report ?? null;

  const buildRecipe = (): Recipe => ({ version: RECIPE_VERSION, name, steps });
  const buildOptions = (dryRun: boolean): BatchOptions => ({
    recipe: buildRecipe(),
    files,
    outputDir,
    filenameTemplate: template,
    overwrite,
    continueOnError,
    dryRun,
    concurrency,
  });

  const addStep = () => {
    setError(null);
    let step: RecipeStep | null = null;
    switch (stepKind) {
      case "transform":
        step = {
          type: "transform",
          spec: { type: transformKind } as TransformSpec,
          columns: transformColumns
            .split(",")
            .map((c) => c.trim())
            .filter((c) => c !== ""),
        };
        break;
      case "validateProfile":
        if (!profileId) {
          setError("pick a profile first");
          return;
        }
        step = { type: "validateProfile", profileId, failOnIssues: true };
        break;
      case "filter":
        if (!filterSpec) {
          setError("the active document has no filter to capture");
          return;
        }
        step = { type: "filter", spec: filterSpec };
        break;
      case "selectColumns": {
        const columns = selectCols
          .split(",")
          .map((c) => c.trim())
          .filter((c) => c !== "");
        if (columns.length === 0) {
          setError("list the columns to keep (comma-separated)");
          return;
        }
        step = { type: "selectColumns", columns };
        break;
      }
      case "sort":
        if (!sortColumn.trim()) {
          setError("name the sort column");
          return;
        }
        step = { type: "sort", keys: [{ column: sortColumn.trim(), descending: false }] };
        break;
      case "deduplicate":
        step = {
          type: "deduplicate",
          spec: {
            keyColumns: [],
            trim: true,
            caseInsensitive: false,
            collapseWhitespace: false,
            blankKeysEqual: false,
            excludeBlankKeys: false,
          },
          keep: "first",
        };
        break;
      case "reparse":
        step = { type: "reparse", delimiter: null, encoding: null, hasHeaderRow: true };
        break;
      case "export":
        step = {
          type: "export",
          options: {
            delimiter: ",",
            encoding: "UTF-8",
            quoteStyle: "minimal",
            lineEnding: "lf",
            bom: false,
            includeHeaders: true,
          },
        };
        break;
    }
    if (step) {
      // Export steps stay last; everything else inserts before them.
      setSteps((s) => {
        const exports = s.filter((x) => x.type === "export");
        const rest = s.filter((x) => x.type !== "export");
        return step.type === "export" ? [...rest, ...exports, step] : [...rest, step, ...exports];
      });
    }
  };

  const addFiles = async () => {
    const chosen = await openFileDialog({
      multiple: true,
      filters: [{ name: "Delimited text", extensions: DELIMITED_EXTENSIONS }],
    });
    const picked = Array.isArray(chosen) ? chosen : chosen ? [chosen] : [];
    if (picked.length > 0) setFiles((f) => [...f, ...picked.filter((p) => !f.includes(p))]);
  };

  const addFolder = async () => {
    const dir = await openFileDialog({ directory: true });
    if (typeof dir !== "string") return;
    try {
      const entries = await readDir(dir);
      const matched = delimitedFilesInDir(
        dir,
        entries.map((e) => ({ name: e.name, isFile: e.isFile })),
      );
      setFiles((f) => [...f, ...matched.filter((p) => !f.includes(p))]);
    } catch (e) {
      setError(String(e));
    }
  };

  const pickOutputDir = async () => {
    const dir = await openFileDialog({ directory: true });
    if (typeof dir === "string") setOutputDir(dir);
  };

  const saveRecipe = async () => {
    const chosen = await saveFileDialog({
      defaultPath: `${name}.ceesvee-recipe.json`,
      filters: [{ name: "CEESVEE recipe", extensions: ["json"] }],
    });
    if (typeof chosen === "string") {
      await writeTextFile(chosen, JSON.stringify(buildRecipe(), null, 2));
    }
  };

  const loadRecipe = async () => {
    const chosen = await openFileDialog({
      filters: [{ name: "CEESVEE recipe", extensions: ["json"] }],
    });
    if (typeof chosen !== "string") return;
    try {
      const recipe = parseRecipeJson(await readTextFile(chosen));
      setName(recipe.name);
      setSteps(recipe.steps);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const run = async (dryRun: boolean) => {
    setError(null);
    clearBatch();
    try {
      await api.validateRecipeBatch(buildOptions(dryRun));
      trackBatch(await api.startRecipeBatch(buildOptions(dryRun)));
    } catch (e) {
      setError(String(e));
    }
  };

  const ready = files.length > 0 && outputDir !== "" && steps.length > 0;

  return (
    <Modal
      title="Batch recipes"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-xs text-zinc-400">
            Declarative steps only — no scripts, no network, no overwrites by default.
          </span>
          <button onClick={() => void loadRecipe()} className={btnGhost}>
            Load…
          </button>
          <button onClick={() => void saveRecipe()} className={btnGhost}>
            Save…
          </button>
          <button onClick={() => void run(true)} disabled={!ready || running} className={btnGhost}>
            Dry run
          </button>
          <button
            onClick={() => void run(false)}
            disabled={!ready || running}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {running ? "Processing…" : "Run batch"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <label className="flex items-center gap-1.5">
            Recipe name
            <input value={name} onChange={(e) => setName(e.target.value)} className={inputCls} />
          </label>
        </div>

        {/* Steps */}
        <div className="space-y-1 text-xs">
          {steps.map((step, i) => (
            <div
              key={i}
              className="flex items-center gap-2 rounded border border-zinc-200 px-2 py-1 dark:border-zinc-800"
            >
              <span className="text-zinc-400">{i + 1}.</span>
              <span className="truncate">{describeRecipeStep(step)}</span>
              <span className="flex-1" />
              <button
                onClick={() => setSteps(steps.filter((_, j) => j !== i))}
                className="text-red-600 hover:underline dark:text-red-400"
              >
                remove
              </button>
            </div>
          ))}
          <div className="flex flex-wrap items-center gap-2 pt-1">
            <select
              value={stepKind}
              onChange={(e) => setStepKind(e.target.value as StepKind)}
              className={selectCls}
            >
              {Object.entries(STEP_LABELS).map(([value, label]) => (
                <option key={value} value={value} className="dark:bg-zinc-800">
                  {label}
                </option>
              ))}
            </select>
            {stepKind === "transform" && (
              <>
                <select
                  value={transformKind}
                  onChange={(e) => setTransformKind(e.target.value)}
                  className={selectCls}
                >
                  {SIMPLE_TRANSFORMS.map(([value, label]) => (
                    <option key={value} value={value} className="dark:bg-zinc-800">
                      {label}
                    </option>
                  ))}
                </select>
                <input
                  value={transformColumns}
                  onChange={(e) => setTransformColumns(e.target.value)}
                  placeholder="columns (blank = all)"
                  className={inputCls}
                />
              </>
            )}
            {stepKind === "validateProfile" && (
              <select
                value={profileId}
                onChange={(e) => setProfileId(e.target.value)}
                className={selectCls}
              >
                <option value="" className="dark:bg-zinc-800">
                  pick a profile…
                </option>
                {(settings?.profiles ?? []).map((p) => (
                  <option key={p.id} value={p.id} className="dark:bg-zinc-800">
                    {p.name}
                  </option>
                ))}
              </select>
            )}
            {stepKind === "selectColumns" && (
              <input
                value={selectCols}
                onChange={(e) => setSelectCols(e.target.value)}
                placeholder="columns to keep, comma-separated"
                className={inputCls}
              />
            )}
            {stepKind === "sort" && (
              <input
                value={sortColumn}
                onChange={(e) => setSortColumn(e.target.value)}
                placeholder="sort column"
                className={inputCls}
              />
            )}
            {stepKind === "filter" && !meta && (
              <span className="text-zinc-400">open a document to capture its filter</span>
            )}
            <button onClick={addStep} className={chipBtn}>
              + add step
            </button>
          </div>
        </div>

        {/* IO */}
        <div className="space-y-1.5 text-xs">
          <div className="flex flex-wrap items-center gap-2">
            <button onClick={() => void addFiles()} className={chipBtn}>
              Add files…
            </button>
            <button onClick={() => void addFolder()} className={chipBtn}>
              Add folder…
            </button>
            <span className="text-zinc-400">
              {files.length} input file{files.length === 1 ? "" : "s"}
            </span>
            {files.length > 0 && (
              <button
                onClick={() => setFiles([])}
                className="text-red-600 hover:underline dark:text-red-400"
              >
                clear
              </button>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <button onClick={() => void pickOutputDir()} className={chipBtn}>
              Output folder…
            </button>
            <span className="max-w-[20rem] truncate font-mono text-[11px] text-zinc-500">
              {outputDir || "(not set)"}
            </span>
            <label className="flex items-center gap-1.5">
              Template
              <input
                value={template}
                onChange={(e) => setTemplate(e.target.value)}
                className="w-44 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 font-mono dark:border-zinc-600"
              />
            </label>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <label className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={overwrite}
                onChange={(e) => setOverwrite(e.target.checked)}
                className="accent-violet-600"
              />
              Overwrite existing outputs
            </label>
            <label className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={continueOnError}
                onChange={(e) => setContinueOnError(e.target.checked)}
                className="accent-violet-600"
              />
              Continue past failing files
            </label>
            <label className="flex items-center gap-1.5">
              Parallel files
              <input
                type="number"
                min={1}
                max={8}
                value={concurrency}
                onChange={(e) => setConcurrency(Number(e.target.value))}
                className="w-14 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
              />
            </label>
          </div>
        </div>

        {(error ?? batch?.error) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? batch?.error}</p>
        )}

        {running && batch && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              {batch.message ?? "processing"} — {batch.processed}
              {batch.total != null && ` / ${batch.total}`} files
            </span>
            <button
              onClick={() => void cancelBatch()}
              className="rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
            >
              Cancel
            </button>
          </div>
        )}

        {report && (
          <div className="space-y-1 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {report.dryRun && "DRY RUN — nothing was written. "}
              {report.ok} ok · {report.skipped} skipped · {report.failed} failed
            </p>
            <div className="max-h-40 space-y-0.5 overflow-y-auto">
              {report.outcomes.map((o, i) => (
                <div key={i} className="flex items-center gap-2">
                  <span
                    className={
                      o.status === "ok"
                        ? "text-emerald-600 dark:text-emerald-400"
                        : o.status === "skipped"
                          ? "text-amber-600 dark:text-amber-400"
                          : "text-red-600 dark:text-red-400"
                    }
                  >
                    {o.status}
                  </span>
                  <span className="max-w-[14rem] truncate font-mono text-[11px]">{o.input}</span>
                  <span className="text-zinc-400">
                    {o.rowsIn} → {o.rowsOut} rows
                    {o.issues > 0 && ` · ${o.issues} issues`}
                  </span>
                  {o.error && <span className="truncate text-zinc-400">{o.error}</span>}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700";
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
const inputCls =
  "w-44 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-600";
