// Pure helpers for semantic data-type detection (F26): display labels, the
// per-type quick-action catalogue, and applying profile overrides (keyed by
// column NAME so they survive rescans and reopened files) over a report.

import type { ColumnSemantics, SemanticAction, SemanticType } from "../types";

export const SEMANTIC_LABELS: Record<SemanticType, string> = {
  uuid: "UUID",
  email: "Email",
  url: "URL",
  ipv4: "IPv4",
  ipv6: "IPv6",
  json: "JSON",
  percentage: "Percentage",
  currency: "Currency",
  phoneNumber: "Phone number",
  postalCode: "Postal code",
  categorical: "Categorical",
  freeText: "Plain text",
};

export const ACTION_LABELS: Record<SemanticAction, string> = {
  normalize: "Normalize",
  percentToDecimal: "To decimal",
  extractUrlHost: "Extract host",
  extractEmailDomain: "Extract domain",
};

/** Types a user can pick as an override (every type plus "Plain text"). */
export const OVERRIDE_CHOICES: SemanticType[] = [
  "uuid",
  "email",
  "url",
  "ipv4",
  "ipv6",
  "json",
  "percentage",
  "currency",
  "phoneNumber",
  "postalCode",
  "categorical",
  "freeText",
];

/** Per-cell pattern types — the only ones valid/invalid filters apply to. */
const FILTERABLE: ReadonlySet<SemanticType> = new Set([
  "uuid",
  "email",
  "url",
  "ipv4",
  "ipv6",
  "json",
  "percentage",
  "currency",
  "phoneNumber",
  "postalCode",
]);

export function isFilterable(semantic: SemanticType): boolean {
  return FILTERABLE.has(semantic);
}

/**
 * Mutating quick actions available for a type. Phone numbers and postal
 * codes deliberately offer NONE — they must never be numeric-converted.
 */
export function actionsForType(semantic: SemanticType): SemanticAction[] {
  switch (semantic) {
    case "email":
      return ["normalize", "extractEmailDomain"];
    case "uuid":
      return ["normalize"];
    case "url":
      return ["extractUrlHost"];
    case "percentage":
      return ["percentToDecimal"];
    default:
      return [];
  }
}

/** A column's report row with any profile override applied on top. */
export interface EffectiveSemantics extends ColumnSemantics {
  /** What the UI should treat the column as (override beats detection). */
  effective: SemanticType | null;
  overridden: boolean;
}

/**
 * Apply overrides (column NAME → type) over a report's columns. An override
 * always wins — including "freeText", which forces a detected column back to
 * plain text. Detection results are untouched; this is presentation only.
 */
export function applyOverrides(
  columns: ColumnSemantics[],
  headers: string[],
  overrides: [string, SemanticType][],
): EffectiveSemantics[] {
  const byName = new Map(overrides);
  return columns.map((c) => {
    const name = headers[c.column] ?? "";
    const override = name === "" ? undefined : byName.get(name);
    return {
      ...c,
      effective: override ?? c.detected,
      overridden: override !== undefined,
    };
  });
}

/**
 * Insert, replace, or remove (type = null) one column's override in a
 * profile's list, returning a new list.
 */
export function upsertOverride(
  overrides: [string, SemanticType][],
  column: string,
  semantic: SemanticType | null,
): [string, SemanticType][] {
  const rest = overrides.filter(([name]) => name !== column);
  return semantic === null ? rest : [...rest, [column, semantic]];
}
