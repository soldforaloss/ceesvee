import { useEffect } from "react";

import { useStore } from "../store/useStore";
import type { DbObjectInfo } from "../types";
import { Modal } from "./Modal";

/**
 * Local database browser (F35). Self-driven by the `dbBrowser` store slice, so
 * "Open database…" opens it automatically. Lists the SQLite file's tables and
 * views with their columns, keys, indexes and row estimates, previews bounded
 * rows, and offers — per object — an indexed READ-ONLY open (the table is never
 * copied into memory) or a memory-bounded editable import.
 *
 * SQLite databases ONLY this cycle: DuckDB is deliberately out of scope (its
 * bundled C++ library cannot build on the low-memory dev machine).
 */
export function DatabaseDialog() {
  const st = useStore((s) => s.dbBrowser);
  const indexing = useStore((s) => s.indexing);
  const close = useStore((s) => s.closeDatabaseDialog);
  const select = useStore((s) => s.selectDbObject);
  const probe = useStore((s) => s.probeDbRefresh);
  const reload = useStore((s) => s.reloadDbSchema);
  const openTable = useStore((s) => s.openDbTable);
  const importTable = useStore((s) => s.importDbTable);
  const dismissForce = useStore((s) => s.dismissDbForceImport);
  const cancelIndexing = useStore((s) => s.cancelIndexing);

  const sessionId = st?.sessionId;
  // Probe for external changes once when the browser opens (and on session
  // change), so a database edited elsewhere surfaces a reload prompt.
  useEffect(() => {
    if (sessionId != null) void probe();
  }, [sessionId, probe]);

  if (!st) return null;

  const tables = st.schema.objects.filter((o) => o.kind === "table");
  const views = st.schema.objects.filter((o) => o.kind === "view");
  const selected = st.schema.objects.find((o) => o.name === st.selected) ?? null;

  const dbBusy =
    (indexing?.kind === "dbOpenTable" || indexing?.kind === "dbImportTable") &&
    indexing.path === st.path;

  return (
    <Modal title={`Database — ${st.fileName}`} onClose={() => void close()} size="2xl">
      <div className="space-y-3 text-sm">
        {/* External-change banner */}
        {st.refresh && (
          <div className="flex items-center gap-3 rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-500/40 dark:bg-amber-950/40 dark:text-amber-300">
            <span className="flex-1">
              The database changed outside CEESVEE
              {st.refresh.rowsChanged && st.refresh.schemaChanged
                ? " (rows and schema)"
                : st.refresh.schemaChanged
                  ? " (schema)"
                  : " (rows)"}
              . Reload the schema to see the current state.
            </span>
            <button onClick={() => void reload()} className={btnPrimarySm} disabled={st.busy}>
              {st.busy ? "Reloading…" : "Reload"}
            </button>
          </div>
        )}

        <div className="flex min-h-[26rem] gap-3">
          {/* Schema tree */}
          <div className="w-64 shrink-0 overflow-y-auto rounded border border-zinc-200 dark:border-zinc-800">
            <TreeSection label="Tables" empty="No tables">
              {tables.map((o) => (
                <TreeItem
                  key={o.name}
                  obj={o}
                  active={o.name === st.selected}
                  onClick={() => void select(o.name)}
                />
              ))}
            </TreeSection>
            {views.length > 0 && (
              <TreeSection label="Views" empty="">
                {views.map((o) => (
                  <TreeItem
                    key={o.name}
                    obj={o}
                    active={o.name === st.selected}
                    onClick={() => void select(o.name)}
                  />
                ))}
              </TreeSection>
            )}
            {st.schema.objects.length === 0 && (
              <p className="px-3 py-2 text-xs text-zinc-500">
                This database has no tables or views.
              </p>
            )}
          </div>

          {/* Detail + preview */}
          <div className="min-w-0 flex-1 space-y-3 overflow-y-auto">
            {selected ? (
              <>
                <ObjectDetail obj={selected} />

                {/* Preview */}
                <Section label="Preview">
                  {st.previewLoading ? (
                    <p className="text-xs text-zinc-500">Loading preview…</p>
                  ) : st.previewError ? (
                    <p className="text-xs text-red-600 dark:text-red-400">{st.previewError}</p>
                  ) : st.preview ? (
                    <PreviewTable columns={st.preview.columns} rows={st.preview.rows} />
                  ) : (
                    <p className="text-xs text-zinc-500">No preview.</p>
                  )}
                  {st.preview?.truncated && (
                    <p className="mt-1 text-[11px] text-zinc-500">
                      Showing the first {st.preview.rows.length.toLocaleString()} rows.
                    </p>
                  )}
                </Section>

                {/* Force-import prompt (memory bound tripped) */}
                {st.forceImport && st.forceImport.object === selected.name && (
                  <div className="rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-500/40 dark:bg-amber-950/40 dark:text-amber-300">
                    <p className="mb-1.5">{st.forceImport.message}</p>
                    <div className="flex gap-2">
                      <button
                        onClick={() => void importTable(selected.name, true)}
                        className={btnPrimarySm}
                      >
                        Import anyway
                      </button>
                      <button onClick={dismissForce} className={btnGhostSm}>
                        Cancel
                      </button>
                    </div>
                  </div>
                )}

                {/* Per-object actions */}
                <div className="flex flex-wrap items-center gap-2">
                  <button
                    onClick={() => void openTable(selected.name)}
                    disabled={dbBusy}
                    className={btnPrimary}
                    title="Open as an indexed, read-only document (never copied into memory)"
                  >
                    Open read-only
                  </button>
                  <button
                    onClick={() => void importTable(selected.name, false)}
                    disabled={dbBusy}
                    className={btnSecondary}
                    title="Copy into a fully editable document (bounded by a memory check)"
                  >
                    Import editable
                  </button>
                </div>

                {st.actionError && (
                  <p className="text-xs text-red-600 dark:text-red-400">{st.actionError}</p>
                )}
              </>
            ) : (
              <p className="text-xs text-zinc-500">Select a table or view.</p>
            )}
          </div>
        </div>

        {/* Progress / footer */}
        <div className="flex items-center gap-3 border-t border-zinc-200 pt-2 text-xs dark:border-zinc-800">
          {dbBusy && indexing ? (
            <>
              <span className="text-zinc-500 dark:text-zinc-400">
                {indexing.kind === "dbImportTable" ? "Importing" : "Opening"}{" "}
                {indexing.dbObject ?? ""}… {indexing.processed.toLocaleString()}
                {indexing.total ? ` / ${indexing.total.toLocaleString()}` : ""} rows
              </span>
              <button onClick={() => void cancelIndexing()} className={cancelBtn}>
                Cancel
              </button>
            </>
          ) : (
            <button onClick={() => void probe()} className={btnGhostSm}>
              Check for changes
            </button>
          )}
          <button
            onClick={() => void close()}
            className="ml-auto rounded px-3 py-1.5 text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            Close
          </button>
        </div>
      </div>
    </Modal>
  );
}

function TreeSection({
  label,
  empty,
  children,
}: {
  label: string;
  empty: string;
  children: React.ReactNode;
}) {
  const items = Array.isArray(children) ? children : [children];
  return (
    <div>
      <p className="sticky top-0 bg-zinc-50 px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
        {label}
      </p>
      {items.length === 0 && empty && <p className="px-2 py-1 text-xs text-zinc-500">{empty}</p>}
      {children}
    </div>
  );
}

function TreeItem({
  obj,
  active,
  onClick,
}: {
  obj: DbObjectInfo;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex w-full items-center gap-1.5 px-2 py-1 text-left ${
        active ? "bg-violet-600 text-white" : "hover:bg-zinc-100 dark:hover:bg-zinc-800"
      }`}
    >
      <span className="min-w-0 flex-1 truncate font-mono text-[12px]" title={obj.name}>
        {obj.name}
      </span>
      <span
        className={`shrink-0 text-[10px] tabular-nums ${active ? "text-violet-100" : "text-zinc-400"}`}
      >
        {obj.rowEstimateExact ? "" : "~"}
        {obj.rowEstimate.toLocaleString()}
      </span>
    </button>
  );
}

function ObjectDetail({ obj }: { obj: DbObjectInfo }) {
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="font-mono text-sm font-medium">{obj.name}</span>
        <Badge>{obj.kind}</Badge>
        {obj.withoutRowid && <Badge>WITHOUT ROWID</Badge>}
        {obj.primaryKey.length > 0 && <Badge tone="pk">PK: {obj.primaryKey.join(", ")}</Badge>}
        <span className="text-xs text-zinc-500">
          {obj.rowEstimateExact ? "" : "≈ "}
          {obj.rowEstimate.toLocaleString()} row{obj.rowEstimate === 1 ? "" : "s"}
        </span>
      </div>

      {/* Columns */}
      <div className="overflow-x-auto rounded border border-zinc-200 dark:border-zinc-800">
        <table className="w-full border-collapse text-[11px]">
          <thead className="bg-zinc-50 text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
            <tr>
              <th className="px-2 py-1 font-medium">Column</th>
              <th className="px-2 py-1 font-medium">Type</th>
              <th className="px-2 py-1 font-medium">Flags</th>
              <th className="px-2 py-1 font-medium">Default</th>
            </tr>
          </thead>
          <tbody>
            {obj.columns.map((c) => (
              <tr key={c.name} className="border-t border-zinc-100 dark:border-zinc-800">
                <td className="px-2 py-1 font-mono">{c.name}</td>
                <td className="px-2 py-1 text-zinc-500">{c.declType || "—"}</td>
                <td className="px-2 py-1">
                  <span className="flex flex-wrap gap-1">
                    {c.pkPosition != null && (
                      <Badge tone="pk">PK{c.pkPosition > 1 ? ` ${c.pkPosition}` : ""}</Badge>
                    )}
                    {c.notnull && <Badge>NOT NULL</Badge>}
                  </span>
                </td>
                <td className="px-2 py-1 font-mono text-zinc-500">{c.defaultValue ?? ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Indexes */}
      {obj.indexes.length > 0 && (
        <Section label="Indexes">
          <ul className="space-y-0.5 text-[11px]">
            {obj.indexes.map((idx) => (
              <li key={idx.name} className="flex flex-wrap items-center gap-1.5">
                <span className="font-mono">{idx.name}</span>
                {idx.unique && <Badge>unique</Badge>}
                {idx.partial && <Badge>partial</Badge>}
                <span className="text-zinc-500">
                  ({idx.columns.map((c) => c ?? "‹expr›").join(", ")})
                </span>
              </li>
            ))}
          </ul>
        </Section>
      )}

      {/* Foreign keys */}
      {obj.foreignKeys.length > 0 && (
        <Section label="Foreign keys">
          <ul className="space-y-0.5 text-[11px]">
            {obj.foreignKeys.map((fk, i) => (
              <li key={i} className="font-mono text-zinc-600 dark:text-zinc-300">
                ({fk.columns.join(", ")}) → {fk.table}
                {fk.refColumns.some((c) => c != null) &&
                  ` (${fk.refColumns.map((c) => c ?? "?").join(", ")})`}
              </li>
            ))}
          </ul>
        </Section>
      )}
    </div>
  );
}

function PreviewTable({ columns, rows }: { columns: string[]; rows: (string | null)[][] }) {
  return (
    <div className="max-h-56 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
      <table className="border-collapse text-[11px]">
        <thead className="sticky top-0 bg-white text-left text-zinc-400 dark:bg-zinc-900">
          <tr>
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
              {row.map((cell, ci) => (
                <td
                  key={ci}
                  className="max-w-64 truncate px-2 py-1 font-mono"
                  title={cell ?? undefined}
                >
                  {cell === null ? <span className="italic text-zinc-400">NULL</span> : cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1">
      <p className="text-xs font-medium text-zinc-600 dark:text-zinc-300">{label}</p>
      {children}
    </div>
  );
}

function Badge({ children, tone }: { children: React.ReactNode; tone?: "pk" }) {
  const cls =
    tone === "pk"
      ? "bg-amber-100 text-amber-700 dark:bg-amber-500/20 dark:text-amber-300"
      : "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300";
  return <span className={`rounded px-1.5 py-0.5 text-[10px] font-medium ${cls}`}>{children}</span>;
}

const btnPrimary =
  "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40";
const btnPrimarySm =
  "rounded bg-violet-600 px-2 py-1 text-xs text-white hover:bg-violet-500 disabled:opacity-40";
const btnSecondary =
  "rounded border border-zinc-300 px-3 py-1.5 text-sm text-zinc-700 hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800";
const btnGhostSm =
  "rounded px-2 py-1 text-xs text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const cancelBtn =
  "rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10";
