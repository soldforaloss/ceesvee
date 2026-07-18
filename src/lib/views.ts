// Pure helpers for named views (F12): building a view from the current
// document state, CRUD over a profile's saved views, and hydrating persisted
// filters back into the UI's tree shape. No React, no store, no invoke.

import type {
  DocumentMeta,
  FilterGroup,
  FilterNode,
  HighlightRule,
  NamedView,
  SortKey,
  ViewSortKey,
} from "../types";
import { widthsToIds, type ColumnLayout } from "./viewProjection";

let viewIdSeq = 0;

/** Unique, persistence-safe view id. */
export function newViewId(): string {
  viewIdSeq += 1;
  return `view-${Date.now().toString(36)}-${viewIdSeq}${Math.random().toString(36).slice(2, 6)}`;
}

let hydrateSeq = 0;

/**
 * A filter loaded from settings has no UI node ids (the backend does not
 * store them); give every node a fresh one so the filter tree can be edited.
 */
export function hydrateFilter(filter: FilterGroup): FilterGroup {
  const withIds = (node: FilterNode): FilterNode => {
    hydrateSeq += 1;
    if (node.type === "condition") {
      return { ...node, id: node.id ?? `view-node-${hydrateSeq}` };
    }
    return {
      ...node,
      id: node.id ?? `view-node-${hydrateSeq}`,
      nodes: node.nodes.map(withIds),
    };
  };
  return withIds(filter) as FilterGroup;
}

/** "name", "name (2)", "name (3)"… against the existing view names. */
export function uniqueViewName(existing: string[], base: string): string {
  const taken = new Set(existing.map((n) => n.toLowerCase()));
  const trimmed = base.trim() || "Untitled view";
  if (!taken.has(trimmed.toLowerCase())) return trimmed;
  for (let i = 2; ; i++) {
    const candidate = `${trimmed} (${i})`;
    if (!taken.has(candidate.toLowerCase())) return candidate;
  }
}

/** Replace the view with the same id, or append. Returns a new array. */
export function upsertView(views: NamedView[], view: NamedView): NamedView[] {
  const at = views.findIndex((v) => v.id === view.id);
  if (at === -1) return [...views, view];
  const next = [...views];
  next[at] = view;
  return next;
}

/** Physical sort keys → ID-based keys for persistence. */
export function sortKeysToIds(keys: SortKey[], columnIds: string[]): ViewSortKey[] {
  const out: ViewSortKey[] = [];
  for (const key of keys) {
    const id = columnIds[key.column];
    if (id !== undefined) out.push({ columnId: id, descending: key.descending });
  }
  return out;
}

/** Everything a view snapshot needs from the live document state. */
export interface ViewSnapshotInput {
  name: string;
  /** Keep this id to REPLACE an existing view; omit for a new one. */
  id?: string;
  meta: DocumentMeta;
  /** The applied filter tree, or null when the document is unfiltered. */
  filter: FilterGroup | null;
  /** The applied non-destructive sort, in physical columns. */
  viewSortKeys: SortKey[];
  layout: ColumnLayout | null;
  columnWidths: Record<number, number>;
  wrapText: boolean;
  /** F42: the active document's conditional-highlighting rules, if any. */
  highlightRules?: HighlightRule[];
}

/** Build a persistable NamedView from the current state. */
export function snapshotView(input: ViewSnapshotInput): NamedView {
  const ids = input.meta.columnIds;
  return {
    id: input.id ?? newViewId(),
    name: input.name,
    filter: input.filter,
    filterColumnIds: input.filter ? [...ids] : [],
    sortKeys: sortKeysToIds(input.viewSortKeys, ids),
    hiddenColumnIds: [...(input.layout?.hiddenColumnIds ?? [])],
    pinnedColumnIds: [...(input.layout?.pinnedColumnIds ?? [])],
    columnOrder: [...(input.layout?.columnOrder ?? [])],
    columnWidths: widthsToIds(input.columnWidths, ids),
    wrapText: input.wrapText,
    highlightRules: (input.highlightRules ?? []).map((r) => ({ ...r })),
  };
}

/** One-line summary of what a view changes, for lists and the palette. */
export function describeView(view: NamedView): string {
  const parts: string[] = [];
  if (view.filter) parts.push("filter");
  if (view.sortKeys.length > 0) {
    parts.push(view.sortKeys.length === 1 ? "sort" : `sort ×${view.sortKeys.length}`);
  }
  if (view.hiddenColumnIds.length > 0) parts.push(`${view.hiddenColumnIds.length} hidden`);
  if (view.pinnedColumnIds.length > 0) parts.push(`${view.pinnedColumnIds.length} pinned`);
  if (view.columnOrder.length > 0) parts.push("reordered");
  if (view.wrapText) parts.push("wrap");
  if (view.highlightRules && view.highlightRules.length > 0) {
    parts.push(
      `${view.highlightRules.length} highlight${view.highlightRules.length === 1 ? "" : "s"}`,
    );
  }
  return parts.length > 0 ? parts.join(" · ") : "layout only";
}
