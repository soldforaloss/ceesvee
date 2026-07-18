// Pure helpers for the data dictionary (F38): field labels and option
// catalogues, the per-column completeness calculation, payload normalisation,
// and the conflict-reduction logic that turns the user's per-field
// existing-vs-incoming choices into the `MergeResolution` the apply command
// expects. Kept free of React and Tauri so the load-bearing logic is unit
// testable in isolation (the dialog only renders this state).
//
// `fieldValue` MIRRORS the Rust `dictionary::value_of`: a field is "present"
// only when it carries a trimmed, non-blank value, so completeness and the
// documented flag agree with the backend that keys merges and profile rules.

import type {
  ConflictChoice,
  DictionaryField,
  DictionaryFieldKey,
  FieldConflict,
  FieldResolution,
  FieldRole,
  MergeMatchBy,
  MergePlan,
  MergeResolution,
  Sensitivity,
} from "../types";

// ---------------------------------------------------------------------------
// Labels and option catalogues
// ---------------------------------------------------------------------------

/** Every documentable field, in stable presentation order (mirrors Rust). */
export const DICTIONARY_FIELD_KEYS: DictionaryFieldKey[] = [
  "displayName",
  "description",
  "role",
  "unit",
  "source",
  "sensitivity",
  "allowedValues",
  "example",
  "owner",
  "notes",
];

/** Human labels for the ten documentation fields. */
export const FIELD_KEY_LABELS: Record<DictionaryFieldKey, string> = {
  displayName: "Display name",
  description: "Description",
  role: "Role",
  unit: "Unit",
  source: "Source",
  sensitivity: "Sensitivity",
  allowedValues: "Allowed values",
  example: "Example",
  owner: "Owner",
  notes: "Notes",
};

/** The five analytical roles, in editor order. */
export const ROLE_OPTIONS: FieldRole[] = [
  "identifier",
  "dimension",
  "measure",
  "timestamp",
  "label",
];

export const ROLE_LABELS: Record<FieldRole, string> = {
  identifier: "Identifier",
  dimension: "Dimension",
  measure: "Measure",
  timestamp: "Timestamp",
  label: "Label",
};

/** The four sensitivity classes, least → most sensitive. */
export const SENSITIVITY_OPTIONS: Sensitivity[] = [
  "public",
  "internal",
  "confidential",
  "restricted",
];

export const SENSITIVITY_LABELS: Record<Sensitivity, string> = {
  public: "Public",
  internal: "Internal",
  confidential: "Confidential",
  restricted: "Restricted",
};

/** How imported entries are matched to current columns. */
export const MATCH_BY_OPTIONS: { value: MergeMatchBy; label: string }[] = [
  { value: "auto", label: "Auto — column ID, then name" },
  { value: "columnId", label: "Column ID only" },
  { value: "columnName", label: "Column name only" },
];

/**
 * The match mode an apply MUST run under: the one recorded on the plan the user
 * reviewed, NOT the live `Match by` dropdown. Preview is async, so between the
 * plan being computed and the user pressing Apply the selector can change (or a
 * stale in-flight preview can land) — applying under the live selection would
 * merge under a different mode than the previewed conflicts/counts and could
 * touch a different set of columns. Threading the plan's own `matchBy` keeps the
 * apply consistent with the displayed plan (the same principle that already
 * guards the apply with `plan.dictionaryRevision`). Taking only the plan makes
 * it structurally impossible to read live UI state here.
 */
export function applyMatchBy(plan: Pick<MergePlan, "matchBy">): MergeMatchBy {
  return plan.matchBy;
}

/**
 * Whether a sensitivity classification makes a column PII-relevant (mirrors the
 * Rust `Sensitivity::is_sensitive`): confidential or restricted feed the F28
 * PII preflight regardless of pattern hits.
 */
export function isSensitive(sensitivity: Sensitivity | undefined): boolean {
  return sensitivity === "confidential" || sensitivity === "restricted";
}

// ---------------------------------------------------------------------------
// Presence, completeness (mirrors `dictionary::value_of` / `is_documented`)
// ---------------------------------------------------------------------------

/**
 * Canonical (trimmed, non-blank) display value of one field, or `null` when it
 * carries no real documentation. Drives presence, completeness and the
 * documented flag uniformly across the differently-typed fields.
 */
export function fieldValue(field: DictionaryField, key: DictionaryFieldKey): string | null {
  const text = (value: string | undefined): string | null => {
    const trimmed = (value ?? "").trim();
    return trimmed === "" ? null : trimmed;
  };
  switch (key) {
    case "displayName":
      return text(field.displayName);
    case "description":
      return text(field.description);
    case "role":
      return field.role ?? null;
    case "unit":
      return text(field.unit);
    case "source":
      return text(field.source);
    case "sensitivity":
      return field.sensitivity ?? null;
    case "allowedValues": {
      const vals = (field.allowedValues ?? []).map((v) => v.trim()).filter((v) => v !== "");
      return vals.length > 0 ? vals.join(", ") : null;
    }
    case "example":
      return text(field.example);
    case "owner":
      return text(field.owner);
    case "notes":
      return text(field.notes);
  }
}

/** Whether `field` populates `key` with a real (non-blank) value. */
export function isFieldPresent(field: DictionaryField, key: DictionaryFieldKey): boolean {
  return fieldValue(field, key) !== null;
}

/** How many of the ten documentation fields carry a real value. */
export function filledFieldCount(field: DictionaryField): number {
  return DICTIONARY_FIELD_KEYS.reduce((n, key) => n + (isFieldPresent(field, key) ? 1 : 0), 0);
}

export interface Completeness {
  filled: number;
  total: number;
  /** `filled / total` in [0, 1]. */
  fraction: number;
}

/** Per-column completeness: filled fields out of the ten documentable ones. */
export function completeness(field: DictionaryField): Completeness {
  const total = DICTIONARY_FIELD_KEYS.length;
  const filled = filledFieldCount(field);
  return { filled, total, fraction: total === 0 ? 0 : filled / total };
}

/** Whether any documentation field carries a real value. */
export function isDocumented(field: DictionaryField): boolean {
  return filledFieldCount(field) > 0;
}

/**
 * Trim every string field and drop blank allowed values before the entry is
 * sent to the backend, so a field of whitespace never counts as documented.
 * Blank strings become `undefined` (omitted on the wire → `Option::None`).
 */
export function normalizeField(field: DictionaryField): DictionaryField {
  const opt = (value: string | undefined): string | undefined => {
    const trimmed = (value ?? "").trim();
    return trimmed === "" ? undefined : trimmed;
  };
  const allowed = (field.allowedValues ?? []).map((v) => v.trim()).filter((v) => v !== "");
  return {
    columnId: field.columnId,
    displayName: opt(field.displayName),
    description: opt(field.description),
    role: field.role,
    unit: opt(field.unit),
    source: opt(field.source),
    sensitivity: field.sensitivity,
    allowedValues: allowed,
    example: opt(field.example),
    owner: opt(field.owner),
    notes: opt(field.notes),
  };
}

// ---------------------------------------------------------------------------
// Import conflict reduction
// ---------------------------------------------------------------------------

/** Stable map key for one conflict (column ID + field). */
export function conflictKey(columnId: string, field: DictionaryFieldKey): string {
  return `${columnId}\u0000${field}`;
}

/** The user's per-conflict choices, keyed by {@link conflictKey}. */
export type ConflictChoices = Record<string, ConflictChoice>;

/** How many reported conflicts still lack an explicit choice. */
export function unresolvedCount(conflicts: FieldConflict[], choices: ConflictChoices): number {
  return conflicts.filter((c) => choices[conflictKey(c.columnId, c.field)] == null).length;
}

/** Whether every reported conflict has an explicit choice. */
export function allConflictsResolved(
  conflicts: FieldConflict[],
  choices: ConflictChoices,
): boolean {
  return unresolvedCount(conflicts, choices) === 0;
}

/**
 * Reduce the reported conflicts + the user's choices into an explicit per-field
 * `MergeResolution`. Only choices that still correspond to a reported conflict
 * are emitted (stale keys are dropped), and the field order follows the plan's
 * conflict order so the payload is deterministic. Conflicts left without a
 * choice are simply omitted — the backend then rejects the apply, which is the
 * safety guarantee the caller relies on (never silently drop a conflict).
 */
export function buildPerFieldResolution(
  conflicts: FieldConflict[],
  choices: ConflictChoices,
): MergeResolution {
  const resolutions: FieldResolution[] = [];
  for (const c of conflicts) {
    const choice = choices[conflictKey(c.columnId, c.field)];
    if (choice != null) {
      resolutions.push({ columnId: c.columnId, field: c.field, choice });
    }
  }
  return { type: "perField", resolutions };
}

/** Apply one bulk choice to every reported conflict (the "keep all" / "take
 * all incoming" shortcuts, expressed as explicit per-field choices). */
export function bulkChoices(conflicts: FieldConflict[], choice: ConflictChoice): ConflictChoices {
  const out: ConflictChoices = {};
  for (const c of conflicts) out[conflictKey(c.columnId, c.field)] = choice;
  return out;
}
