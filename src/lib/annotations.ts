// Pure helpers for the row bookmarks / tags / notes UI (F40). Everything here
// is side-effect free and unit-tested; the store and components own the async
// invoke calls and rendering. Annotations are keyed by ABSOLUTE record number
// (matched entries only) so the grid can place indicators through a
// display-row -> record map, independent of the current sort/filter.

import type {
  AnnotationPredicate,
  AnnotationsView,
  MatchStatus,
  RowAnnotationView,
} from "../types";

/** The state filters offered in the panel and filter bar, in display order. */
export type AnnotationFilterKind =
  | "all"
  | "starred"
  | "flagged"
  | "tagged"
  | "hasNote"
  | "hasCellNote"
  | "review";

/** Index the MATCHED entries by their resolved record number. Ambiguous and
 * orphaned entries have no single record and are intentionally excluded — an
 * uncertain row never gets a gutter indicator. */
export function buildRecordIndex(view: AnnotationsView | null): Map<number, RowAnnotationView> {
  const map = new Map<number, RowAnnotationView>();
  if (!view) return map;
  for (const entry of view.entries) {
    if (entry.status === "matched" && entry.record != null) map.set(entry.record, entry);
  }
  return map;
}

/** The stable column ids that carry a cell note on this entry. */
export function cellNoteColumns(entry: RowAnnotationView): Set<string> {
  const set = new Set<string>();
  for (const cn of entry.cellNotes ?? []) set.add(cn.columnId);
  return set;
}

/** Whether an entry carries any live annotation at all. */
export function entryHasAnnotation(entry: RowAnnotationView): boolean {
  return (
    entry.star ||
    entry.flag ||
    (entry.tags?.length ?? 0) > 0 ||
    entry.note != null ||
    (entry.cellNotes?.length ?? 0) > 0
  );
}

/** How many glyph "slots" a row shows in the gutter (star, flag, note). Used
 * only to size the drawn indicator cluster. */
export function gutterGlyphCount(entry: RowAnnotationView): number {
  let n = 0;
  if (entry.star) n += 1;
  if (entry.flag) n += 1;
  if (entry.note != null || (entry.tags?.length ?? 0) > 0) n += 1;
  return n;
}

/** Human label for a match status. */
export function matchStatusLabel(status: MatchStatus): string {
  switch (status) {
    case "matched":
      return "Matched";
    case "ambiguous":
      return "Ambiguous";
    case "orphaned":
      return "Orphaned";
  }
}

/** The annotation-state predicate for a panel/filter kind, or null for kinds
 * that are not a single backend predicate ("all", "review"). */
export function predicateForKind(
  kind: AnnotationFilterKind,
  tag?: string,
): AnnotationPredicate | null {
  switch (kind) {
    case "starred":
      return { type: "starred" };
    case "flagged":
      return { type: "flagged" };
    case "tagged":
      return { type: "tagged", tag };
    case "hasNote":
      return { type: "hasNote" };
    case "hasCellNote":
      return { type: "hasCellNote" };
    default:
      return null;
  }
}

/** Human label for an annotation predicate (for the status bar / filter chip). */
export function predicateLabel(p: AnnotationPredicate): string {
  switch (p.type) {
    case "starred":
      return "Starred rows";
    case "flagged":
      return "Flagged rows";
    case "tagged":
      return p.tag ? `Tagged "${p.tag}"` : "Tagged rows";
    case "hasNote":
      return "Rows with a note";
    case "hasCellNote":
      return "Rows with a cell note";
    case "anyAnnotation":
      return "Annotated rows";
  }
}

/** Client-side mirror of the backend predicate (annotations.rs `matches`), used
 * to filter the panel list without a round-trip. Kept in lock-step with Rust. */
export function predicateMatches(entry: RowAnnotationView, p: AnnotationPredicate): boolean {
  switch (p.type) {
    case "starred":
      return entry.star;
    case "flagged":
      return entry.flag;
    case "tagged":
      return p.tag ? (entry.tags?.includes(p.tag) ?? false) : (entry.tags?.length ?? 0) > 0;
    case "hasNote":
      return entry.note != null;
    case "hasCellNote":
      return (entry.cellNotes?.length ?? 0) > 0;
    case "anyAnnotation":
      return entryHasAnnotation(entry);
  }
}

/** Whether an entry passes a panel filter kind. "all" always passes; "review"
 * selects the non-matched (ambiguous/orphaned) entries. */
export function entryPassesKind(entry: RowAnnotationView, kind: AnnotationFilterKind): boolean {
  if (kind === "all") return true;
  if (kind === "review") return entry.status !== "matched";
  const p = predicateForKind(kind);
  return p ? predicateMatches(entry, p) : true;
}

/** Case-insensitive search over an entry's tags and note text (row + cell). */
export function entryMatchesQuery(entry: RowAnnotationView, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  if (entry.tags?.some((t) => t.toLowerCase().includes(q))) return true;
  if (entry.note?.text.toLowerCase().includes(q)) return true;
  if (entry.note?.author?.toLowerCase().includes(q)) return true;
  for (const cn of entry.cellNotes ?? []) {
    if (cn.note.text.toLowerCase().includes(q)) return true;
  }
  return false;
}

/** Order the panel entries: matched (by record) first, then review items by
 * handle. Mirrors the backend's stable order but re-applied after client-side
 * filtering. */
export function sortEntries(entries: RowAnnotationView[]): RowAnnotationView[] {
  return [...entries].sort((a, b) => {
    const ar = a.record ?? Number.POSITIVE_INFINITY;
    const br = b.record ?? Number.POSITIVE_INFINITY;
    if (ar !== br) return ar - br;
    return a.handle - b.handle;
  });
}

/** A validated tag name: trimmed, non-empty. Returns null when unusable. */
export function normalizeTagName(name: string): string | null {
  const t = name.trim();
  return t.length > 0 ? t : null;
}

/** The default output file name for an annotation export. */
export function annotationExportName(base: string, format: "json" | "csv"): string {
  const stem = base.replace(/\.[^.]+$/, "") || "annotations";
  return `${stem}.annotations.${format}`;
}

/** A deterministic fallback swatch for a tag with no declared colour, so tag
 * chips stay visually distinct across the palette. */
const TAG_SWATCHES = [
  "#7c3aed",
  "#2563eb",
  "#0d9488",
  "#65a30d",
  "#d97706",
  "#dc2626",
  "#db2777",
  "#0891b2",
];

/** The chip colour for a tag: its declared colour, else a stable palette pick
 * from the tag name so the same tag always looks the same. */
export function tagColor(name: string, declared?: string): string {
  if (declared && declared.trim()) return declared;
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  return TAG_SWATCHES[hash % TAG_SWATCHES.length];
}

/** A short, human relative-ish label for an annotation timestamp (ms epoch). */
export function noteTimeLabel(ms: number): string {
  if (!ms) return "";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
