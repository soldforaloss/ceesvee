import { useMemo, useState } from "react";

import {
  entryMatchesQuery,
  entryPassesKind,
  matchStatusLabel,
  noteTimeLabel,
  normalizeTagName,
  predicateForKind,
  sortEntries,
  tagColor,
  type AnnotationFilterKind,
} from "../lib/annotations";
import { useActiveMeta, useStore } from "../store/useStore";
import type { RowAnnotationView } from "../types";
import { Close } from "./Icons";

/** Tiny inline glyphs so the panel echoes the grid gutter indicators. */
const StarGlyph = ({ on }: { on: boolean }) => (
  <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" aria-hidden>
    <path
      d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77 5.82 21l1.18-6.88-5-4.87 6.91-1.01L12 2z"
      fill={on ? "#f59e0b" : "none"}
      stroke="#f59e0b"
      strokeWidth="1.5"
      strokeLinejoin="round"
    />
  </svg>
);

const FlagGlyph = ({ on }: { on: boolean }) => (
  <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" aria-hidden>
    <path
      d="M5 21V4h11l-1.5 4L16 12H5"
      fill={on ? "#f43f5e" : "none"}
      stroke="#f43f5e"
      strokeWidth="1.5"
      strokeLinejoin="round"
    />
  </svg>
);

const FILTERS: { kind: AnnotationFilterKind; label: string }[] = [
  { kind: "all", label: "All" },
  { kind: "starred", label: "Starred" },
  { kind: "flagged", label: "Flagged" },
  { kind: "tagged", label: "Tagged" },
  { kind: "hasNote", label: "Notes" },
  { kind: "hasCellNote", label: "Cell notes" },
  { kind: "review", label: "Review" },
];

/**
 * The annotations panel (F40): every bookmark / tag / note for the active
 * document, with type filters, search, jump-to-row, a review section for
 * ambiguous / orphaned annotations, the tag namespace (with tag-to-column and
 * remove), and the anchoring / author settings. Filtering the panel list is
 * client-side; "Filter grid" pushes the state predicate into the row filter.
 */
export function AnnotationsPanel() {
  const meta = useActiveMeta();
  const view = useStore((s) => s.annotationsView);
  const setOpen = useStore((s) => s.setAnnotationsPanelOpen);
  const jumpToCell = useStore((s) => s.jumpToCell);
  const removeAnnotation = useStore((s) => s.removeAnnotation);
  const discardOrphans = useStore((s) => s.discardAnnotationOrphans);
  const applyFilter = useStore((s) => s.applyAnnotationFilter);
  const exportToFile = useStore((s) => s.exportAnnotationsToFile);
  const defineTag = useStore((s) => s.defineAnnotationTag);
  const removeTag = useStore((s) => s.removeAnnotationTag);
  const openTagToColumn = useStore((s) => s.openTagToColumn);
  const openRowNote = useStore((s) => s.openRowNoteEditor);
  const setAuthor = useStore((s) => s.setAnnotationAuthor);
  const setKeySpec = useStore((s) => s.setAnnotationKeySpec);

  const [kind, setKind] = useState<AnnotationFilterKind>("all");
  const [query, setQuery] = useState("");
  const [newTag, setNewTag] = useState("");
  const [showTags, setShowTags] = useState(true);
  const [showSettings, setShowSettings] = useState(false);

  const filtered = useMemo(() => {
    if (!view) return [] as RowAnnotationView[];
    return sortEntries(
      view.entries.filter((e) => entryPassesKind(e, kind) && entryMatchesQuery(e, query)),
    );
  }, [view, kind, query]);

  if (!meta) return null;

  const reviewCount = (view?.ambiguous ?? 0) + (view?.orphaned ?? 0);
  const headerFor = (columnId: string): string => {
    const phys = meta.columnIds.indexOf(columnId);
    return phys >= 0 ? meta.headers[phys] || `Column ${phys + 1}` : columnId;
  };

  const jumpTo = (record: number) => void jumpToCell(record, 0);

  return (
    <aside className="flex w-96 shrink-0 flex-col border-l border-zinc-200 bg-white text-sm dark:border-zinc-800 dark:bg-zinc-900">
      <div className="flex items-center gap-2 border-b border-zinc-200 px-3 py-2 dark:border-zinc-800">
        <span className="font-medium">Annotations</span>
        {view && (
          <span className="text-xs text-zinc-400">
            {view.matched} matched
            {reviewCount > 0 && (
              <span className="text-amber-600 dark:text-amber-400"> · {reviewCount} review</span>
            )}
          </span>
        )}
        <span className="flex-1" />
        <button
          onClick={() => void exportToFile("json")}
          title="Export annotations as JSON"
          className="rounded px-1.5 py-0.5 text-xs text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          Export JSON
        </button>
        <button
          onClick={() => void exportToFile("csv")}
          title="Export annotations as CSV"
          className="rounded px-1.5 py-0.5 text-xs text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          CSV
        </button>
        <button
          onClick={() => setOpen(false)}
          className="rounded p-0.5 text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          <Close className="h-4 w-4" />
        </button>
      </div>

      {/* filter chips + search */}
      <div className="space-y-2 border-b border-zinc-200 px-3 py-2 dark:border-zinc-800">
        <div className="flex flex-wrap gap-1">
          {FILTERS.map((f) => {
            const count =
              f.kind === "all"
                ? (view?.entries.length ?? 0)
                : (view?.entries.filter((e) => entryPassesKind(e, f.kind)).length ?? 0);
            return (
              <button
                key={f.kind}
                onClick={() => setKind(f.kind)}
                className={`rounded-full px-2 py-0.5 text-xs ${
                  kind === f.kind
                    ? "bg-violet-600 text-white"
                    : "bg-zinc-100 text-zinc-600 hover:bg-zinc-200 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
                }`}
              >
                {f.label}
                {count > 0 && <span className="ml-1 opacity-70">{count}</span>}
              </button>
            );
          })}
        </div>
        <div className="flex items-center gap-1.5">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search tags & notes…"
            className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-xs outline-none focus:border-violet-400 dark:border-zinc-700"
          />
          {predicateForKind(kind) && (
            <button
              onClick={() => {
                const p = predicateForKind(kind);
                if (p) void applyFilter(p);
              }}
              title="Filter the grid to these rows"
              className="shrink-0 rounded border border-zinc-200 px-1.5 py-1 text-xs hover:border-violet-400 dark:border-zinc-700"
            >
              Filter grid
            </button>
          )}
        </div>
      </div>

      {/* review banner */}
      {reviewCount > 0 && (
        <div className="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-3 py-1.5 text-xs text-amber-800 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-300">
          <span className="flex-1">
            {view?.ambiguous ?? 0} ambiguous, {view?.orphaned ?? 0} orphaned — resolve below.
          </span>
          {(view?.orphaned ?? 0) > 0 && (
            <button
              onClick={() => void discardOrphans()}
              className="rounded border border-amber-300 px-1.5 py-0.5 hover:bg-amber-100 dark:border-amber-500/40 dark:hover:bg-amber-500/20"
            >
              Discard orphans
            </button>
          )}
        </div>
      )}

      {/* tag namespace */}
      {view && (
        <div className="border-b border-zinc-200 dark:border-zinc-800">
          <button
            onClick={() => setShowTags((v) => !v)}
            className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs font-medium text-zinc-500 hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
          >
            <span>Tags</span>
            <span className="text-zinc-400">{view.tags.length}</span>
            <span className="flex-1" />
            <span className="text-zinc-400">{showTags ? "▾" : "▸"}</span>
          </button>
          {showTags && (
            <div className="space-y-1 px-3 pb-2">
              {view.tags.length === 0 && (
                <p className="py-1 text-xs text-zinc-400">
                  No tags yet — tag rows from the grid’s right-click menu.
                </p>
              )}
              {view.tags.map((t) => (
                <div key={t.name} className="flex items-center gap-1.5 text-xs">
                  <span
                    className="h-2.5 w-2.5 shrink-0 rounded-full"
                    style={{ background: tagColor(t.name, t.color) }}
                  />
                  <span className="truncate" title={t.description}>
                    {t.name}
                  </span>
                  <span className="text-zinc-400">{t.count}</span>
                  <span className="flex-1" />
                  <button
                    onClick={() => openTagToColumn(t.name)}
                    disabled={t.count === 0}
                    title="Copy this tag into a real column"
                    className="rounded px-1 text-zinc-400 hover:bg-zinc-100 hover:text-violet-500 disabled:opacity-40 dark:hover:bg-zinc-800"
                  >
                    → column
                  </button>
                  <button
                    onClick={() => void removeTag(t.name)}
                    title="Remove this tag from the namespace and every row"
                    className="rounded px-1 text-zinc-400 hover:bg-zinc-100 hover:text-red-500 dark:hover:bg-zinc-800"
                  >
                    ✕
                  </button>
                </div>
              ))}
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  const name = normalizeTagName(newTag);
                  if (name) void defineTag({ name }).then(() => setNewTag(""));
                }}
                className="flex items-center gap-1.5 pt-1"
              >
                <input
                  value={newTag}
                  onChange={(e) => setNewTag(e.target.value)}
                  placeholder="New tag…"
                  className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-xs outline-none focus:border-violet-400 dark:border-zinc-700"
                />
                <button
                  type="submit"
                  disabled={!normalizeTagName(newTag)}
                  className="rounded border border-zinc-200 px-2 py-1 text-xs hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700"
                >
                  Add
                </button>
              </form>
            </div>
          )}
        </div>
      )}

      {/* entries */}
      <div className="min-h-0 flex-1 space-y-1.5 overflow-y-auto p-2">
        {!view && <p className="py-6 text-center text-xs text-zinc-400">Loading…</p>}
        {view && filtered.length === 0 && (
          <p className="py-6 text-center text-xs text-zinc-400">
            {view.entries.length === 0
              ? "No annotations yet. Star, flag, tag or note rows from the grid."
              : "No annotations match this filter."}
          </p>
        )}
        {filtered.map((e) => (
          <EntryCard
            key={e.handle}
            entry={e}
            headerFor={headerFor}
            onJump={jumpTo}
            onRemove={() => void removeAnnotation(e.handle)}
            onEditNote={() => {
              // Jump first: that resets any sort/filter, so the display row
              // then equals the record and the editor targets the right row.
              if (e.record == null) return;
              void jumpToCell(e.record, 0).then(() =>
                openRowNote(e.record!, `Row ${e.record! + 1}`, e.note?.text ?? ""),
              );
            }}
          />
        ))}
      </div>

      {/* settings: author + anchoring key columns */}
      <div className="border-t border-zinc-200 text-xs dark:border-zinc-800">
        <button
          onClick={() => setShowSettings((v) => !v)}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-left font-medium text-zinc-500 hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
        >
          <span>Anchoring &amp; author</span>
          <span className="flex-1" />
          <span className="text-zinc-400">{showSettings ? "▾" : "▸"}</span>
        </button>
        {showSettings && (
          <div className="space-y-2 px-3 pb-3">
            <label className="block">
              <span className="text-zinc-500">Note author</span>
              <input
                defaultValue={view?.author ?? ""}
                onBlur={(e) => void setAuthor(e.target.value.trim() || null)}
                placeholder="(none)"
                className="mt-0.5 w-full rounded border border-zinc-300 bg-transparent px-2 py-1 outline-none focus:border-violet-400 dark:border-zinc-700"
              />
            </label>
            <KeyColumnsEditor
              headers={meta.headers}
              columnIds={meta.columnIds}
              active={view?.keyColumns ?? []}
              onApply={(cols) => void setKeySpec(cols.length ? { columns: cols } : null)}
            />
          </div>
        )}
      </div>
    </aside>
  );
}

function EntryCard({
  entry,
  headerFor,
  onJump,
  onRemove,
  onEditNote,
}: {
  entry: RowAnnotationView;
  headerFor: (columnId: string) => string;
  onJump: (record: number) => void;
  onRemove: () => void;
  onEditNote: () => void;
}) {
  const statusClass =
    entry.status === "matched"
      ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300"
      : entry.status === "ambiguous"
        ? "bg-amber-100 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300"
        : "bg-zinc-200 text-zinc-600 dark:bg-zinc-700 dark:text-zinc-300";

  return (
    <div className="rounded border border-zinc-200 p-2 text-xs dark:border-zinc-800">
      <div className="flex items-center gap-1.5">
        <span className={`rounded px-1.5 py-0.5 text-[11px] ${statusClass}`}>
          {matchStatusLabel(entry.status)}
        </span>
        {entry.record != null ? (
          <button
            onClick={() => onJump(entry.record!)}
            title="Jump to this row"
            className="rounded border border-zinc-200 px-1.5 py-0.5 font-mono text-[11px] hover:border-violet-400 dark:border-zinc-700"
          >
            Row {entry.record + 1}
          </button>
        ) : (
          <span className="font-mono text-[11px] text-zinc-400">
            {entry.candidates && entry.candidates.length > 0
              ? `candidates: ${entry.candidates
                  .slice(0, 4)
                  .map((c) => c + 1)
                  .join(", ")}${entry.candidates.length > 4 ? "…" : ""}`
              : "no matching row"}
          </span>
        )}
        {entry.star && <StarGlyph on />}
        {entry.flag && <FlagGlyph on />}
        <span className="flex-1" />
        <button
          onClick={onRemove}
          title="Delete this annotation"
          className="rounded px-1 text-zinc-400 hover:bg-zinc-100 hover:text-red-500 dark:hover:bg-zinc-800"
        >
          ✕
        </button>
      </div>

      {entry.tags && entry.tags.length > 0 && (
        <div className="mt-1 flex flex-wrap gap-1">
          {entry.tags.map((t) => (
            <span
              key={t}
              className="rounded-full px-1.5 py-0.5 text-[10px] text-white"
              style={{ background: tagColor(t) }}
            >
              {t}
            </span>
          ))}
        </div>
      )}

      {entry.note && (
        <button onClick={onEditNote} className="mt-1 block w-full text-left" title="Edit this note">
          <span className="text-zinc-700 dark:text-zinc-200">{entry.note.text}</span>
          <span className="ml-1 text-[10px] text-zinc-400">
            {entry.note.author ? `— ${entry.note.author} · ` : ""}
            {noteTimeLabel(entry.note.updatedMs)}
          </span>
        </button>
      )}

      {entry.cellNotes && entry.cellNotes.length > 0 && (
        <div className="mt-1 space-y-0.5">
          {entry.cellNotes.map((cn) => (
            <div key={cn.columnId} className="flex gap-1.5">
              <span className="shrink-0 rounded bg-violet-100 px-1 text-[10px] text-violet-700 dark:bg-violet-500/15 dark:text-violet-300">
                {headerFor(cn.columnId)}
              </span>
              <span className="truncate text-zinc-600 dark:text-zinc-300">{cn.note.text}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** A compact multi-select of key columns for annotation anchoring. Applying
 * writes a KeySpec of stable column ids (survives reorder/rename). */
function KeyColumnsEditor({
  headers,
  columnIds,
  active,
  onApply,
}: {
  headers: string[];
  columnIds: string[];
  active: string[];
  onApply: (columns: string[]) => void;
}) {
  const [sel, setSel] = useState<string[]>(active);
  const dirty = sel.join(" ") !== active.join(" ");

  const toggle = (id: string) =>
    setSel((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]));

  return (
    <div>
      <div className="mb-1 flex items-center gap-1.5">
        <span className="text-zinc-500">Key columns</span>
        <span className="flex-1" />
        <button
          onClick={() => onApply(sel)}
          disabled={!dirty}
          className="rounded border border-zinc-200 px-1.5 py-0.5 hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700"
        >
          Apply
        </button>
      </div>
      <p className="mb-1 text-[10px] text-zinc-400">
        Anchor annotations to these columns so they survive row reordering.
      </p>
      <div className="max-h-28 space-y-0.5 overflow-y-auto rounded border border-zinc-200 p-1 dark:border-zinc-700">
        {columnIds.map((id, i) => (
          <label key={id || i} className="flex items-center gap-1.5">
            <input type="checkbox" checked={sel.includes(id)} onChange={() => toggle(id)} />
            <span className="truncate">{headers[i] || `Column ${i + 1}`}</span>
          </label>
        ))}
      </div>
    </div>
  );
}
