import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { readDir } from "@tauri-apps/plugin-fs";
import { useState } from "react";

import { DELIMITED_EXTENSIONS, delimitedFilesInDir } from "../lib/append";
import * as api from "../lib/tauri";
import { useStore } from "../store/useStore";
import type { AlignMode, AppendInput, AppendPreview, SchemaMode } from "../types";
import { Modal } from "./Modal";

type AlignKey = "exactName" | "caseInsensitiveName" | "position";

/**
 * Multi-file append (F20): combine rows from open tabs, picked files, or a
 * folder of delimited files into a NEW document. Inputs are never touched;
 * huge outputs automatically open indexed. Everything is previewed —
 * schema, per-input mappings, projected rows/backing — before running.
 */
export function AppendDialog({ onClose }: { onClose: () => void }) {
  const tabs = useStore((s) => s.tabs);
  const derive = useStore((s) => s.derive);
  const deriveError = useStore((s) => s.deriveError);
  const trackDerive = useStore((s) => s.trackDerive);
  const cancelDerive = useStore((s) => s.cancelDerive);

  const [docIds, setDocIds] = useState<number[]>([]);
  const [files, setFiles] = useState<string[]>([]);
  const [align, setAlign] = useState<AlignKey>("exactName");
  const [schema, setSchema] = useState<SchemaMode>("union");
  const [addSourceFile, setAddSourceFile] = useState(true);
  const [addSourceRow, setAddSourceRow] = useState(false);
  const [allowDuplicateHeaders, setAllowDuplicateHeaders] = useState(false);
  const [continueOnError, setContinueOnError] = useState(false);
  const [preview, setPreview] = useState<AppendPreview | null>(null);
  const [error, setError] = useState<string | null>(null);

  const running = derive?.kind === "append";

  const inputs = (): AppendInput[] => [
    ...docIds.map((docId) => ({ type: "openDoc" as const, docId })),
    ...files.map((path) => ({ type: "file" as const, path })),
  ];
  const inputCount = docIds.length + files.length;

  const buildAlign = (): AlignMode => ({ type: align });
  const buildOptions = () => ({
    align: buildAlign(),
    schema,
    addSourceFile,
    addSourceRow,
    allowDuplicateHeaders,
    continueOnError,
  });

  const invalidate = () => setPreview(null);

  const addFiles = async () => {
    const chosen = await openFileDialog({
      multiple: true,
      filters: [{ name: "Delimited text", extensions: DELIMITED_EXTENSIONS }],
    });
    const picked = Array.isArray(chosen) ? chosen : chosen ? [chosen] : [];
    if (picked.length > 0) {
      setFiles((f) => [...f, ...picked.filter((p) => !f.includes(p))]);
      invalidate();
    }
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
      if (matched.length === 0) {
        setError("No delimited files found in that folder.");
        return;
      }
      setFiles((f) => [...f, ...matched.filter((p) => !f.includes(p))]);
      invalidate();
    } catch (e) {
      setError(String(e));
    }
  };

  const runPreview = async () => {
    setError(null);
    try {
      setPreview(await api.previewAppend(inputs(), buildOptions()));
    } catch (e) {
      setError(String(e));
      setPreview(null);
    }
  };

  const run = async () => {
    setError(null);
    try {
      const started = await api.startAppend(inputs(), buildOptions());
      trackDerive(started.jobId, started.docId, "append");
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <Modal
      title="Append files"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
          <button
            onClick={() => void runPreview()}
            disabled={inputCount < 1 || running}
            className={btnGhost}
          >
            Preview
          </button>
          <button
            onClick={() => void run()}
            disabled={inputCount < 1 || running || !preview}
            title={!preview ? "Preview first" : undefined}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {running ? "Appending…" : "Append into a new document"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {/* Inputs */}
        <div className="text-xs">
          <p className="mb-1 text-zinc-500 dark:text-zinc-400">Open documents:</p>
          {tabs.length === 0 && <p className="text-zinc-400">No open tabs.</p>}
          <div className="flex flex-wrap gap-x-3 gap-y-1">
            {tabs.map((t) => (
              <label key={t.id} className="flex items-center gap-1">
                <input
                  type="checkbox"
                  checked={docIds.includes(t.id)}
                  onChange={(e) => {
                    setDocIds(
                      e.target.checked ? [...docIds, t.id] : docIds.filter((d) => d !== t.id),
                    );
                    invalidate();
                  }}
                  className="accent-violet-600"
                />
                {t.fileName}
              </label>
            ))}
          </div>
          <div className="mt-1.5 flex items-center gap-2">
            <button onClick={() => void addFiles()} className={chipBtn}>
              Add files…
            </button>
            <button onClick={() => void addFolder()} className={chipBtn}>
              Add folder…
            </button>
            {files.length > 0 && (
              <span className="text-zinc-400">
                {files.length} file{files.length === 1 ? "" : "s"} added
              </span>
            )}
          </div>
          {files.length > 0 && (
            <ul className="mt-1 max-h-20 space-y-0.5 overflow-y-auto">
              {files.map((f) => (
                <li key={f} className="flex items-center gap-2">
                  <span className="truncate font-mono text-[11px]">{f}</span>
                  <button
                    onClick={() => {
                      setFiles(files.filter((x) => x !== f));
                      invalidate();
                    }}
                    className="text-red-600 hover:underline dark:text-red-400"
                  >
                    remove
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        {/* Options */}
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <label className="flex items-center gap-1.5">
            Match columns
            <select
              value={align}
              onChange={(e) => {
                setAlign(e.target.value as AlignKey);
                invalidate();
              }}
              className={selectCls}
            >
              <option value="exactName">by exact name</option>
              <option value="caseInsensitiveName">by name (ignore case)</option>
              <option value="position">by position</option>
            </select>
          </label>
          <label className="flex items-center gap-1.5">
            Output columns
            <select
              value={schema}
              onChange={(e) => {
                setSchema(e.target.value as SchemaMode);
                invalidate();
              }}
              className={selectCls}
            >
              <option value="union">union of all</option>
              <option value="intersection">common to all</option>
              <option value="primary">first input only</option>
            </select>
          </label>
        </div>
        <div className="flex flex-wrap gap-x-4 gap-y-1.5 text-xs">
          <Check
            label="Add source file column"
            value={addSourceFile}
            onChange={(v) => {
              setAddSourceFile(v);
              invalidate();
            }}
          />
          <Check
            label="Add source row column"
            value={addSourceRow}
            onChange={(v) => {
              setAddSourceRow(v);
              invalidate();
            }}
          />
          <Check
            label="Allow duplicate headers"
            value={allowDuplicateHeaders}
            onChange={(v) => {
              setAllowDuplicateHeaders(v);
              invalidate();
            }}
          />
          <Check
            label="Continue past failing inputs"
            value={continueOnError}
            onChange={(v) => {
              setContinueOnError(v);
              invalidate();
            }}
          />
        </div>

        {(error ?? deriveError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? deriveError}</p>
        )}

        {running && derive && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              {derive.message ?? "working"} — {derive.processed}
              {derive.total != null && ` / ${derive.total}`} inputs
            </span>
            <button
              onClick={() => void cancelDerive()}
              className="rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
            >
              Cancel
            </button>
          </div>
        )}

        {/* Preview */}
        {preview && (
          <div className="space-y-1.5 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {preview.outputColumns.length} output column
              {preview.outputColumns.length === 1 ? "" : "s"} · {preview.rowsEstimated ? "≈" : ""}
              {preview.projectedRows.toLocaleString()} rows ·{" "}
              {preview.projectedIndexed ? "will open read-only (indexed)" : "opens editable"}
            </p>
            <p className="truncate font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
              {preview.outputColumns.join(" · ")}
            </p>
            <div className="max-h-32 space-y-0.5 overflow-y-auto">
              {preview.perInput.map((p, i) => (
                <div key={i} className="flex items-center gap-2">
                  <span className="max-w-[14rem] truncate">{p.name}</span>
                  <span className="text-zinc-400">
                    {p.mapped}/{preview.outputColumns.length} columns mapped
                  </span>
                  {p.missing.length > 0 && (
                    <span className="truncate text-amber-600 dark:text-amber-400">
                      blank: {p.missing.join(", ")}
                    </span>
                  )}
                  {p.warning && (
                    <span className="truncate text-amber-600 dark:text-amber-400">{p.warning}</span>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </Modal>
  );
}

function Check({
  label,
  value,
  onChange,
}: {
  label: string;
  value: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-1.5">
      <input
        type="checkbox"
        checked={value}
        onChange={(e) => onChange(e.target.checked)}
        className="accent-violet-600"
      />
      {label}
    </label>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700";
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
