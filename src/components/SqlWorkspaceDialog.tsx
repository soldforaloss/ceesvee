import { useMemo, useRef, useState } from "react";

import {
  SQL_PARAM_TYPES,
  allParamsValid,
  applySuggestion,
  buildSuggestions,
  currentToken,
  historyLabel,
  matchSuggestions,
  validateParamValue,
} from "../lib/sqlWorkspace";
import { useStore } from "../store/useStore";
import type { SqlParamType, SqlPlanNode } from "../types";
import { Modal } from "./Modal";

/**
 * Sandboxed SQL query workspace (F36). Composes open documents, approved local
 * files and one approved SQLite database into read-only SQL run through the F35
 * SafeQueryEngine. A plain textarea (no editor deps) with a prefix-matched
 * suggestion list from the bounded schema DTO, a typed `:param` editor,
 * validate / explain / run with progress + cancel, a bounded results grid, an
 * EXPLAIN QUERY PLAN tree, per-run limits, the persisted history ring, and
 * save / load / materialize / export actions.
 */
export function SqlWorkspaceDialog() {
  const ws = useStore((s) => s.sqlWorkspace);
  const tabs = useStore((s) => s.tabs);
  const project = useStore((s) => s.project);
  const derive = useStore((s) => s.derive);
  const close = useStore((s) => s.closeSqlWorkspace);

  const setText = useStore((s) => s.sqlSetText);
  const setParam = useStore((s) => s.sqlSetParam);
  const toggleDoc = useStore((s) => s.sqlToggleDocument);
  const addFile = useStore((s) => s.sqlAddFile);
  const removeFile = useStore((s) => s.sqlRemoveFile);
  const chooseDb = useStore((s) => s.sqlChooseDatabase);
  const clearDb = useStore((s) => s.sqlClearDatabase);
  const validate = useStore((s) => s.sqlValidate);
  const explain = useStore((s) => s.sqlExplain);
  const run = useStore((s) => s.sqlRun);
  const cancelRun = useStore((s) => s.sqlCancelRun);
  const setLimits = useStore((s) => s.sqlSetLimits);
  const setView = useStore((s) => s.sqlSetView);

  const textRef = useRef<HTMLTextAreaElement>(null);
  const [caret, setCaret] = useState(0);

  const suggestions = useMemo(() => buildSuggestions(ws.schema), [ws.schema]);
  const token = currentToken(ws.sql, caret);
  const matches = matchSuggestions(suggestions, token.token, 8);

  const materializing = derive?.kind === "sqlMaterialize";
  const paramsOk = allParamsValid(ws.params);
  const canRun = ws.sql.trim() !== "" && paramsOk && !ws.running;

  const accept = (text: string) => {
    const next = applySuggestion(ws.sql, caret, text);
    setText(next.text);
    // Restore focus and place the caret after the inserted identifier.
    requestAnimationFrame(() => {
      const el = textRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(next.caret, next.caret);
        setCaret(next.caret);
      }
    });
  };

  const onEditorKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Tab accepts the top suggestion (only while a fragment is being typed).
    if (e.key === "Tab" && matches.length > 0 && token.token !== "") {
      e.preventDefault();
      accept(matches[0].text);
      return;
    }
    // Ctrl/Cmd+Enter runs.
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && canRun) {
      e.preventDefault();
      void run();
    }
  };

  const loadedRows = ws.result ? ws.result.rows.length + ws.extraRows.length : 0;
  const moreAvailable = ws.result ? loadedRows < ws.result.rowCount : false;

  return (
    <Modal title="SQL workspace" onClose={close} size="2xl">
      <div className="flex min-h-[32rem] gap-3 text-sm">
        {/* ---- Sources + schema browser ---- */}
        <div className="flex w-64 shrink-0 flex-col gap-3 overflow-y-auto rounded border border-zinc-200 p-2 dark:border-zinc-800">
          <SchemaBrowser
            ws={ws}
            tabs={tabs}
            onToggleDoc={toggleDoc}
            onAddFile={() => void addFile()}
            onRemoveFile={(a) => void removeFile(a)}
            onChooseDb={() => void chooseDb()}
            onClearDb={clearDb}
            onInsert={accept}
          />
        </div>

        {/* ---- Editor + params + results ---- */}
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <textarea
            ref={textRef}
            value={ws.sql}
            spellCheck={false}
            placeholder="SELECT * FROM …   (only SELECT / WITH / VALUES / EXPLAIN)"
            onChange={(e) => {
              setText(e.target.value);
              setCaret(e.target.selectionStart);
            }}
            onKeyDown={onEditorKeyDown}
            onKeyUp={(e) => setCaret(e.currentTarget.selectionStart)}
            onClick={(e) => setCaret(e.currentTarget.selectionStart)}
            className="h-40 w-full resize-y rounded border border-zinc-300 bg-zinc-50 px-2 py-1.5 font-mono text-[13px] leading-relaxed outline-none focus:border-violet-500 dark:border-zinc-700 dark:bg-zinc-900"
          />

          {/* Suggestions */}
          {matches.length > 0 && (
            <div className="flex flex-wrap items-center gap-1">
              <span className="text-[11px] text-zinc-400">Suggestions:</span>
              {matches.map((m) => (
                <button
                  key={`${m.kind}:${m.text}`}
                  onClick={() => accept(m.text)}
                  title={m.detail}
                  className="rounded border border-zinc-300 bg-white px-1.5 py-0.5 font-mono text-[11px] text-zinc-700 hover:border-violet-400 hover:bg-violet-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200 dark:hover:bg-violet-500/10"
                >
                  {m.text}
                  <span className="ml-1 text-zinc-400">{m.kind === "table" ? "▦" : "·"}</span>
                </button>
              ))}
            </div>
          )}

          {/* Parameters */}
          {ws.params.length > 0 && (
            <div className="rounded border border-zinc-200 dark:border-zinc-800">
              <p className="border-b border-zinc-100 px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-zinc-400 dark:border-zinc-800">
                Parameters
              </p>
              <table className="w-full border-collapse text-[12px]">
                <tbody>
                  {ws.params.map((p) => {
                    const err = validateParamValue(p);
                    return (
                      <tr key={p.name} className="border-t border-zinc-100 dark:border-zinc-800">
                        <td className="px-2 py-1 font-mono text-violet-700 dark:text-violet-300">
                          :{p.name}
                        </td>
                        <td className="px-2 py-1">
                          <select
                            value={p.type}
                            onChange={(e) =>
                              setParam(p.name, { type: e.target.value as SqlParamType })
                            }
                            className="rounded border border-zinc-300 bg-transparent px-1 py-0.5 text-xs outline-none focus:border-violet-500 dark:border-zinc-700"
                          >
                            {SQL_PARAM_TYPES.map((t) => (
                              <option key={t} value={t} className="dark:bg-zinc-800">
                                {t}
                              </option>
                            ))}
                          </select>
                        </td>
                        <td className="px-2 py-1">
                          <input
                            value={p.type === "null" ? "" : (p.value ?? "")}
                            disabled={p.type === "null"}
                            placeholder={p.type === "null" ? "NULL" : "value"}
                            onChange={(e) => setParam(p.name, { value: e.target.value })}
                            className={`w-full rounded border bg-transparent px-1.5 py-0.5 font-mono text-xs outline-none focus:border-violet-500 disabled:opacity-40 dark:border-zinc-700 ${
                              err
                                ? "border-red-400 dark:border-red-500/60"
                                : "border-zinc-300 dark:border-zinc-700"
                            }`}
                          />
                        </td>
                        <td className="px-2 py-1 text-[11px] text-red-600 dark:text-red-400">
                          {err ?? ""}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}

          {/* Limits + actions */}
          <LimitsRow limits={ws.limits} onChange={setLimits} />

          <div className="flex flex-wrap items-center gap-2">
            <button onClick={() => void validate()} className={btnSecondary} disabled={ws.running}>
              Validate
            </button>
            <button onClick={() => void explain()} className={btnSecondary} disabled={ws.running}>
              Explain
            </button>
            {ws.running ? (
              <button onClick={() => void cancelRun()} className={btnDanger}>
                Cancel{ws.runProcessed > 0 ? ` (${ws.runProcessed.toLocaleString()} rows)` : "…"}
              </button>
            ) : (
              <button onClick={() => void run()} disabled={!canRun} className={btnPrimary}>
                Run
              </button>
            )}
            <div className="flex-1" />
            <HistoryMenu />
            <SavedMenu hasProject={project != null} />
          </div>

          {/* Notices */}
          {ws.notice && (
            <p className="rounded border border-emerald-300 bg-emerald-50 px-2 py-1 text-xs text-emerald-700 dark:border-emerald-500/40 dark:bg-emerald-950/40 dark:text-emerald-300">
              {ws.notice}
            </p>
          )}
          {ws.error && (
            <p className="rounded border border-red-300 bg-red-50 px-2 py-1 text-xs text-red-700 dark:border-red-500/40 dark:bg-red-950/40 dark:text-red-300">
              {ws.error}
            </p>
          )}

          {/* Results / plan */}
          <div className="flex min-h-0 flex-1 flex-col rounded border border-zinc-200 dark:border-zinc-800">
            <div className="flex shrink-0 items-center gap-1 border-b border-zinc-100 px-2 py-1 dark:border-zinc-800">
              <TabBtn active={ws.view === "results"} onClick={() => setView("results")}>
                Results
              </TabBtn>
              <TabBtn active={ws.view === "plan"} onClick={() => setView("plan")}>
                Plan
              </TabBtn>
              <div className="flex-1" />
              {ws.view === "results" && ws.result && (
                <span className="text-[11px] tabular-nums text-zinc-500">
                  {ws.result.rowCount.toLocaleString()} row
                  {ws.result.rowCount === 1 ? "" : "s"}
                  {ws.result.truncated ? " (capped)" : ""} · {ws.result.elapsedMs} ms ·{" "}
                  {formatBytes(ws.result.byteCount)}
                </span>
              )}
            </div>

            <div className="min-h-0 flex-1 overflow-auto p-2">
              {ws.view === "plan" ? (
                <PlanView plan={ws.plan} />
              ) : ws.validation && !ws.validation.ok ? (
                <p className="font-mono text-xs text-red-600 dark:text-red-400">
                  {ws.validation.error}
                </p>
              ) : ws.result ? (
                <ResultGrid
                  columns={ws.result.columns}
                  rows={[...ws.result.rows, ...ws.extraRows]}
                />
              ) : ws.validation?.ok ? (
                <p className="text-xs text-emerald-600 dark:text-emerald-400">
                  Valid. Output columns: {ws.validation.columns.join(", ") || "(none)"}
                </p>
              ) : (
                <p className="text-xs text-zinc-400">Run a query to see results here.</p>
              )}
            </div>

            {/* Result actions */}
            {ws.view === "results" && ws.result && (
              <div className="flex flex-wrap items-center gap-2 border-t border-zinc-100 px-2 py-1.5 dark:border-zinc-800">
                {moreAvailable && (
                  <button
                    onClick={() => void useStore.getState().sqlLoadMore()}
                    disabled={ws.loadingMore}
                    className={btnGhostSm}
                  >
                    {ws.loadingMore
                      ? "Loading…"
                      : `Load more (${loadedRows.toLocaleString()} / ${ws.result.rowCount.toLocaleString()})`}
                  </button>
                )}
                <div className="flex-1" />
                {materializing ? (
                  <span className="text-[11px] text-zinc-500">
                    Materializing… {derive?.processed.toLocaleString()}
                  </span>
                ) : (
                  <>
                    <button
                      onClick={() => void useStore.getState().sqlMaterialize(false)}
                      className={btnSecondary}
                      title="Create an editable document from the result"
                    >
                      To document
                    </button>
                    <button
                      onClick={() => void useStore.getState().sqlMaterialize(true)}
                      className={btnGhostSm}
                      title="Create an indexed (read-only) document from the result"
                    >
                      Indexed
                    </button>
                  </>
                )}
                <button
                  onClick={() => void useStore.getState().sqlExport()}
                  disabled={ws.exporting}
                  className={btnSecondary}
                >
                  {ws.exporting ? "Exporting…" : "Export CSV…"}
                </button>
              </div>
            )}
          </div>
        </div>
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Schema browser (left column)
// ---------------------------------------------------------------------------

function SchemaBrowser({
  ws,
  tabs,
  onToggleDoc,
  onAddFile,
  onRemoveFile,
  onChooseDb,
  onClearDb,
  onInsert,
}: {
  ws: ReturnType<typeof useStore.getState>["sqlWorkspace"];
  tabs: ReturnType<typeof useStore.getState>["tabs"];
  onToggleDoc: (id: number) => void;
  onAddFile: () => void;
  onRemoveFile: (alias: string) => void;
  onChooseDb: () => void;
  onClearDb: () => void;
  onInsert: (text: string) => void;
}) {
  return (
    <div className="space-y-3">
      {/* Documents */}
      <div>
        <SectionLabel>Open documents</SectionLabel>
        {tabs.length === 0 ? (
          <p className="px-1 text-[11px] text-zinc-400">No documents open.</p>
        ) : (
          <ul className="space-y-0.5">
            {tabs.map((t) => (
              <li key={t.id}>
                <label className="flex cursor-pointer items-center gap-1.5 rounded px-1 py-0.5 hover:bg-zinc-100 dark:hover:bg-zinc-800">
                  <input
                    type="checkbox"
                    checked={ws.documentIds.includes(t.id)}
                    onChange={() => onToggleDoc(t.id)}
                    className="accent-violet-600"
                  />
                  <span className="min-w-0 flex-1 truncate text-[12px]" title={t.fileName}>
                    {t.fileName}
                  </span>
                </label>
              </li>
            ))}
          </ul>
        )}
      </div>

      {/* Approved files */}
      <div>
        <div className="flex items-center gap-1">
          <SectionLabel>Approved files</SectionLabel>
          <div className="flex-1" />
          <button
            onClick={onAddFile}
            disabled={ws.addingFile}
            className="rounded px-1 text-[11px] text-violet-600 hover:bg-violet-50 disabled:opacity-40 dark:text-violet-300 dark:hover:bg-violet-500/10"
          >
            {ws.addingFile ? "Adding…" : "+ Add"}
          </button>
        </div>
        {ws.files.length === 0 ? (
          <p className="px-1 text-[11px] text-zinc-400">
            Add a CSV, JSON, Parquet or Arrow file to query it.
          </p>
        ) : (
          <ul className="space-y-0.5">
            {ws.files.map((f) => (
              <li
                key={f.alias}
                className="group flex items-center gap-1 rounded px-1 py-0.5 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              >
                <span
                  className="min-w-0 flex-1 truncate font-mono text-[12px]"
                  title={`${f.alias} — ${f.label} (${f.kind})`}
                >
                  {f.alias}
                </span>
                <span className="shrink-0 text-[10px] uppercase text-zinc-400">{f.kind}</span>
                <button
                  onClick={() => onRemoveFile(f.alias)}
                  title="Remove and revoke approval"
                  className="shrink-0 rounded px-1 text-zinc-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-500/10"
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>

      {/* Database */}
      <div>
        <div className="flex items-center gap-1">
          <SectionLabel>SQLite database</SectionLabel>
          <div className="flex-1" />
          {ws.database ? (
            <button
              onClick={onClearDb}
              className="rounded px-1 text-[11px] text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
            >
              Clear
            </button>
          ) : (
            <button
              onClick={onChooseDb}
              className="rounded px-1 text-[11px] text-violet-600 hover:bg-violet-50 dark:text-violet-300 dark:hover:bg-violet-500/10"
            >
              + Add
            </button>
          )}
        </div>
        {ws.database ? (
          <p
            className="truncate px-1 font-mono text-[11px] text-zinc-600 dark:text-zinc-300"
            title={ws.database}
          >
            {ws.database.split(/[\\/]/).pop()}
          </p>
        ) : (
          <p className="px-1 text-[11px] text-zinc-400">Optional — one approved database.</p>
        )}
      </div>

      {/* Schema (columns) */}
      <div>
        <SectionLabel>{ws.schemaLoading ? "Schema (loading…)" : "Schema"}</SectionLabel>
        {ws.schema ? (
          <div className="space-y-1">
            {[...ws.schema.documents, ...ws.schema.files, ...ws.schema.database].map((t) => (
              <details key={`${t.kind}:${t.alias}`} className="rounded">
                <summary className="cursor-pointer truncate px-1 py-0.5 font-mono text-[12px] text-zinc-700 hover:bg-zinc-100 dark:text-zinc-200 dark:hover:bg-zinc-800">
                  <button
                    onClick={(e) => {
                      e.preventDefault();
                      onInsert(t.alias);
                    }}
                    className="hover:text-violet-600 dark:hover:text-violet-300"
                  >
                    {t.alias}
                  </button>
                  <span className="ml-1 text-[10px] text-zinc-400">{t.columns.length}c</span>
                </summary>
                <ul className="ml-3 border-l border-zinc-200 pl-2 dark:border-zinc-800">
                  {t.columns.map((c) => (
                    <li key={c.name}>
                      <button
                        onClick={() => onInsert(c.name)}
                        className="flex w-full items-baseline gap-1 truncate px-1 py-0.5 text-left text-[11px] hover:bg-violet-50 dark:hover:bg-violet-500/10"
                        title={`${c.name} : ${c.declType}`}
                      >
                        <span className="truncate font-mono">{c.name}</span>
                        <span className="shrink-0 text-zinc-400">{c.declType}</span>
                      </button>
                    </li>
                  ))}
                  {t.columnsTruncated && (
                    <li className="px-1 text-[10px] text-zinc-400">…columns capped</li>
                  )}
                </ul>
              </details>
            ))}
            {ws.schema.documents.length === 0 &&
              ws.schema.files.length === 0 &&
              ws.schema.database.length === 0 && (
                <p className="px-1 text-[11px] text-zinc-400">Select a source to see its tables.</p>
              )}
          </div>
        ) : (
          <p className="px-1 text-[11px] text-zinc-400">Select a source to see its tables.</p>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

function LimitsRow({
  limits,
  onChange,
}: {
  limits: { maxRows?: number; maxBytes?: number; timeLimitMs?: number };
  onChange: (patch: { maxRows?: number; maxBytes?: number; timeLimitMs?: number }) => void;
}) {
  const num = (v: string): number | undefined => {
    const n = Number(v);
    return v.trim() === "" || !Number.isFinite(n) || n <= 0 ? undefined : n;
  };
  return (
    <div className="flex flex-wrap items-center gap-3 text-[11px] text-zinc-500">
      <span className="font-semibold uppercase tracking-wide text-zinc-400">Limits</span>
      <LimitInput
        label="rows"
        value={limits.maxRows}
        onChange={(v) => onChange({ maxRows: num(v) })}
      />
      <LimitInput
        label="MB"
        value={limits.maxBytes != null ? Math.round(limits.maxBytes / (1024 * 1024)) : undefined}
        onChange={(v) => {
          const n = num(v);
          onChange({ maxBytes: n != null ? n * 1024 * 1024 : undefined });
        }}
      />
      <LimitInput
        label="sec"
        value={limits.timeLimitMs != null ? Math.round(limits.timeLimitMs / 1000) : undefined}
        onChange={(v) => {
          const n = num(v);
          onChange({ timeLimitMs: n != null ? n * 1000 : undefined });
        }}
      />
    </div>
  );
}

function LimitInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value?: number;
  onChange: (v: string) => void;
}) {
  return (
    <label className="flex items-center gap-1">
      <input
        type="number"
        min={1}
        value={value ?? ""}
        placeholder="default"
        onChange={(e) => onChange(e.target.value)}
        className="w-20 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 tabular-nums outline-none focus:border-violet-500 dark:border-zinc-700"
      />
      {label}
    </label>
  );
}

// ---------------------------------------------------------------------------
// History + saved queries menus
// ---------------------------------------------------------------------------

function HistoryMenu() {
  const history = useStore((s) => s.sqlWorkspace.history);
  const load = useStore((s) => s.sqlLoadFromHistory);
  const clear = useStore((s) => s.sqlClearHistory);
  const [open, setOpen] = useState(false);
  return (
    <div className="relative">
      <button onClick={() => setOpen((v) => !v)} className={btnGhostSm}>
        History ({history.length})
      </button>
      {open && (
        <div className="absolute right-0 z-10 mt-1 max-h-72 w-96 overflow-auto rounded border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-zinc-900">
          {history.length === 0 ? (
            <p className="px-2 py-2 text-[11px] text-zinc-400">No history yet.</p>
          ) : (
            <>
              {history.map((h, i) => (
                <button
                  key={i}
                  onClick={() => {
                    load(h);
                    setOpen(false);
                  }}
                  className="flex w-full items-center gap-2 border-b border-zinc-100 px-2 py-1 text-left last:border-0 hover:bg-zinc-50 dark:border-zinc-800 dark:hover:bg-zinc-800"
                >
                  <StatusDot status={h.status} />
                  <span className="min-w-0 flex-1 truncate font-mono text-[11px]">
                    {historyLabel(h.sql)}
                  </span>
                  <span className="shrink-0 text-[10px] tabular-nums text-zinc-400">
                    {h.rowCount != null ? `${h.rowCount.toLocaleString()}r` : h.status}
                  </span>
                </button>
              ))}
              <button
                onClick={() => {
                  void clear();
                  setOpen(false);
                }}
                className="w-full px-2 py-1 text-left text-[11px] text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Clear history
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}

function SavedMenu({ hasProject }: { hasProject: boolean }) {
  const saved = useStore((s) => s.sqlWorkspace.saved);
  const save = useStore((s) => s.sqlSaveCurrent);
  const load = useStore((s) => s.sqlLoadSavedQuery);
  const del = useStore((s) => s.sqlDeleteSaved);
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  return (
    <div className="relative">
      <button onClick={() => setOpen((v) => !v)} className={btnGhostSm}>
        Saved ({saved.length})
      </button>
      {open && (
        <div className="absolute right-0 z-10 mt-1 w-80 rounded border border-zinc-200 bg-white p-2 shadow-lg dark:border-zinc-700 dark:bg-zinc-900">
          {hasProject ? (
            <div className="mb-2 flex items-center gap-1">
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="Save current as…"
                className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 text-xs outline-none focus:border-violet-500 dark:border-zinc-700"
              />
              <button
                onClick={() => {
                  if (name.trim()) {
                    void save(name);
                    setName("");
                  }
                }}
                disabled={name.trim() === ""}
                className={btnPrimarySm}
              >
                Save
              </button>
            </div>
          ) : (
            <p className="mb-2 text-[11px] text-zinc-400">Open a project to save queries.</p>
          )}
          {saved.length === 0 ? (
            <p className="text-[11px] text-zinc-400">No saved queries.</p>
          ) : (
            <ul className="max-h-56 overflow-auto">
              {saved.map((q) => (
                <li
                  key={q.id}
                  className="group flex items-center gap-1 rounded px-1 py-0.5 hover:bg-zinc-50 dark:hover:bg-zinc-800"
                >
                  <button
                    onClick={() => {
                      load(q);
                      setOpen(false);
                    }}
                    className="min-w-0 flex-1 truncate text-left text-[12px]"
                    title={q.sql}
                  >
                    {q.name}
                  </button>
                  <button
                    onClick={() => void del(q.id)}
                    title="Delete"
                    className="shrink-0 rounded px-1 text-zinc-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-500/10"
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Results grid + plan tree
// ---------------------------------------------------------------------------

function ResultGrid({ columns, rows }: { columns: string[]; rows: (string | null)[][] }) {
  if (columns.length === 0) return <p className="text-xs text-zinc-400">No columns.</p>;
  return (
    <table className="border-collapse text-[11px]">
      <thead className="sticky top-0 bg-white text-left text-zinc-400 dark:bg-zinc-900">
        <tr>
          <th className="border-b border-zinc-200 px-2 py-1 text-right font-normal text-zinc-300 dark:border-zinc-800">
            #
          </th>
          {columns.map((c, i) => (
            <th
              key={i}
              className="whitespace-nowrap border-b border-zinc-200 px-2 py-1 font-mono font-medium dark:border-zinc-800"
            >
              {c}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.map((row, ri) => (
          <tr key={ri} className="border-t border-zinc-100 dark:border-zinc-900">
            <td className="px-2 py-1 text-right tabular-nums text-zinc-300">{ri + 1}</td>
            {row.map((cell, ci) => (
              <td
                key={ci}
                className="max-w-72 truncate px-2 py-1 font-mono"
                title={cell ?? undefined}
              >
                {cell === null ? <span className="italic text-zinc-400">NULL</span> : cell}
              </td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function PlanView({ plan }: { plan: SqlPlanNode[] | null }) {
  if (plan == null) return <p className="text-xs text-zinc-400">Run Explain to see the plan.</p>;
  if (plan.length === 0)
    return <p className="text-xs text-zinc-400">The plan is empty (trivial statement).</p>;
  return (
    <ul className="font-mono text-[11px]">
      {plan.map((n) => (
        <PlanNode key={n.id} node={n} depth={0} />
      ))}
    </ul>
  );
}

function PlanNode({ node, depth }: { node: SqlPlanNode; depth: number }) {
  return (
    <li>
      <div className="flex gap-1 py-0.5" style={{ paddingLeft: `${depth * 14}px` }}>
        <span className="text-zinc-400">{depth > 0 ? "└" : "•"}</span>
        <span className="text-zinc-700 dark:text-zinc-200">{node.detail}</span>
      </div>
      {node.children.map((c) => (
        <PlanNode key={c.id} node={c} depth={depth + 1} />
      ))}
    </li>
  );
}

// ---------------------------------------------------------------------------
// Small shared bits
// ---------------------------------------------------------------------------

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <p className="px-1 text-[10px] font-semibold uppercase tracking-wide text-zinc-400">
      {children}
    </p>
  );
}

function TabBtn({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`rounded px-2 py-0.5 text-xs ${
        active
          ? "bg-violet-600 text-white"
          : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
      }`}
    >
      {children}
    </button>
  );
}

function StatusDot({ status }: { status: string }) {
  const color =
    status === "done" ? "bg-emerald-500" : status === "cancelled" ? "bg-amber-500" : "bg-red-500";
  return <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${color}`} title={status} />;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

const btnPrimary =
  "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40";
const btnPrimarySm =
  "rounded bg-violet-600 px-2 py-1 text-xs text-white hover:bg-violet-500 disabled:opacity-40";
const btnSecondary =
  "rounded border border-zinc-300 px-3 py-1.5 text-sm text-zinc-700 hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800";
const btnGhostSm =
  "rounded px-2 py-1 text-xs text-zinc-600 hover:bg-zinc-100 disabled:opacity-40 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnDanger =
  "rounded bg-red-600 px-3 py-1.5 text-sm text-white hover:bg-red-500 disabled:opacity-40";
