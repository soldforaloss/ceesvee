import { useEffect, useRef, useState } from "react";

import {
  FACET_KIND_LABELS,
  SEMANTIC_FACET_TYPES,
  displayOrder,
  formatBucketCount,
  formatPopulation,
  isColumnScoped,
  selectionActive,
} from "../lib/facets";
import { useActiveMeta, useStore } from "../store/useStore";
import type { FacetKind, FacetResult, FacetSpec, SemanticType } from "../types";
import { BarChart, ChevronDown, ChevronUp, Close } from "./Icons";

/** Facet kinds offered in the add-facet picker, in menu order. */
const COLUMN_FACET_KINDS: FacetKind[] = [
  "text",
  "number",
  "date",
  "boolean",
  "nullability",
  "semantic",
];
const STATUS_FACET_KINDS: FacetKind[] = ["diagnostics", "validation", "duplicate", "annotation"];

/**
 * Multi-facet exploration panel (F39). Several facets are active at once; the
 * grid row view is driven by the AND across panels and the OR (with include /
 * exclude) inside one. Every facet's counts are recomputed against the
 * population filtered by all the OTHER facets, so counts always reflect the
 * current cross-filter. Faceting is non-destructive — it never dirties the
 * document — and integrates with the existing row-view pipeline, so visible-row
 * export just works.
 */
export function FacetsPanel() {
  const meta = useActiveMeta();
  const facets = useStore((s) => s.facets);
  const setOpen = useStore((s) => s.setFacetsOpen);
  const addFacet = useStore((s) => s.addFacet);
  const clearAll = useStore((s) => s.clearAllFacets);
  const convertToFilter = useStore((s) => s.convertFacetsToFilter);
  const syncFacets = useStore((s) => s.syncFacets);
  const setModal = useStore((s) => s.setModal);

  // Recompute when the panel opens or the active document changes (selection
  // changes drive their own debounced sync from the store).
  useEffect(() => {
    if (facets.open && meta) void syncFacets();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [facets.open, meta?.id]);

  if (!meta || !facets.open) return null;

  const results = facets.results;
  const anyActive = facets.config.facets.some((f) => selectionActive(f.selection));

  return (
    <aside className="flex w-[26rem] shrink-0 flex-col border-l border-zinc-200 bg-white text-sm dark:border-zinc-800 dark:bg-zinc-950">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-zinc-200 px-3 dark:border-zinc-800">
        <BarChart className="h-4 w-4 text-violet-500" />
        <span className="font-semibold text-zinc-700 dark:text-zinc-200">Facets</span>
        {facets.loading && <span className="text-[11px] text-zinc-400">…</span>}
        <div className="flex-1" />
        <button
          title="Close facets"
          onClick={() => setOpen(false)}
          className="rounded p-1 text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          <Close className="h-4 w-4" />
        </button>
      </div>

      {/* Population + actions */}
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-zinc-100 px-3 py-2 text-xs dark:border-zinc-800/60">
        <span className="tabular-nums text-zinc-500">
          {results
            ? formatPopulation(results.matchedRows, results.totalRows, results.sampled)
            : "—"}
        </span>
        {results?.sampled && (
          <span
            title="Counts estimated from a leading sample (large indexed document); the applied filter is exact."
            className="rounded bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:bg-amber-500/15 dark:text-amber-300"
          >
            estimated
          </span>
        )}
        <div className="flex-1" />
        <button
          onClick={() => void convertToFilter()}
          disabled={!anyActive}
          title="Convert the active facets to a filter (one-way)"
          className="rounded border border-zinc-300 px-1.5 py-0.5 text-zinc-600 hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          To filter
        </button>
        <button
          onClick={() => setModal("views")}
          title="Save the current facets and layout into a named view"
          className="rounded border border-zinc-300 px-1.5 py-0.5 text-zinc-600 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          Save view…
        </button>
        <button
          onClick={() => clearAll()}
          disabled={!anyActive}
          title="Clear every facet's selection"
          className="rounded border border-zinc-300 px-1.5 py-0.5 text-zinc-600 hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          Clear
        </button>
      </div>

      <AddFacetBar meta={meta} onAdd={addFacet} />

      {facets.error && (
        <p className="mx-3 mt-2 rounded border border-red-200 bg-red-50 px-2 py-1.5 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/50 dark:text-red-300">
          {facets.error}
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        {facets.config.facets.length === 0 ? (
          <p className="mt-6 text-center text-xs text-zinc-400">
            Add a facet to slice the data by a column or a status dimension.
          </p>
        ) : (
          <ul className="space-y-2.5">
            {displayOrder(facets.config).map((spec) => (
              <FacetCard
                key={spec.id}
                spec={spec}
                result={results?.facets.find((f) => f.id === spec.id) ?? null}
                headers={meta.headers}
                columnIds={meta.columnIds}
              />
            ))}
          </ul>
        )}
      </div>
    </aside>
  );
}

/** The add-facet control: a kind picker plus a column (or semantic type) picker. */
function AddFacetBar({
  meta,
  onAdd,
}: {
  meta: NonNullable<ReturnType<typeof useActiveMeta>>;
  onAdd: (kind: FacetKind, columnId?: string | null, semantic?: SemanticType | null) => void;
}) {
  const [kind, setKind] = useState<FacetKind>("text");
  const [column, setColumn] = useState(0);
  const [semantic, setSemantic] = useState<SemanticType>("email");
  const scoped = isColumnScoped(kind);

  const add = () => {
    if (scoped) {
      const columnId = meta.columnIds[column] ?? null;
      onAdd(kind, columnId, kind === "semantic" ? semantic : null);
    } else {
      onAdd(kind);
    }
  };

  return (
    <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-zinc-100 px-3 py-2 dark:border-zinc-800/60">
      <select
        value={kind}
        onChange={(e) => setKind(e.target.value as FacetKind)}
        className={selectCls}
        title="Facet type"
      >
        <optgroup label="Column">
          {COLUMN_FACET_KINDS.map((k) => (
            <option key={k} value={k} className="dark:bg-zinc-800">
              {FACET_KIND_LABELS[k]}
            </option>
          ))}
        </optgroup>
        <optgroup label="Status">
          {STATUS_FACET_KINDS.map((k) => (
            <option key={k} value={k} className="dark:bg-zinc-800">
              {FACET_KIND_LABELS[k]}
            </option>
          ))}
        </optgroup>
      </select>

      {scoped && (
        <select
          value={column}
          onChange={(e) => setColumn(Number(e.target.value))}
          className={`${selectCls} min-w-0 flex-1`}
          title="Column"
        >
          {meta.headers.map((h, i) => (
            <option key={i} value={i} className="dark:bg-zinc-800">
              {h.trim() || `Column ${i + 1}`}
            </option>
          ))}
        </select>
      )}

      {kind === "semantic" && (
        <select
          value={semantic}
          onChange={(e) => setSemantic(e.target.value as SemanticType)}
          className={selectCls}
          title="Semantic type"
        >
          {SEMANTIC_FACET_TYPES.map((t) => (
            <option key={t} value={t} className="dark:bg-zinc-800">
              {t}
            </option>
          ))}
        </select>
      )}

      <button
        onClick={add}
        className="rounded bg-violet-600 px-2 py-1 text-xs font-medium text-white hover:bg-violet-500"
      >
        Add
      </button>
    </div>
  );
}

/** One facet panel: header (title / mode / pin / collapse / copy / remove) plus
 * a type-appropriate body. Draggable to reorder. */
function FacetCard({
  spec,
  result,
  headers,
  columnIds,
}: {
  spec: FacetSpec;
  result: FacetResult | null;
  headers: string[];
  columnIds: string[];
}) {
  const config = useStore((s) => s.facets.config);
  const removeFacet = useStore((s) => s.removeFacet);
  const patchFacet = useStore((s) => s.patchFacet);
  const toggleMode = useStore((s) => s.toggleFacetMode);
  const clearFacet = useStore((s) => s.clearFacet);
  const copyFacet = useStore((s) => s.copyFacet);
  const reorderFacet = useStore((s) => s.reorderFacet);
  const dragId = useRef<string | null>(null);

  const title = isColumnScoped(spec.kind)
    ? `${columnName(headers, columnIds, spec.columnId)} · ${FACET_KIND_LABELS[spec.kind]}`
    : FACET_KIND_LABELS[spec.kind];
  const active = selectionActive(spec.selection);

  const onDrop = (targetId: string) => {
    const from = config.facets.findIndex((f) => f.id === dragId.current);
    const to = config.facets.findIndex((f) => f.id === targetId);
    if (from >= 0 && to >= 0) reorderFacet(from, to);
    dragId.current = null;
  };

  return (
    <li
      draggable
      onDragStart={() => (dragId.current = spec.id)}
      onDragOver={(e) => e.preventDefault()}
      onDrop={() => onDrop(spec.id)}
      className={`rounded-lg border ${
        active
          ? "border-violet-300 dark:border-violet-500/40"
          : "border-zinc-200 dark:border-zinc-800"
      } bg-white dark:bg-zinc-900/40`}
    >
      <div className="flex items-center gap-1 px-2 py-1.5">
        <button
          title={spec.collapsed ? "Expand" : "Collapse"}
          onClick={() => patchFacet(spec.id, { collapsed: !spec.collapsed })}
          className="rounded p-0.5 text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          {spec.collapsed ? (
            <ChevronDown className="h-3.5 w-3.5" />
          ) : (
            <ChevronUp className="h-3.5 w-3.5" />
          )}
        </button>
        <span
          className="min-w-0 flex-1 truncate text-xs font-medium text-zinc-700 dark:text-zinc-200"
          title={title}
        >
          {title}
        </span>
        {result?.sampled && (
          <span className="rounded bg-amber-100 px-1 text-[9px] font-medium text-amber-700 dark:bg-amber-500/15 dark:text-amber-300">
            est
          </span>
        )}
        <button
          title={
            spec.selection.mode === "include"
              ? "Including matches — click to exclude"
              : "Excluding matches — click to include"
          }
          onClick={() => toggleMode(spec.id)}
          className={`rounded px-1 text-[10px] font-semibold ${
            spec.selection.mode === "exclude"
              ? "bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300"
              : "bg-violet-100 text-violet-700 dark:bg-violet-500/15 dark:text-violet-300"
          }`}
        >
          {spec.selection.mode === "exclude" ? "NOT" : "IN"}
        </button>
        <button
          title={spec.pinned ? "Unpin" : "Pin to top"}
          onClick={() => patchFacet(spec.id, { pinned: !spec.pinned })}
          className={`rounded px-1 text-xs ${
            spec.pinned
              ? "text-violet-600 dark:text-violet-300"
              : "text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800"
          }`}
        >
          ★
        </button>
        <button
          title="Copy values and counts"
          onClick={() => void copyFacet(spec.id)}
          className="rounded px-1 text-xs text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          ⧉
        </button>
        <button
          title="Remove facet"
          onClick={() => removeFacet(spec.id)}
          className="rounded p-0.5 text-zinc-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-500/10 dark:hover:text-red-300"
        >
          <Close className="h-3.5 w-3.5" />
        </button>
      </div>

      {!spec.collapsed && (
        <div className="border-t border-zinc-100 px-2.5 py-2 dark:border-zinc-800/60">
          {result?.unresolved ? (
            <UnresolvedNote kind={spec.kind} />
          ) : (
            <FacetBody spec={spec} result={result} />
          )}
          {active && (
            <button
              onClick={() => clearFacet(spec.id)}
              className="mt-1.5 text-[11px] text-violet-600 hover:underline dark:text-violet-300"
            >
              Clear selection
            </button>
          )}
        </div>
      )}
    </li>
  );
}

function FacetBody({ spec, result }: { spec: FacetSpec; result: FacetResult | null }) {
  if (!result) return <p className="text-[11px] text-zinc-400">Computing…</p>;
  switch (spec.kind) {
    case "number":
    case "date":
      return <RangeBody spec={spec} result={result} />;
    default:
      // text / boolean / nullability / semantic / status all render value lists.
      return <ValueListBody spec={spec} result={result} />;
  }
}

function UnresolvedNote({ kind }: { kind: FacetKind }) {
  const msg =
    kind === "diagnostics"
      ? "Run a diagnostics scan to enable this facet."
      : kind === "duplicate"
        ? "Run a duplicate scan (Duplicates dialog) to enable this facet."
        : "This facet's column no longer exists in the current layout.";
  return <p className="text-[11px] text-zinc-400">{msg}</p>;
}

/** Text / boolean / nullability / semantic / status: a checkbox value list with
 * cross-filtered counts, a proportion bar, and (for text) a search box. */
function ValueListBody({ spec, result }: { spec: FacetSpec; result: FacetResult }) {
  const toggleValue = useStore((s) => s.toggleFacetValue);
  const patchFacet = useStore((s) => s.patchFacet);
  const isText = spec.kind === "text";
  const maxCount = result.buckets.reduce((m, b) => Math.max(m, b.count), 0) || 1;

  return (
    <div>
      {isText && (
        <input
          value={spec.search ?? ""}
          onChange={(e) => patchFacet(spec.id, { search: e.target.value || null })}
          placeholder={
            result.distinct != null
              ? `Search ${result.distinct.toLocaleString()} values…`
              : "Search…"
          }
          className="mb-1.5 w-full rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-xs outline-none focus:border-violet-500 dark:border-zinc-700"
        />
      )}
      {result.buckets.length === 0 ? (
        <p className="text-[11px] text-zinc-400">No values.</p>
      ) : (
        <ul className="space-y-0.5">
          {result.buckets.map((b) => (
            <li
              key={b.key}
              className="group relative flex cursor-pointer items-center gap-2 rounded px-1 py-0.5 text-xs hover:bg-zinc-50 dark:hover:bg-zinc-900"
              onClick={() => toggleValue(spec.id, b.key)}
            >
              <span
                className="pointer-events-none absolute inset-y-0 left-0 rounded bg-violet-100/60 dark:bg-violet-500/10"
                style={{ width: `${Math.max(2, (b.count / maxCount) * 100)}%` }}
              />
              <input
                type="checkbox"
                readOnly
                checked={b.selected}
                className="relative h-3 w-3 shrink-0 accent-violet-600"
              />
              <span className="relative min-w-0 flex-1 truncate" title={b.label}>
                {b.label === "" ? <em className="text-zinc-400">(empty)</em> : b.label}
              </span>
              <span className="relative shrink-0 tabular-nums text-zinc-400">
                {formatBucketCount(b.count, result.sampled)}
              </span>
            </li>
          ))}
        </ul>
      )}
      {result.truncated && (
        <p className="mt-1 text-[10px] text-amber-600 dark:text-amber-400">
          Showing top values only — this column has more distinct values than can be listed.
        </p>
      )}
    </div>
  );
}

// The backend labels a histogram bin "lo – hi" (spaced en dash); split on the
// spaced dash so date/number labels with internal hyphens stay intact.
const RANGE_SEP = /\s+[–-]\s+/;

/** Number / date: a pure-CSS histogram (click a bar to select its range) plus
 * min/max inputs for a precise continuous range. */
function RangeBody({ spec, result }: { spec: FacetSpec; result: FacetResult }) {
  const setRange = useStore((s) => s.setFacetRange);
  const range = result.range;
  const selMin = range?.selectedMin ?? "";
  const selMax = range?.selectedMax ?? "";
  const [min, setMin] = useState(selMin);
  const [max, setMax] = useState(selMax);

  // Keep local inputs in sync when the selection changes elsewhere (bar click,
  // clear, view restore).
  useEffect(() => setMin(selMin), [selMin]);
  useEffect(() => setMax(selMax), [selMax]);

  const maxCount = result.buckets.reduce((m, b) => Math.max(m, b.count), 0) || 1;

  const barSelected = (lo?: number, hi?: number) => {
    if (!selectionActive(spec.selection) || lo == null || hi == null) return false;
    return overlapsSelection(spec, lo, hi);
  };

  const clickBar = (label: string) => {
    const parts = label.split(RANGE_SEP);
    if (parts.length === 2) setRange(spec.id, parts[0].trim(), parts[1].trim());
  };

  return (
    <div>
      {range && (range.min != null || range.max != null) && (
        <p className="mb-1 text-[10px] text-zinc-400">
          Range {range.min ?? "—"} to {range.max ?? "—"}
        </p>
      )}
      {result.buckets.length > 0 && (
        <div className="mb-2 flex h-16 items-end gap-px" title="Click a bar to select its range">
          {result.buckets.map((b) => (
            <button
              key={b.key}
              onClick={() => clickBar(b.label)}
              title={`${b.label}: ${formatBucketCount(b.count, result.sampled)}`}
              className={`min-w-0 flex-1 rounded-t ${
                barSelected(b.lo, b.hi)
                  ? "bg-violet-500"
                  : "bg-violet-300/70 hover:bg-violet-400 dark:bg-violet-500/30 dark:hover:bg-violet-500/60"
              }`}
              style={{ height: `${Math.max(3, (b.count / maxCount) * 100)}%` }}
            />
          ))}
        </div>
      )}
      <div className="flex items-center gap-1.5 text-xs">
        <input
          value={min}
          onChange={(e) => setMin(e.target.value)}
          onBlur={() => setRange(spec.id, min || null, max || null)}
          onKeyDown={(e) => e.key === "Enter" && setRange(spec.id, min || null, max || null)}
          placeholder={range?.min ?? "min"}
          className={rangeInputCls}
        />
        <span className="text-zinc-400">to</span>
        <input
          value={max}
          onChange={(e) => setMax(e.target.value)}
          onBlur={() => setRange(spec.id, min || null, max || null)}
          onKeyDown={(e) => e.key === "Enter" && setRange(spec.id, min || null, max || null)}
          placeholder={range?.max ?? "max"}
          className={rangeInputCls}
        />
      </div>
    </div>
  );
}

/** Whether a histogram bar [lo,hi] overlaps the facet's selected numeric band.
 * The selection bounds are strings in the column's format, so we compare on the
 * bar's numeric edges only when both selection bounds parse as finite numbers;
 * date columns fall back to no highlight (still fully usable via the inputs). */
function overlapsSelection(spec: FacetSpec, lo: number, hi: number): boolean {
  const min = spec.selection.range.min;
  const max = spec.selection.range.max;
  const lowerOk = min == null || !Number.isFinite(Number(min)) || hi >= Number(min);
  const upperOk = max == null || !Number.isFinite(Number(max)) || lo <= Number(max);
  const anyNumeric =
    (min != null && Number.isFinite(Number(min))) || (max != null && Number.isFinite(Number(max)));
  return anyNumeric && lowerOk && upperOk;
}

/** Resolve a facet's stable column ID to its current header label (falls back
 * to the ID when the column no longer exists — the card also flags unresolved). */
function columnName(headers: string[], columnIds: string[], columnId?: string | null): string {
  if (!columnId) return "—";
  const idx = columnIds.indexOf(columnId);
  if (idx < 0) return columnId;
  return headers[idx]?.trim() || `Column ${idx + 1}`;
}

const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-xs outline-none focus:border-violet-500 dark:border-zinc-700";
const rangeInputCls =
  "w-24 rounded border border-zinc-300 bg-transparent px-1.5 py-1 tabular-nums outline-none focus:border-violet-500 dark:border-zinc-700";
