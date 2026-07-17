import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useState } from "react";

import {
  METHOD_LABELS,
  PARTITION_PRESETS,
  generateSeed,
  isIntegerCountMethod,
  methodProblem,
  normalizeWeights,
  parseSeed,
  partitionProblem,
  projectPartitionCounts,
  projectSampleCount,
  type MethodKey,
} from "../lib/sampling";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type {
  ExportOptions,
  PartitionOutput,
  SampleDestination,
  SamplePlan,
  SamplePreview,
  SampleRequest,
  SamplingMethod,
} from "../types";
import { Modal } from "./Modal";

type Mode = "sampling" | "partitioning";
type PartMode = "plain" | "stratified" | "group";

/**
 * Sampling & partitioning (F48): carve a reproducible subset out of the active
 * document, or split it into disjoint weighted partitions — without deleting a
 * single source row. Every run is driven by an explicit, surfaced seed, so the
 * same source + settings + seed produce byte-identical outputs. Outputs become
 * NEW documents or direct CSV exports (with an optional JSON manifest). Works
 * on indexed (read-only) sources too — the source is never mutated.
 */
export function SamplingDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const sample = useStore((s) => s.sample);
  const sampleError = useStore((s) => s.sampleError);
  const trackSample = useStore((s) => s.trackSample);
  const cancelSample = useStore((s) => s.cancelSample);
  const clearSampleError = useStore((s) => s.clearSampleError);
  const initialMode = useStore((s) => s.samplingInitialMode);

  const [mode, setMode] = useState<Mode>(initialMode);

  // ----- sampling method params ------------------------------------------------
  const [methodType, setMethodType] = useState<MethodKey>("head");
  const [count, setCount] = useState(100);
  const [percent, setPercent] = useState(10);
  const [step, setStep] = useState(10);
  const [offsetMode, setOffsetMode] = useState<"random" | "fixed">("random");
  const [offset, setOffset] = useState(0);
  const [fraction, setFraction] = useState(0.1);
  const [tolerance, setTolerance] = useState(0.02);
  const [perStratum, setPerStratum] = useState(50);
  // Stable column IDs for stratified / balanced / hash-specific-columns.
  const [sampleColumns, setSampleColumns] = useState<string[]>([]);
  const [hashUseColumns, setHashUseColumns] = useState(false);

  // ----- partition params ------------------------------------------------------
  const [parts, setParts] = useState<PartitionOutput[]>(PARTITION_PRESETS[0].parts);
  const [partMode, setPartMode] = useState<PartMode>("plain");
  const [partColumns, setPartColumns] = useState<string[]>([]);

  // ----- shared ----------------------------------------------------------------
  const [scopeVisible, setScopeVisible] = useState(false);
  const [shuffle, setShuffle] = useState(false);
  const [seedText, setSeedText] = useState(() => String(generateSeed()));
  const [dest, setDest] = useState<"derivedDocuments" | "export">("derivedDocuments");
  const [exportDir, setExportDir] = useState<string | null>(null);
  const [baseName, setBaseName] = useState("sample");
  const [writeManifest, setWriteManifest] = useState(true);

  const [preview, setPreview] = useState<SamplePreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState(false);

  const seed = parseSeed(seedText);
  const running = sample !== null;

  // The dialog closes itself the moment a successful run finishes (the new
  // documents open underneath, or the files are on disk); a failure keeps it
  // open with the error so the user can adjust and retry.
  useEffect(() => {
    if (!submitted || running) return;
    if (sampleError) {
      setSubmitted(false);
    } else {
      onClose();
    }
  }, [submitted, running, sampleError, onClose]);

  const buildMethod = (): SamplingMethod => {
    switch (methodType) {
      case "head":
        return { type: "head", n: count };
      case "tail":
        return { type: "tail", n: count };
      case "randomCount":
        return { type: "randomCount", n: count };
      case "randomPercentage":
        return { type: "randomPercentage", percent };
      case "systematic":
        return { type: "systematic", step, offset: offsetMode === "fixed" ? offset : null };
      case "stratified":
        return { type: "stratified", columns: sampleColumns, fraction, tolerance };
      case "balanced":
        return { type: "balanced", columns: sampleColumns, perStratum };
      case "hashDeterministic":
        return {
          type: "hashDeterministic",
          columns: hashUseColumns ? sampleColumns : null,
          percent,
        };
    }
  };

  const buildPlan = (): SamplePlan =>
    mode === "sampling"
      ? { kind: "sampling", ...buildMethod() }
      : {
          kind: "partitioning",
          parts,
          stratifyBy: partMode === "stratified" ? partColumns : [],
          groupBy: partMode === "group" ? partColumns : [],
          allowOverlap: false,
        };

  const buildDestination = (): SampleDestination => {
    if (dest === "derivedDocuments") return { type: "derivedDocuments" };
    const options: ExportOptions = {
      delimiter: meta?.delimiter ?? ",",
      encoding: meta?.encoding ?? "UTF-8",
      quoteStyle: "minimal",
      lineEnding: meta?.lineEnding ?? "lf",
      bom: meta?.hadBom ?? false,
      includeHeaders: meta?.hasHeaderRow ?? true,
      backup: "none",
    };
    return {
      type: "export",
      dir: exportDir ?? "",
      baseName: baseName.trim(),
      options,
      writeManifest,
    };
  };

  const buildRequest = (): SampleRequest => ({
    plan: buildPlan(),
    scope: scopeVisible ? "visibleRows" : "all",
    order: shuffle ? "shuffle" : "sourceOrder",
    seed,
    destination: buildDestination(),
  });

  // Counts are destination-independent; only re-preview when the SELECTION
  // (plan + scope + order + seed) changes.
  const previewKey = JSON.stringify({
    plan: buildPlan(),
    scope: scopeVisible,
    order: shuffle,
    seed,
  });
  useEffect(() => {
    setPreview(null);
  }, [previewKey]);

  const scopeTotal = useMemo(() => {
    if (!meta) return 0;
    return scopeVisible ? meta.rowCount : meta.totalRowCount;
  }, [meta, scopeVisible]);

  if (!meta) return null;

  const columns = meta.columnIds.map((id, i) => ({
    id,
    label: meta.headers[i] || `Column ${i + 1}`,
  }));

  const planProblem =
    mode === "sampling"
      ? seed == null
        ? "Enter a whole-number seed"
        : methodProblem(buildMethod())
      : seed == null
        ? "Enter a whole-number seed"
        : partitionProblem({
            parts,
            stratifyBy: partMode === "stratified" ? partColumns : [],
            groupBy: partMode === "group" ? partColumns : [],
            allowOverlap: false,
          });

  const destProblem =
    dest === "export"
      ? !exportDir
        ? "Choose an output folder"
        : baseName.trim() === ""
          ? "Enter a base file name"
          : null
      : null;

  const stale = preview !== null && preview.expectedRevision !== meta.revision;
  const canPreview = planProblem === null && !running;
  const canRun = canPreview && destProblem === null && preview !== null && !stale;

  // Live client-side projection (before a preview round-trip).
  const liveProjection: { name: string; projected: number | null }[] =
    mode === "sampling"
      ? [{ name: "sample", projected: projectSampleCount(buildMethod(), scopeTotal) }]
      : (() => {
          const counts = partMode === "plain" ? projectPartitionCounts(parts, scopeTotal) : null;
          return parts.map((p, i) => ({
            name: p.name || `part ${i + 1}`,
            projected: counts ? counts[i] : null,
          }));
        })();

  const runPreview = async () => {
    setError(null);
    try {
      const result = await api.previewSample(meta.id, buildRequest(), meta.revision);
      setPreview(result);
      // The backend echoes the seed we sent; keep the field in exact sync.
      setSeedText(String(result.seed));
    } catch (e) {
      setError(String(e));
      setPreview(null);
    }
  };

  const run = async () => {
    if (seed == null) return;
    setError(null);
    clearSampleError();
    try {
      const started = await api.startSample(meta.id, buildRequest(), seed, meta.revision);
      trackSample(started.jobId, started.docIds, dest);
      setSubmitted(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const chooseFolder = async () => {
    const dir = await openFolderDialog({ directory: true });
    if (typeof dir === "string") setExportDir(dir);
  };

  const runLabel =
    mode === "sampling"
      ? dest === "export"
        ? "Sample & export"
        : "Sample into a new document"
      : dest === "export"
        ? "Partition & export"
        : "Split into new documents";

  return (
    <Modal
      title="Sample & partition"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
          <button onClick={() => void runPreview()} disabled={!canPreview} className={btnGhost}>
            Preview counts
          </button>
          <button
            onClick={() => void run()}
            disabled={!canRun}
            title={
              planProblem ??
              destProblem ??
              (preview === null
                ? "Preview first"
                : stale
                  ? "The document changed — preview again"
                  : undefined)
            }
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {running ? "Running…" : runLabel}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {/* Mode */}
        <div className="flex overflow-hidden rounded border border-zinc-300 text-xs dark:border-zinc-700">
          {(["sampling", "partitioning"] as Mode[]).map((m) => (
            <button
              key={m}
              onClick={() => setMode(m)}
              className={`flex-1 px-3 py-1.5 ${
                mode === m
                  ? "bg-violet-600 text-white"
                  : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              }`}
            >
              {m === "sampling" ? "Sample a subset" : "Partition into splits"}
            </button>
          ))}
        </div>

        {/* ----- Sampling method ----- */}
        {mode === "sampling" && (
          <div className="space-y-2">
            <label className="flex items-center gap-1.5 text-xs">
              Method
              <select
                value={methodType}
                onChange={(e) => setMethodType(e.target.value as MethodKey)}
                className={selectCls}
              >
                {(Object.keys(METHOD_LABELS) as MethodKey[]).map((k) => (
                  <option key={k} value={k} className="dark:bg-zinc-800">
                    {METHOD_LABELS[k]}
                  </option>
                ))}
              </select>
            </label>

            <div className="flex flex-wrap items-center gap-3 text-xs">
              {isIntegerCountMethod(methodType) && (
                <Field label="Count">
                  <input
                    type="number"
                    min={1}
                    value={count}
                    onChange={(e) => setCount(Math.floor(Number(e.target.value)))}
                    className={numCls}
                  />
                </Field>
              )}
              {(methodType === "randomPercentage" || methodType === "hashDeterministic") && (
                <Field label="Percentage">
                  <input
                    type="number"
                    min={0}
                    max={100}
                    step={0.5}
                    value={percent}
                    onChange={(e) => setPercent(Number(e.target.value))}
                    className={numCls}
                  />
                  <span className="text-zinc-400">%</span>
                </Field>
              )}
              {methodType === "systematic" && (
                <>
                  <Field label="Every">
                    <input
                      type="number"
                      min={1}
                      value={step}
                      onChange={(e) => setStep(Math.floor(Number(e.target.value)))}
                      className={numCls}
                    />
                    <span className="text-zinc-400">rows</span>
                  </Field>
                  <Field label="Start">
                    <select
                      value={offsetMode}
                      onChange={(e) => setOffsetMode(e.target.value as "random" | "fixed")}
                      className={selectCls}
                    >
                      <option value="random" className="dark:bg-zinc-800">
                        random offset (from seed)
                      </option>
                      <option value="fixed" className="dark:bg-zinc-800">
                        fixed offset
                      </option>
                    </select>
                  </Field>
                  {offsetMode === "fixed" && (
                    <Field label="Offset">
                      <input
                        type="number"
                        min={0}
                        value={offset}
                        onChange={(e) => setOffset(Math.floor(Number(e.target.value)))}
                        className={numCls}
                      />
                    </Field>
                  )}
                </>
              )}
              {methodType === "stratified" && (
                <>
                  <Field label="Fraction">
                    <input
                      type="number"
                      min={0}
                      max={1}
                      step={0.01}
                      value={fraction}
                      onChange={(e) => setFraction(Number(e.target.value))}
                      className={numCls}
                    />
                  </Field>
                  <Field label="Tolerance">
                    <input
                      type="number"
                      min={0}
                      max={1}
                      step={0.01}
                      value={tolerance}
                      onChange={(e) => setTolerance(Number(e.target.value))}
                      className={numCls}
                    />
                  </Field>
                </>
              )}
              {methodType === "balanced" && (
                <Field label="Rows per group">
                  <input
                    type="number"
                    min={1}
                    value={perStratum}
                    onChange={(e) => setPerStratum(Math.floor(Number(e.target.value)))}
                    className={numCls}
                  />
                </Field>
              )}
              {methodType === "hashDeterministic" && (
                <label className="flex items-center gap-1.5">
                  <input
                    type="checkbox"
                    checked={hashUseColumns}
                    onChange={(e) => setHashUseColumns(e.target.checked)}
                    className="accent-violet-600"
                  />
                  Hash specific columns (else the whole row)
                </label>
              )}
            </div>

            {(methodType === "stratified" ||
              methodType === "balanced" ||
              (methodType === "hashDeterministic" && hashUseColumns)) && (
              <ColumnChecklist
                label={methodType === "hashDeterministic" ? "Hash columns" : "Group by columns"}
                columns={columns}
                selected={sampleColumns}
                onChange={setSampleColumns}
              />
            )}
          </div>
        )}

        {/* ----- Partition editor ----- */}
        {mode === "partitioning" && (
          <div className="space-y-2">
            <div className="flex items-center gap-2 text-xs">
              <span className="text-zinc-500 dark:text-zinc-400">Preset</span>
              <select
                defaultValue=""
                onChange={(e) => {
                  const preset = PARTITION_PRESETS.find((p) => p.id === e.target.value);
                  if (preset) setParts(preset.parts.map((p) => ({ ...p })));
                }}
                className={selectCls}
              >
                <option value="" className="dark:bg-zinc-800">
                  Custom…
                </option>
                {PARTITION_PRESETS.map((p) => (
                  <option key={p.id} value={p.id} className="dark:bg-zinc-800">
                    {p.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              {parts.map((part, i) => {
                const fractions = normalizeWeights(parts);
                const projected =
                  partMode === "plain" ? projectPartitionCounts(parts, scopeTotal)[i] : null;
                return (
                  <div key={i} className="flex flex-wrap items-center gap-2 text-xs">
                    <input
                      value={part.name}
                      onChange={(e) =>
                        setParts((ps) =>
                          ps.map((p, j) => (j === i ? { ...p, name: e.target.value } : p)),
                        )
                      }
                      placeholder="name"
                      className="w-36 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 dark:border-zinc-600"
                    />
                    <span className="text-zinc-400">weight</span>
                    <input
                      type="number"
                      min={0}
                      step="any"
                      value={part.weight}
                      onChange={(e) =>
                        setParts((ps) =>
                          ps.map((p, j) =>
                            j === i ? { ...p, weight: Number(e.target.value) } : p,
                          ),
                        )
                      }
                      className={numCls}
                    />
                    <span className="w-14 text-right tabular-nums text-zinc-500 dark:text-zinc-400">
                      {(fractions[i] * 100).toFixed(1)}%
                    </span>
                    {projected != null && (
                      <span className="tabular-nums text-zinc-400">
                        ≈ {projected.toLocaleString()} rows
                      </span>
                    )}
                    {parts.length > 2 && (
                      <button
                        onClick={() => setParts((ps) => ps.filter((_, j) => j !== i))}
                        className="text-red-600 hover:underline dark:text-red-400"
                      >
                        remove
                      </button>
                    )}
                  </div>
                );
              })}
              <button
                onClick={() =>
                  setParts((ps) => [...ps, { name: `part${ps.length + 1}`, weight: 1 }])
                }
                className={`${chipBtn} mt-1`}
              >
                + partition
              </button>
            </div>

            <div className="flex flex-wrap items-center gap-3 text-xs">
              <Field label="Constraint">
                <select
                  value={partMode}
                  onChange={(e) => setPartMode(e.target.value as PartMode)}
                  className={selectCls}
                >
                  <option value="plain" className="dark:bg-zinc-800">
                    split independently
                  </option>
                  <option value="stratified" className="dark:bg-zinc-800">
                    stratified by columns
                  </option>
                  <option value="group" className="dark:bg-zinc-800">
                    keep groups together
                  </option>
                </select>
              </Field>
            </div>

            {partMode !== "plain" && (
              <ColumnChecklist
                label={partMode === "stratified" ? "Stratify by" : "Group key"}
                columns={columns}
                selected={partColumns}
                onChange={setPartColumns}
              />
            )}
          </div>
        )}

        <hr className="border-zinc-100 dark:border-zinc-800" />

        {/* ----- Seed / scope / order ----- */}
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <Field label="Seed">
            <input
              value={seedText}
              onChange={(e) => setSeedText(e.target.value)}
              className="w-44 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 font-mono dark:border-zinc-600"
            />
            <button
              onClick={() => setSeedText(String(generateSeed()))}
              title="Generate a new random seed"
              className={chipBtn}
            >
              Regenerate
            </button>
          </Field>
          <label className="flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={shuffle}
              onChange={(e) => setShuffle(e.target.checked)}
              className="accent-violet-600"
            />
            Shuffle output (else preserve source order)
          </label>
          {meta.filtered && (
            <label className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={scopeVisible}
                onChange={(e) => setScopeVisible(e.target.checked)}
                className="accent-violet-600"
              />
              Visible rows only
            </label>
          )}
        </div>
        {seed == null && (
          <p className="text-xs text-amber-600 dark:text-amber-400">
            The seed must be a whole number (0 – {Number.MAX_SAFE_INTEGER.toLocaleString()}).
          </p>
        )}

        {/* ----- Destination ----- */}
        <div className="space-y-1.5 text-xs">
          <div className="flex gap-4">
            <label className="flex items-center gap-1.5">
              <input
                type="radio"
                checked={dest === "derivedDocuments"}
                onChange={() => setDest("derivedDocuments")}
                className="accent-violet-600"
              />
              New document{mode === "partitioning" ? "s" : ""}
            </label>
            <label className="flex items-center gap-1.5">
              <input
                type="radio"
                checked={dest === "export"}
                onChange={() => setDest("export")}
                className="accent-violet-600"
              />
              Export to CSV files
            </label>
          </div>
          {dest === "export" && (
            <div className="flex flex-wrap items-center gap-2 pl-5">
              <button onClick={() => void chooseFolder()} className={chipBtn}>
                Choose folder…
              </button>
              <span className="max-w-[18rem] truncate text-zinc-500 dark:text-zinc-400">
                {exportDir ?? "no folder chosen"}
              </span>
              <Field label="Base name">
                <input
                  value={baseName}
                  onChange={(e) => setBaseName(e.target.value)}
                  className="w-40 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 dark:border-zinc-600"
                />
              </Field>
              <label className="flex items-center gap-1.5">
                <input
                  type="checkbox"
                  checked={writeManifest}
                  onChange={(e) => setWriteManifest(e.target.checked)}
                  className="accent-violet-600"
                />
                JSON manifest (counts + SHA-256)
              </label>
            </div>
          )}
          {dest === "export" && (
            <p className="pl-5 text-[11px] text-zinc-400">
              Files inherit the source dialect ({meta.delimiter === "\t" ? "tab" : meta.delimiter}
              -delimited, {meta.encoding}). Use Export… for full formatting control.
            </p>
          )}
        </div>

        {(error ?? sampleError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? sampleError}</p>
        )}

        {stale && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            The document changed since this preview — preview again before running.
          </p>
        )}

        {running && sample && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              {sample.message ?? "working"} — {sample.processed.toLocaleString()}
              {sample.total != null && ` / ${sample.total.toLocaleString()}`} rows
            </span>
            <button
              onClick={() => void cancelSample()}
              className="rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
            >
              Cancel
            </button>
          </div>
        )}

        {/* ----- Counts ----- */}
        <div className="space-y-1.5 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
          <p className="font-medium">
            {preview ? (
              <>
                {preview.totalRows.toLocaleString()} rows in scope · seed{" "}
                <span className="font-mono">{preview.seed}</span>
              </>
            ) : (
              <>
                {scopeTotal.toLocaleString()} rows in scope · projected estimate (Preview for exact
                counts)
              </>
            )}
          </p>
          <table className="w-full text-left tabular-nums">
            <thead className="text-zinc-400">
              <tr>
                <th className="pr-4 font-normal">Output</th>
                <th className="pr-4 font-normal">Projected</th>
                {preview && <th className="pr-4 font-normal">Exact</th>}
              </tr>
            </thead>
            <tbody className="text-zinc-600 dark:text-zinc-300">
              {(preview
                ? preview.outputs.map((o) => ({
                    name: o.name,
                    projected: o.projected,
                    exact: o.exact as number | null,
                  }))
                : liveProjection.map((o) => ({ ...o, exact: null }))
              ).map((o, i) => (
                <tr key={i}>
                  <td className="pr-4 font-mono">{o.name}</td>
                  <td className="pr-4">
                    {o.projected == null ? "—" : o.projected.toLocaleString()}
                  </td>
                  {preview && (
                    <td className="pr-4 font-medium">
                      {o.exact == null ? "—" : o.exact.toLocaleString()}
                    </td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
          {preview && (
            <p className="text-zinc-400">
              Outputs are disjoint and reproducible: the same source + settings + this seed always
              produce identical results.
            </p>
          )}
        </div>

        {/* Strata table */}
        {preview?.strata && preview.strata.length > 0 && (
          <div className="max-h-[24vh] overflow-y-auto rounded border border-zinc-200 dark:border-zinc-800">
            <table className="w-full text-left text-[11px] tabular-nums">
              <thead className="sticky top-0 bg-zinc-50 text-zinc-400 dark:bg-zinc-900">
                <tr>
                  <th className="px-2 py-1 font-normal">Stratum</th>
                  <th className="px-2 py-1 font-normal">Population</th>
                  <th className="px-2 py-1 font-normal">Selected</th>
                  <th className="px-2 py-1 font-normal">Fraction</th>
                </tr>
              </thead>
              <tbody className="text-zinc-600 dark:text-zinc-300">
                {preview.strata.slice(0, 200).map((s, i) => (
                  <tr key={i} className="border-t border-zinc-100 dark:border-zinc-800/60">
                    <td className="px-2 py-1 font-mono">{s.key.join(" · ") || "∅"}</td>
                    <td className="px-2 py-1">{s.population.toLocaleString()}</td>
                    <td className="px-2 py-1">{s.selected.toLocaleString()}</td>
                    <td className="px-2 py-1">{(s.fraction * 100).toFixed(1)}%</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {/* Warnings */}
        {preview && preview.warnings.length > 0 && (
          <div className="space-y-1 rounded bg-amber-50 p-2 text-[11px] text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            {preview.warnings.map((w, i) => (
              <p key={i}>⚠ {w}</p>
            ))}
          </div>
        )}
      </div>
    </Modal>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="flex items-center gap-1.5">
      <span className="text-zinc-500 dark:text-zinc-400">{label}</span>
      {children}
    </label>
  );
}

function ColumnChecklist({
  label,
  columns,
  selected,
  onChange,
}: {
  label: string;
  columns: { id: string; label: string }[];
  selected: string[];
  onChange: (next: string[]) => void;
}) {
  return (
    <div className="text-xs">
      <p className="mb-1 text-zinc-500 dark:text-zinc-400">{label}:</p>
      <div className="flex max-h-20 flex-wrap gap-x-3 gap-y-1 overflow-y-auto">
        {columns.map((c) => (
          <label key={c.id} className="flex items-center gap-1">
            <input
              type="checkbox"
              checked={selected.includes(c.id)}
              onChange={(e) =>
                onChange(
                  e.target.checked ? [...selected, c.id] : selected.filter((x) => x !== c.id),
                )
              }
              className="accent-violet-600"
            />
            {c.label}
          </label>
        ))}
      </div>
    </div>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700";
const numCls =
  "w-20 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 text-right tabular-nums dark:border-zinc-600";
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
