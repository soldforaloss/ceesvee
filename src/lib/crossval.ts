// Pure helpers for cross-column validation (F27): rule descriptions (mirrors
// the Rust `describe`), blank rule factories for the builder UI, and shape
// checks that catch invalid configurations before they reach the backend.

import type { CrossRule, CompareOp } from "../types";

export const RULE_TYPE_LABELS: Record<CrossRule["type"], string> = {
  columnsEqual: "Columns equal / differ",
  numericCompare: "Numeric comparison",
  dateOrder: "Date order",
  conditionalRequired: "Conditional required",
  exactlyOne: "Exactly one populated",
  atLeastOne: "At least one populated",
  atMostOne: "Mutually exclusive",
  sumEquals: "Sum equality",
  allowedCombinations: "Allowed combinations",
};

export const COMPARE_OP_LABELS: Record<CompareOp, string> = {
  lt: "<",
  le: "≤",
  gt: ">",
  ge: "≥",
  eq: "=",
  ne: "≠",
};

/** Human one-liner for a rule (mirrors the backend's `describe`). */
export function describeRule(rule: CrossRule): string {
  switch (rule.type) {
    case "columnsEqual":
      return `"${rule.left}" must ${rule.negate ? "differ from" : "equal"} "${rule.right}"`;
    case "numericCompare":
      return `"${rule.left}" ${COMPARE_OP_LABELS[rule.op]} "${rule.right}" (numeric)`;
    case "dateOrder":
      return `"${rule.earlier}" must be ${rule.allowEqual ? "on or before" : "before"} "${rule.later}"`;
    case "conditionalRequired": {
      const cond =
        rule.when.type === "equals"
          ? `= "${rule.when.value}"`
          : rule.when.type === "nonBlank"
            ? "is not blank"
            : "is blank";
      return `when "${rule.whenColumn}" ${cond}, "${rule.thenRequired}" is required`;
    }
    case "exactlyOne":
      return `exactly one of ${rule.columns.join(", ")} populated`;
    case "atLeastOne":
      return `at least one of ${rule.columns.join(", ")} populated`;
    case "atMostOne":
      return `at most one of ${rule.columns.join(", ")} populated`;
    case "sumEquals":
      return `${rule.parts.join(" + ")} must sum to "${rule.total}" (±${rule.tolerance}${rule.tolerancePercent ? "%" : ""})`;
    case "allowedCombinations":
      return `(${rule.columns.join(", ")}) must be one of ${rule.allowed.length} allowed combination${rule.allowed.length === 1 ? "" : "s"}`;
  }
}

/** A blank rule of the given type, seeded with the first columns. */
export function emptyRule(type: CrossRule["type"], headers: string[]): CrossRule {
  const a = headers[0] ?? "";
  const b = headers[1] ?? headers[0] ?? "";
  switch (type) {
    case "columnsEqual":
      return { type, left: a, right: b, negate: false };
    case "numericCompare":
      return { type, left: a, op: "le", right: b };
    case "dateOrder":
      return { type, earlier: a, later: b, allowEqual: false };
    case "conditionalRequired":
      return { type, whenColumn: a, when: { type: "nonBlank" }, thenRequired: b };
    case "exactlyOne":
    case "atLeastOne":
    case "atMostOne":
      return { type, columns: [a, b] };
    case "sumEquals":
      return { type, parts: [a], total: b, tolerance: 0, tolerancePercent: false };
    case "allowedCombinations":
      return { type, columns: [a], allowed: [] };
  }
}

/**
 * Shape validation mirroring the backend's `validate_rules` — returns a
 * problem description or null when the rule can be submitted.
 */
export function ruleProblem(rule: CrossRule): string | null {
  switch (rule.type) {
    case "columnsEqual":
    case "numericCompare":
      return rule.left === rule.right ? "pick two different columns" : null;
    case "dateOrder":
      return rule.earlier === rule.later ? "pick two different columns" : null;
    case "exactlyOne":
    case "atLeastOne":
    case "atMostOne":
      return rule.columns.length < 2 ? "pick at least two columns" : null;
    case "sumEquals":
      if (rule.parts.length === 0) return "pick at least one part column";
      if (!Number.isFinite(rule.tolerance) || rule.tolerance < 0)
        return "tolerance must be a non-negative number";
      return null;
    case "allowedCombinations":
      if (rule.columns.length === 0) return "pick at least one column";
      if (rule.allowed.length === 0) return "add at least one allowed combination";
      if (rule.allowed.some((row) => row.length !== rule.columns.length))
        return "every combination needs one value per column";
      return null;
    case "conditionalRequired":
      return null;
  }
}

/**
 * Parse an "allowed combinations" textarea: one comma-separated combination
 * per line, values trimmed, blank lines skipped.
 */
export function parseCombinations(text: string): string[][] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== "")
    .map((line) => line.split(",").map((v) => v.trim()));
}
