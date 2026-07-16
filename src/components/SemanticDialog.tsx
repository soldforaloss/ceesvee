import { useEffect, useState } from "react";

import * as api from "../lib/tauri";
import { matchingProfiles } from "../lib/profiles";
import {
  ACTION_LABELS,
  SEMANTIC_LABELS,
  actionsForType,
  applyOverrides,
  isFilterable,
  upsertOverride,
} from "../lib/semantics";
import { useActiveMeta, useStore } from "../store/useStore";
import type { SemanticAction, SemanticActionPreview, SemanticType } from "../types";
import { Modal } from "./Modal";

/**
 * Semantic data-type detection (F26): recognise emails, URLs, UUIDs, IPs,
 * percentages, currencies, phones, postal codes, JSON and categorical
 * columns. Detection never mutates; every quick action shows an exact
 * preview first and applies as one undo step. Overrides persist into a
 * matching file profile (keyed by column name) so they survive rescans.
 */
export function SemanticDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const semantic = useStore((s) => s.semantic);
  const settings = useStore((s) => s.settings);
  const startScan = useStore((s) => s.startSemanticScan);
  const cancelScan = useStore((s) => s.cancelSemanticScan);
  const loadCached = useStore((s) => s.loadCachedSemanticReport);
  const applyFilter = useStore((s) => s.applySemanticFilter);
  const applyAction = useStore((s) => s.applySemanticAction);
  const saveProfiles = useStore((s) => s.saveProfiles);

  const profile = meta?.path ? matchingProfiles(settings?.profiles ?? [], meta.path)[0] : undefined;

  const [overrides, setOverrides] = useState<[string, SemanticType][]>(
    () => profile?.semanticTypes ?? [],
  );
  const [preview, setPreview] = useState<{
    column: number;
    semantic: SemanticType;
    action: SemanticAction;
    data: SemanticActionPreview;
  } | null>(null);
  const [working, setWorking] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);

  // Adopt a report computed earlier (it survives in the backend cache).
  useEffect(() => {
    void loadCached();
  }, [loadCached]);

  const { report, scanJobId, processed, total, error } = semantic;
  const scanning = scanJobId != null;

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";
  const stale = report !== null && report.revision !== meta.revision;
  const columns = report ? applyOverrides(report.columns, meta.headers, overrides) : [];

  const setOverride = (columnName: string, value: SemanticType | null) => {
    const next = upsertOverride(overrides, columnName, value);
    setOverrides(next);
    if (profile) {
      const profiles = (settings?.profiles ?? []).map((p) =>
        p.id === profile.id ? { ...p, semanticTypes: next } : p,
      );
      void saveProfiles(profiles);
    }
  };

  const runPreview = async (column: number, sem: SemanticType, action: SemanticAction) => {
    setPreviewError(null);
    try {
      const data = await api.previewSemanticAction(meta.id, column, sem, action, meta.revision);
      setPreview({ column, semantic: sem, action, data });
    } catch (e) {
      setPreviewError(String(e));
    }
  };

  const confirmPreview = async () => {
    if (!preview) return;
    setWorking(true);
    const ok = await applyAction(preview.column, preview.semantic, preview.action, meta.revision);
    setWorking(false);
    if (ok) setPreview(null);
  };

  const runFilter = async (column: number, sem: SemanticType, keepValid: boolean) => {
    if (!report) return;
    setWorking(true);
    const ok = await applyFilter(column, sem, keepValid, report.revision);
    setWorking(false);
    if (ok) onClose();
  };

  return (
    <Modal
      title="Semantic types"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-xs text-zinc-400">
            {profile
              ? `Overrides persist in profile "${profile.name}"`
              : "Save a file profile to persist overrides"}
          </span>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex items-center gap-3">
          <button
            onClick={() => void startScan()}
            disabled={scanning}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {scanning ? "Scanning…" : report ? "Rescan columns" : "Detect column types"}
          </button>
          {scanning && (
            <>
              <span className="text-xs text-zinc-500 dark:text-zinc-400">
                {processed.toLocaleString()}
                {total != null && ` / ${total.toLocaleString()}`} rows
              </span>
              <button
                onClick={() => void cancelScan()}
                className="rounded px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Cancel
              </button>
            </>
          )}
          {report && !scanning && (
            <span className="text-xs text-zinc-500 dark:text-zinc-400">
              Scanned {report.scannedRows.toLocaleString()} rows · badge at ≥
              {Math.round(report.threshold * 100)}% matching
            </span>
          )}
        </div>

        {report?.sampled && (
          <p className="rounded bg-sky-50 px-2 py-1.5 text-xs text-sky-700 dark:bg-sky-500/10 dark:text-sky-300">
            Sampled: only the first {report.scannedRows.toLocaleString()} rows of this indexed
            document were scanned — treat these results as evidence, not certainty.
          </p>
        )}
        {stale && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            The document changed since this scan — actions are disabled. Rescan to refresh.
          </p>
        )}
        {(error ?? previewError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? previewError}</p>
        )}

        {report && columns.length > 0 && (
          <div className="max-h-[46vh] space-y-1.5 overflow-y-auto pr-1">
            {columns.map((c) => {
              const name = meta.headers[c.column] || `Column ${c.column + 1}`;
              const shown = c.effective;
              const belowThreshold =
                !c.overridden && c.detected === null && c.bestCandidate !== null;
              const actions = shown ? actionsForType(shown) : [];
              const filterable = shown !== null && isFilterable(shown);
              return (
                <div
                  key={c.column}
                  className="rounded border border-zinc-200 p-2 dark:border-zinc-800"
                >
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    <span className="max-w-[16rem] truncate font-medium" title={name}>
                      {name}
                    </span>
                    {shown ? (
                      <span
                        className={`rounded px-1.5 py-0.5 text-[11px] ${
                          c.overridden
                            ? "bg-sky-100 text-sky-700 dark:bg-sky-500/15 dark:text-sky-300"
                            : "bg-violet-100 text-violet-700 dark:bg-violet-500/15 dark:text-violet-300"
                        }`}
                      >
                        {SEMANTIC_LABELS[shown]}
                        {c.overridden && " (override)"}
                      </span>
                    ) : (
                      <span className="rounded bg-zinc-100 px-1.5 py-0.5 text-[11px] text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
                        Text
                      </span>
                    )}
                    {c.nonBlank > 0 && !c.overridden && c.bestCandidate && (
                      <span className="text-zinc-400">
                        {belowThreshold && `${SEMANTIC_LABELS[c.bestCandidate]} `}
                        {Math.round(c.confidence * 100)}% · {c.matching.toLocaleString()} match
                        {c.conflicting > 0 && ` · ${c.conflicting.toLocaleString()} conflict`}
                        {belowThreshold && " — below badge threshold"}
                      </span>
                    )}
                    <span className="flex-1" />
                    <label className="flex items-center gap-1.5 text-zinc-500 dark:text-zinc-400">
                      Override
                      <select
                        value={c.overridden && shown ? shown : ""}
                        onChange={(e) =>
                          setOverride(
                            meta.headers[c.column] ?? "",
                            e.target.value === "" ? null : (e.target.value as SemanticType),
                          )
                        }
                        disabled={!meta.headers[c.column]}
                        title={
                          meta.headers[c.column]
                            ? undefined
                            : "Overrides need a named header column"
                        }
                        className={selectCls}
                      >
                        <option value="" className="dark:bg-zinc-800">
                          detected
                        </option>
                        {Object.entries(SEMANTIC_LABELS).map(([value, label]) => (
                          <option key={value} value={value} className="dark:bg-zinc-800">
                            {label}
                          </option>
                        ))}
                      </select>
                    </label>
                  </div>

                  {shown && (filterable || actions.length > 0) && (
                    <div className="mt-1.5 flex flex-wrap gap-1.5">
                      {filterable && (
                        <>
                          <button
                            onClick={() => void runFilter(c.column, shown, true)}
                            disabled={working || stale}
                            className={chipBtn}
                          >
                            Keep valid rows
                          </button>
                          <button
                            onClick={() => void runFilter(c.column, shown, false)}
                            disabled={working || stale}
                            className={chipBtn}
                          >
                            Keep invalid rows
                          </button>
                        </>
                      )}
                      {actions.map((a) => (
                        <button
                          key={a}
                          onClick={() => void runPreview(c.column, shown, a)}
                          disabled={working || stale || readOnly}
                          title={readOnly ? "Read-only (indexed) document" : undefined}
                          className={chipBtn}
                        >
                          {ACTION_LABELS[a]}…
                        </button>
                      ))}
                    </div>
                  )}

                  {preview && preview.column === c.column && (
                    <div className="mt-2 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
                      <p className="font-medium">
                        {ACTION_LABELS[preview.action]}
                        {preview.data.newColumn
                          ? ` → new column "${preview.data.newColumn}" (${preview.data.affected.toLocaleString()} values)`
                          : ` — ${preview.data.affected.toLocaleString()} cell${
                              preview.data.affected === 1 ? "" : "s"
                            } would change`}
                      </p>
                      {preview.data.examples.length > 0 && (
                        <ul className="mt-1 space-y-0.5 font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                          {preview.data.examples.slice(0, 5).map(([before, after], i) => (
                            <li key={i} className="truncate">
                              {before} → {after}
                            </li>
                          ))}
                        </ul>
                      )}
                      <div className="mt-1.5 flex gap-2">
                        <button
                          onClick={() => void confirmPreview()}
                          disabled={working || preview.data.affected === 0}
                          className="rounded bg-violet-600 px-2 py-1 text-white hover:bg-violet-500 disabled:opacity-40"
                        >
                          {working ? "Applying…" : "Apply (one undo step)"}
                        </button>
                        <button onClick={() => setPreview(null)} className={btnGhost}>
                          Cancel
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}

        {!report && !scanning && (
          <p className="py-6 text-center text-xs text-zinc-400">
            Detect real-world types — emails, URLs, UUIDs, IPs, percentages, currencies, phone
            numbers, postal codes, JSON, categorical values. Detection is read-only.
          </p>
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
