// Pure helpers for multi-facet exploration (F39): selection-state reducers,
// count formatting, the facets → filter conversion summary, and small config
// updaters. No React, no store, no invoke — everything here is unit-tested.

import type {
  DroppedFacet,
  FacetConfig,
  FacetConversion,
  FacetKind,
  FacetMode,
  FacetResult,
  FacetSelection,
  FacetSpec,
  FilterGroup,
  FilterNode,
  SemanticType,
} from "../types";

// ----- facet metadata -------------------------------------------------------

/** Human labels for the picker and card headers. */
export const FACET_KIND_LABELS: Record<FacetKind, string> = {
  text: "Values",
  number: "Number range",
  date: "Date range",
  boolean: "True / false",
  nullability: "Blank / null / invalid",
  semantic: "Semantic type",
  diagnostics: "Diagnostics status",
  validation: "Validation status",
  duplicate: "Duplicate status",
  annotation: "Bookmarks / flags / tags",
};

/** Column-scoped facets need a column; the four status facets do not. */
export const COLUMN_SCOPED_KINDS: ReadonlySet<FacetKind> = new Set<FacetKind>([
  "text",
  "number",
  "date",
  "boolean",
  "nullability",
  "semantic",
]);

/** The row-level status facets, sourced from the analysis caches. */
export const STATUS_KINDS: ReadonlySet<FacetKind> = new Set<FacetKind>([
  "diagnostics",
  "validation",
  "duplicate",
  "annotation",
]);

/** Facet kinds that convert cleanly to a filter-builder condition. Boolean,
 * semantic and the status facets have no faithful column-filter equivalent, and
 * nullability only converts for a single blank/value include. */
export const CONVERTIBLE_KINDS: ReadonlySet<FacetKind> = new Set<FacetKind>([
  "text",
  "number",
  "date",
  "nullability",
]);

/** Semantic types offered when adding a semantic facet (those with a matcher). */
export const SEMANTIC_FACET_TYPES: SemanticType[] = [
  "email",
  "url",
  "uuid",
  "ipv4",
  "ipv6",
  "json",
  "percentage",
  "currency",
  "phoneNumber",
  "postalCode",
];

export function isColumnScoped(kind: FacetKind): boolean {
  return COLUMN_SCOPED_KINDS.has(kind);
}

// ----- spec construction ----------------------------------------------------

let facetSeq = 0;

/** Unique, persistence-safe facet panel id. */
export function newFacetId(): string {
  facetSeq += 1;
  return `facet-${Date.now().toString(36)}-${facetSeq}${Math.random().toString(36).slice(2, 5)}`;
}

/** A fresh, empty selection (include mode, no values, no range). */
export function emptySelection(): FacetSelection {
  return { mode: "include", values: [], range: {} };
}

/** Build a default facet spec for a kind (+ column / semantic type as needed). */
export function createFacetSpec(
  kind: FacetKind,
  columnId?: string | null,
  semantic?: SemanticType | null,
): FacetSpec {
  return {
    id: newFacetId(),
    kind,
    columnId: columnId ?? null,
    semantic: semantic ?? null,
    selection: emptySelection(),
    pinned: false,
    collapsed: false,
  };
}

// ----- selection-state reducers (pure) --------------------------------------

function hasBound(value: string | null | undefined): boolean {
  return typeof value === "string" && value.trim() !== "";
}

/** Whether a selection is currently narrowing the population. */
export function selectionActive(sel: FacetSelection): boolean {
  return sel.values.length > 0 || hasBound(sel.range.min) || hasBound(sel.range.max);
}

/** Whether ANY facet in the config carries an active selection. */
export function anyFacetActive(config: FacetConfig): boolean {
  return config.facets.some((f) => selectionActive(f.selection));
}

/** Toggle one categorical value in the OR set (add if absent, remove if present). */
export function toggleValue(sel: FacetSelection, key: string): FacetSelection {
  const has = sel.values.includes(key);
  return {
    ...sel,
    values: has ? sel.values.filter((v) => v !== key) : [...sel.values, key],
  };
}

/** Replace the whole value set (e.g. "select all" / "clear values"). */
export function setValues(sel: FacetSelection, values: string[]): FacetSelection {
  return { ...sel, values: [...values] };
}

/** Set the include/exclude mode. */
export function setMode(sel: FacetSelection, mode: FacetMode): FacetSelection {
  return { ...sel, mode };
}

/** Flip include ⇄ exclude. */
export function toggleMode(sel: FacetSelection): FacetSelection {
  return { ...sel, mode: sel.mode === "include" ? "exclude" : "include" };
}

/** Set the continuous range bounds (blank strings clear the bound). */
export function setRange(
  sel: FacetSelection,
  min: string | null,
  max: string | null,
): FacetSelection {
  return {
    ...sel,
    range: {
      min: hasBound(min) ? min!.trim() : null,
      max: hasBound(max) ? max!.trim() : null,
    },
  };
}

/** Clear the selection but keep the mode (so a cleared exclude stays exclude). */
export function clearSelection(sel: FacetSelection): FacetSelection {
  return { mode: sel.mode, values: [], range: {} };
}

// ----- config updaters (pure) -----------------------------------------------

export function addFacet(config: FacetConfig, spec: FacetSpec): FacetConfig {
  return { facets: [...config.facets, spec] };
}

export function removeFacet(config: FacetConfig, id: string): FacetConfig {
  return { facets: config.facets.filter((f) => f.id !== id) };
}

export function updateFacet(
  config: FacetConfig,
  id: string,
  fn: (spec: FacetSpec) => FacetSpec,
): FacetConfig {
  return { facets: config.facets.map((f) => (f.id === id ? fn(f) : f)) };
}

/** Update just one facet's selection through a reducer. */
export function updateSelection(
  config: FacetConfig,
  id: string,
  fn: (sel: FacetSelection) => FacetSelection,
): FacetConfig {
  return updateFacet(config, id, (f) => ({ ...f, selection: fn(f.selection) }));
}

/** Move a facet from one index to another (drag reorder). */
export function moveFacet(config: FacetConfig, from: number, to: number): FacetConfig {
  const n = config.facets.length;
  if (from < 0 || from >= n || to < 0 || to >= n || from === to) return config;
  const facets = [...config.facets];
  const [moved] = facets.splice(from, 1);
  facets.splice(to, 0, moved);
  return { facets };
}

/**
 * Display order for the panel: pinned facets first, then unpinned, each in the
 * config's own (drag) order. The stored config order is left untouched so pin
 * and reorder stay independent.
 */
export function displayOrder(config: FacetConfig): FacetSpec[] {
  const pinned = config.facets.filter((f) => f.pinned);
  const rest = config.facets.filter((f) => !f.pinned);
  return [...pinned, ...rest];
}

// ----- count formatting -----------------------------------------------------

/** Group digits with commas, deterministically (locale-independent). */
export function formatCount(n: number): string {
  const neg = n < 0;
  const digits = Math.abs(Math.trunc(n)).toString();
  const grouped = digits.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return neg ? `-${grouped}` : grouped;
}

/** The "estimated" prefix for a sampled count (almost-equal sign + ASCII space,
 * defined once so the UI and tests share one source of truth). */
export const ESTIMATE_MARK = "≈ ";

/** A bucket count, prefixed with the estimate mark when sampled. */
export function formatBucketCount(count: number, sampled: boolean): string {
  return `${sampled ? "≈ " : ""}${formatCount(count)}`;
}

/** "3 of 1,000 rows" style population summary. */
export function formatPopulation(matched: number, total: number, sampled: boolean): string {
  const est = sampled ? "≈ " : "";
  return `${formatCount(matched)} of ${est}${formatCount(total)} rows`;
}

// ----- clipboard ------------------------------------------------------------

/** Tab-separated "value\tcount" lines for the whole facet (copy values+counts). */
export function facetClipboardText(result: FacetResult): string {
  return result.buckets.map((b) => `${b.label}\t${b.count}`).join("\n");
}

// ----- conversion mapping ---------------------------------------------------

/** Count the leaf conditions in a filter tree (recursively). */
export function countConditions(group: FilterGroup): number {
  let total = 0;
  const walk = (nodes: FilterNode[]) => {
    for (const node of nodes) {
      if (node.type === "condition") total += 1;
      else walk(node.nodes);
    }
  };
  walk(group.nodes);
  return total;
}

/** Summary of a facets → filter conversion for the UI: how many conditions it
 * produced, whether it is empty (nothing convertible), and what was dropped. */
export interface ConversionSummary {
  conditionCount: number;
  empty: boolean;
  dropped: DroppedFacet[];
}

export function summarizeConversion(conv: FacetConversion): ConversionSummary {
  const conditionCount = countConditions(conv.filter);
  return { conditionCount, empty: conditionCount === 0, dropped: conv.dropped };
}

/** One-line human note about what a conversion dropped (empty string if none). */
export function describeDropped(dropped: DroppedFacet[]): string {
  if (dropped.length === 0) return "";
  const n = dropped.length;
  return `${n} facet${n === 1 ? "" : "s"} had no filter equivalent and ${
    n === 1 ? "was" : "were"
  } left out.`;
}
