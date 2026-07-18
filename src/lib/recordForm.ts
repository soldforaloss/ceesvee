// Pure, UI-agnostic helpers for the F41 record form: the draft reducer, the
// visible-record navigation mapping, save-gating, and layout sectioning. Kept
// separate from the React component so they can be unit-tested in isolation
// (see recordForm.test.ts) and reused by the store.

import type {
  DraftField,
  DraftValidation,
  RecordField,
  RecordFieldGroup,
  RecordLayout,
} from "../types";

/** A record-form draft: drafted raw text keyed by grid column position. */
export type RecordDraft = Record<number, string>;

/**
 * The stored (baseline) raw value for a field. A field absent from a ragged
 * short row still has `raw === ""` from the backend, so this is total.
 */
function storedValue(field: RecordField): string {
  return field.raw;
}

/**
 * Whether one field's draft differs from its stored value. A draft entry that
 * equals the stored text (typed then reverted) is NOT a change — so the null
 * token "N/A" and an empty string are correctly told apart, but "N/A" over a
 * stored "N/A" is not a change.
 */
export function fieldChanged(field: RecordField, draft: RecordDraft): boolean {
  if (!(field.col in draft)) return false;
  return draft[field.col] !== storedValue(field);
}

/** The effective value shown/edited for a field: its draft, else stored raw. */
export function fieldValue(field: RecordField, draft: RecordDraft): string {
  return field.col in draft ? draft[field.col] : storedValue(field);
}

/** The changed fields of a draft, as {col, value} — the pre-check payload. */
export function changedFields(fields: RecordField[], draft: RecordDraft): DraftField[] {
  return fields
    .filter((f) => fieldChanged(f, draft))
    .map((f) => ({ col: f.col, value: draft[f.col] }));
}

/** Whether any field's draft differs from its stored value. */
export function isDraftDirty(fields: RecordField[], draft: RecordDraft): boolean {
  return fields.some((f) => fieldChanged(f, draft));
}

/**
 * Clamp a target record index into the visible range. Returns `null` when there
 * are no visible records (an empty or fully-filtered document).
 */
export function clampRecord(target: number, visibleLen: number): number | null {
  if (visibleLen <= 0) return null;
  if (target < 0) return 0;
  if (target > visibleLen - 1) return visibleLen - 1;
  return Math.floor(target);
}

/**
 * Step `delta` visible records from `current`, clamped to the range. Returns
 * `null` if there is nowhere to go (already at the edge, or no records), so the
 * caller can leave the position untouched and disable the button.
 */
export function stepRecord(current: number, delta: number, visibleLen: number): number | null {
  const next = clampRecord(current + delta, visibleLen);
  if (next === null || next === current) return null;
  return next;
}

/**
 * Parse a 1-based go-to-record input into a 0-based clamped display row.
 * Returns `null` for blank / non-numeric input or when there are no records.
 */
export function parseGoto(input: string, visibleLen: number): number | null {
  const trimmed = input.trim();
  if (trimmed === "") return null;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || !Number.isInteger(n)) return null;
  return clampRecord(n - 1, visibleLen);
}

/**
 * Whether a draft save must be blocked: any strict column carries an invalid
 * value. Mirrors exactly what the backend `apply_validated_cells` would reject,
 * so the Save button and a real commit never disagree.
 */
export function saveBlocked(validation: DraftValidation | null): boolean {
  return validation?.strictBlocks ?? false;
}

/** One rendered section of the form: a named group, or the default (ungrouped). */
export interface RecordSection {
  /** The group, or `null` for the implicit "everything else" section. */
  group: RecordFieldGroup | null;
  fields: RecordField[];
}

/**
 * Order the fields into sections under a layout: hidden fields removed, grouped
 * fields placed in their group (in the group's declared column order), and
 * every remaining field kept in schema order in a trailing default section.
 * A `null` layout yields a single default section in schema order (automatic).
 *
 * Robust to drift: a group's column ID that no longer maps to a field is
 * skipped, and a field whose group was deleted falls back to the default
 * section — so a stale persisted layout never drops or duplicates a field.
 */
export function layoutSections(
  fields: RecordField[],
  layout: RecordLayout | null,
): RecordSection[] {
  if (!layout) return [{ group: null, fields }];

  const hidden = new Set(layout.hiddenColumnIds);
  const visible = fields.filter((f) => !hidden.has(f.columnId));
  const byId = new Map(visible.map((f) => [f.columnId, f]));

  const claimed = new Set<string>();
  const sections: RecordSection[] = [];
  for (const group of layout.groups) {
    const groupFields: RecordField[] = [];
    for (const id of group.columnIds) {
      const field = byId.get(id);
      if (field && !claimed.has(id)) {
        groupFields.push(field);
        claimed.add(id);
      }
    }
    sections.push({ group, fields: groupFields });
  }

  const rest = visible.filter((f) => !claimed.has(f.columnId));
  if (rest.length > 0 || sections.length === 0) {
    sections.push({ group: null, fields: rest });
  }
  return sections;
}

/** Whether a field is hidden under a layout. */
export function isFieldHidden(columnId: string, layout: RecordLayout | null): boolean {
  return !!layout && layout.hiddenColumnIds.includes(columnId);
}

/** The layout to persist after toggling one field's hidden state. */
export function toggleHidden(layout: RecordLayout | null, columnId: string): RecordLayout {
  const base = layout ?? emptyLayout();
  const hiddenColumnIds = base.hiddenColumnIds.includes(columnId)
    ? base.hiddenColumnIds.filter((id) => id !== columnId)
    : [...base.hiddenColumnIds, columnId];
  return { ...base, hiddenColumnIds };
}

/** Assign a field to a group (or to the default section when `groupId` null). */
export function assignToGroup(
  layout: RecordLayout | null,
  columnId: string,
  groupId: string | null,
): RecordLayout {
  const base = layout ?? emptyLayout();
  const groups = base.groups.map((g) => ({
    ...g,
    columnIds: g.columnIds.filter((id) => id !== columnId),
  }));
  if (groupId !== null) {
    const target = groups.find((g) => g.id === groupId);
    if (target) target.columnIds = [...target.columnIds, columnId];
  }
  return { ...base, groups };
}

/** The layout after adding a new, empty group with the given name. */
export function addGroup(layout: RecordLayout | null, id: string, name: string): RecordLayout {
  const base = layout ?? emptyLayout();
  return { ...base, groups: [...base.groups, { id, name, columnIds: [] }] };
}

/** The layout after removing a group (its fields fall back to the default). */
export function removeGroup(layout: RecordLayout | null, groupId: string): RecordLayout {
  const base = layout ?? emptyLayout();
  return { ...base, groups: base.groups.filter((g) => g.id !== groupId) };
}

/** A fresh, empty layout (comfortable density, nothing hidden, no groups). */
export function emptyLayout(): RecordLayout {
  return { density: "comfortable", hiddenColumnIds: [], groups: [] };
}
