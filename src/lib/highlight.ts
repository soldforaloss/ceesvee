// Pure helpers for conditional highlighting (F42): the rule/condition model in
// display terms, validation that mirrors the Rust `validate_rule`, the
// deterministic priority ordering used both for display and to explain
// overlaps, and the SEMANTIC tone → colour token mapping shared by the grid
// paint (see `gridTheme.ts`) and the dialog swatches. No React, no store, no
// invoke — everything here is unit-testable.

import type {
  HighlightCondition,
  HighlightDecoration,
  HighlightEmphasis,
  HighlightRule,
  HighlightTarget,
  HighlightTextStyle,
  HighlightTone,
} from "../types";

// ----- ids -------------------------------------------------------------------

let ruleSeq = 0;

/** Unique, persistence-safe rule id (stable tie-break key for priority). */
export function newHighlightId(): string {
  ruleSeq += 1;
  return `hl-${Date.now().toString(36)}-${ruleSeq}${Math.random().toString(36).slice(2, 6)}`;
}

// ----- semantic token mapping ------------------------------------------------

/** The base RGB for each semantic tone. Never persisted — a rule stores the
 *  tone, and this maps it to a readable tint for whichever theme is active. */
export const HIGHLIGHT_TONE_RGB: Record<HighlightTone, [number, number, number]> = {
  accent: [139, 92, 246], // violet-500
  info: [14, 165, 233], // sky-500
  warn: [217, 119, 6], // amber-600 (darker for light-mode contrast)
  error: [239, 68, 68], // red-500
  success: [22, 163, 74], // green-600
  neutral: [113, 113, 122], // zinc-500
};

// Background opacity per emphasis. Dark mode needs a touch more to read over
// the near-black cell background; both stay translucent so the cell text
// underneath keeps its normal (theme-provided) contrast.
const EMPHASIS_ALPHA_LIGHT: Record<HighlightEmphasis, number> = {
  subtle: 0.12,
  normal: 0.2,
  strong: 0.34,
};
const EMPHASIS_ALPHA_DARK: Record<HighlightEmphasis, number> = {
  subtle: 0.18,
  normal: 0.28,
  strong: 0.44,
};

/** The translucent cell background for a tone + emphasis on the active theme. */
export function highlightBackground(
  tone: HighlightTone,
  emphasis: HighlightEmphasis,
  dark: boolean,
): string {
  const [r, g, b] = HIGHLIGHT_TONE_RGB[tone];
  const alpha = (dark ? EMPHASIS_ALPHA_DARK : EMPHASIS_ALPHA_LIGHT)[emphasis];
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/** A solid, theme-appropriate accent for swatches / badges / icons. Dark mode
 *  lightens the tone so it reads against a dark surface. */
export function highlightAccent(tone: HighlightTone, dark: boolean): string {
  const [r, g, b] = HIGHLIGHT_TONE_RGB[tone];
  if (!dark) return `rgb(${r}, ${g}, ${b})`;
  const lift = (c: number) => Math.round(c + (255 - c) * 0.35);
  return `rgb(${lift(r)}, ${lift(g)}, ${lift(b)})`;
}

/** The CSS font-weight/style for a decoration's text style override. */
export function highlightFontStyle(style: HighlightTextStyle): string | undefined {
  switch (style) {
    case "bold":
      return "600 13px";
    case "italic":
      return "italic 13px";
    default:
      return undefined;
  }
}

// ----- labels for the editor -------------------------------------------------

export const TONE_LABELS: Record<HighlightTone, string> = {
  accent: "Accent",
  info: "Info",
  warn: "Warning",
  error: "Error",
  success: "Success",
  neutral: "Neutral",
};

export const TONE_ORDER: HighlightTone[] = [
  "accent",
  "info",
  "success",
  "warn",
  "error",
  "neutral",
];

export const EMPHASIS_LABELS: Record<HighlightEmphasis, string> = {
  subtle: "Subtle",
  normal: "Normal",
  strong: "Strong",
};

export const TEXT_STYLE_LABELS: Record<HighlightTextStyle, string> = {
  normal: "Normal",
  bold: "Bold",
  italic: "Italic",
};

export type ConditionKind = HighlightCondition["type"];

export const CONDITION_LABELS: Record<ConditionKind, string> = {
  equals: "Equals",
  notEquals: "Does not equal",
  contains: "Contains",
  regex: "Matches regex",
  numericRange: "Numeric range",
  dateRange: "Date range",
  blank: "Blank / null",
  invalid: "Invalid for type",
  duplicate: "Duplicate value",
  diagnostic: "Diagnostic issue",
  crossColumn: "Cross-column violation",
  outlier: "Statistical outlier",
  changedSinceSave: "Changed since save",
  bookmarked: "Bookmarked",
  flagged: "Flagged",
  tagged: "Tagged",
};

/** Grouped condition kinds for the editor's type picker. */
export const CONDITION_GROUPS: { label: string; kinds: ConditionKind[] }[] = [
  { label: "Value", kinds: ["equals", "notEquals", "contains", "regex"] },
  { label: "Range", kinds: ["numericRange", "dateRange"] },
  { label: "Quality", kinds: ["blank", "invalid", "duplicate", "changedSinceSave"] },
  { label: "Analysis", kinds: ["diagnostic", "crossColumn", "outlier"] },
  { label: "Annotations (F40)", kinds: ["bookmarked", "flagged", "tagged"] },
];

/** Conditions that carry an optional single-column scope. */
const COLUMN_SCOPED: ReadonlySet<ConditionKind> = new Set<ConditionKind>([
  "equals",
  "notEquals",
  "contains",
  "regex",
  "numericRange",
  "dateRange",
  "blank",
  "invalid",
  "duplicate",
  "changedSinceSave",
]);

/** Whether a condition kind carries an optional `columnId` scope. */
export function conditionSupportsColumn(kind: ConditionKind): boolean {
  return COLUMN_SCOPED.has(kind);
}

/** Row-annotation conditions (F40) — modelled but not yet wired: they match
 *  nothing until row annotations land beneath this feature. */
const RESERVED: ReadonlySet<ConditionKind> = new Set<ConditionKind>([
  "bookmarked",
  "flagged",
  "tagged",
]);

/** Whether a condition is a reserved (currently-unavailable) F40 stub. */
export function conditionReserved(kind: ConditionKind): boolean {
  return RESERVED.has(kind);
}

/** Analysis-backed conditions that read a cached scan (empty until it runs). */
const ANALYSIS_BACKED: ReadonlySet<ConditionKind> = new Set<ConditionKind>([
  "diagnostic",
  "crossColumn",
  "outlier",
]);

export function conditionAnalysisBacked(kind: ConditionKind): boolean {
  return ANALYSIS_BACKED.has(kind);
}

// ----- factories -------------------------------------------------------------

/** The current column scope of a condition, or undefined if it has none / is
 *  unscoped ("any column"). */
export function conditionColumnId(condition: HighlightCondition): string | undefined {
  if ("columnId" in condition && condition.columnId) return condition.columnId;
  return undefined;
}

/** A fresh condition of the given kind, defaulting its fields. */
export function defaultCondition(kind: ConditionKind): HighlightCondition {
  switch (kind) {
    case "equals":
    case "notEquals":
    case "contains":
      return { type: kind, columnId: null, value: "", caseSensitive: false };
    case "regex":
      return { type: "regex", columnId: null, pattern: "", caseSensitive: false };
    case "numericRange":
      return { type: "numericRange", columnId: null, min: null, max: null, inclusive: true };
    case "dateRange":
      return { type: "dateRange", columnId: null, min: null, max: null };
    case "blank":
    case "invalid":
    case "changedSinceSave":
      return { type: kind, columnId: null };
    case "duplicate":
      return {
        type: "duplicate",
        columnId: null,
        trim: true,
        caseInsensitive: false,
        collapseWhitespace: false,
      };
    case "diagnostic":
      return { type: "diagnostic", issueId: null };
    case "crossColumn":
      return { type: "crossColumn", ruleIndex: null };
    case "outlier":
      return { type: "outlier" };
    case "bookmarked":
      return { type: "bookmarked" };
    case "flagged":
      return { type: "flagged", label: null };
    case "tagged":
      return { type: "tagged", tag: "" };
  }
}

/** A new rule with sensible defaults (a value-equals cell rule). `priority`
 *  is placed just above the current maximum so a fresh rule wins by default. */
export function newHighlightRule(existing: HighlightRule[]): HighlightRule {
  const maxPriority = existing.reduce((m, r) => Math.max(m, r.priority), 0);
  return {
    id: newHighlightId(),
    name: "",
    condition: defaultCondition("equals"),
    target: { type: "cell" },
    priority: maxPriority + 1,
    decoration: { tone: "accent", emphasis: "normal", icon: null, textStyle: "normal" },
    enabled: true,
  };
}

// ----- priority ordering (mirrors the Rust winning order) --------------------

/**
 * Rules in WINNING order: priority descending, then id ascending as a stable,
 * deterministic tie-break — identical to the backend's `winning_order`, so the
 * dialog's ordering and the "explain" popover agree with what the grid paints.
 * Returns a new array; the input is not mutated.
 */
export function orderRulesByPriority(rules: HighlightRule[]): HighlightRule[] {
  return [...rules].sort(
    (a, b) => b.priority - a.priority || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );
}

// ----- validation (mirrors the Rust validate_rule) ---------------------------

/**
 * Validate a rule the way the backend will before it is stored, so the editor
 * can surface regex-compile and range errors inline instead of on save.
 * Returns a human-readable message, or null when the rule is acceptable.
 * Unknown column ids are tolerated (they simply match nothing), matching the
 * backend — a saved rule survives a column rename until it is re-pointed.
 */
export function validateHighlightRule(rule: HighlightRule): string | null {
  if (!rule.id.trim()) return "Rule id must not be empty";
  const c = rule.condition;
  switch (c.type) {
    case "regex": {
      if (!c.pattern.trim()) return "Enter a regular expression";
      try {
        // Compile with the same case flag the backend will use.
        void new RegExp(c.pattern, c.caseSensitive ? "" : "i");
      } catch (e) {
        return `Invalid regular expression: ${e instanceof Error ? e.message : String(e)}`;
      }
      break;
    }
    case "numericRange": {
      const { min, max } = c;
      if (min != null && !Number.isFinite(min)) return "Minimum must be a finite number";
      if (max != null && !Number.isFinite(max)) return "Maximum must be a finite number";
      if (min != null && max != null && min > max) return "Minimum must be ≤ maximum";
      if (min == null && max == null) return "Set a minimum, a maximum, or both";
      break;
    }
    case "dateRange": {
      const lo = parseDateBound(c.min);
      const hi = parseDateBound(c.max);
      if (lo === "invalid") return "Minimum is not a recognised date";
      if (hi === "invalid") return "Maximum is not a recognised date";
      if (typeof lo === "number" && typeof hi === "number" && lo > hi)
        return "Minimum must be ≤ maximum";
      if (lo === "empty" && hi === "empty") return "Set an earliest date, a latest date, or both";
      break;
    }
    case "tagged":
      if (!c.tag.trim()) return "Enter a tag";
      break;
    case "equals":
    case "notEquals":
    case "contains":
      if (!c.value) return "Enter a value to match";
      break;
    default:
      break;
  }
  if (rule.target.type === "columns" && rule.target.columnIds.length === 0) {
    return "Pick at least one column for a columns target";
  }
  return null;
}

/** Parse a date bound like the validator needs: "empty", "invalid", or ms. */
function parseDateBound(raw: string | null | undefined): "empty" | "invalid" | number {
  const t = (raw ?? "").trim();
  if (!t) return "empty";
  const ms = Date.parse(t);
  return Number.isNaN(ms) ? "invalid" : ms;
}

// ----- description -----------------------------------------------------------

/** A short human summary of a condition for the rule list. `nameFor` maps a
 *  column id to a display header (falls back to the id). */
export function describeCondition(
  condition: HighlightCondition,
  nameFor: (columnId: string) => string,
): string {
  const col = conditionColumnId(condition);
  const where = col ? ` in ${nameFor(col)}` : "";
  switch (condition.type) {
    case "equals":
      return `= "${condition.value}"${where}`;
    case "notEquals":
      return `≠ "${condition.value}"${where}`;
    case "contains":
      return `contains "${condition.value}"${where}`;
    case "regex":
      return `matches /${condition.pattern}/${where}`;
    case "numericRange":
      return `${rangeText(condition.min ?? null, condition.max ?? null)}${where}`;
    case "dateRange":
      return `${rangeText(condition.min ?? null, condition.max ?? null)}${where}`;
    case "blank":
      return `is blank / null${where}`;
    case "invalid":
      return `is invalid for its type${where}`;
    case "duplicate":
      return `is a duplicate value${where}`;
    case "changedSinceSave":
      return `changed since save${where}`;
    case "diagnostic":
      return condition.issueId ? `diagnostic: ${condition.issueId}` : "any diagnostic issue";
    case "crossColumn":
      return condition.ruleIndex != null
        ? `cross-column rule #${condition.ruleIndex + 1}`
        : "any cross-column violation";
    case "outlier":
      return "flagged as an outlier";
    case "bookmarked":
      return "bookmarked (F40)";
    case "flagged":
      return condition.label ? `flagged "${condition.label}" (F40)` : "flagged (F40)";
    case "tagged":
      return `tagged "${condition.tag}" (F40)`;
  }
}

function rangeText(min: number | string | null, max: number | string | null): string {
  if (min != null && max != null) return `between ${min} and ${max}`;
  if (min != null) return `≥ ${min}`;
  if (max != null) return `≤ ${max}`;
  return "any value";
}

/** A one-line summary of a rule's target. */
export function describeTarget(target: HighlightTarget): string {
  switch (target.type) {
    case "cell":
      return "matched cell";
    case "row":
      return "whole row";
    case "columns":
      return `${target.columnIds.length} column${target.columnIds.length === 1 ? "" : "s"}`;
  }
}

/** Build the decoration object with one field replaced (immutably). */
export function withDecoration(
  decoration: HighlightDecoration,
  patch: Partial<HighlightDecoration>,
): HighlightDecoration {
  return { ...decoration, ...patch };
}

// ----- debounced draft persistence ------------------------------------------

/** A debounced writer for the in-progress rule edit. The dialog persists a
 *  draft on a short debounce for a live grid preview, but a pending write must
 *  never be lost when the user switches rules or closes the dialog inside the
 *  debounce window — {@link DraftPersister.flush} commits it synchronously. */
export interface DraftPersister<T> {
  /** (Re)schedule a debounced persist of `draft`, replacing any pending one. */
  schedule(draft: T): void;
  /** Persist the latest scheduled draft immediately, if one is still pending. */
  flush(): void;
  /** Drop the pending draft without persisting it (e.g. its rule was deleted). */
  cancel(): void;
}

/**
 * A debounced draft writer that never silently drops a pending edit.
 *
 * `schedule` (re)arms a timer; when it fires — or when `flush` is called first,
 * on rule-switch or dialog-close — the latest scheduled draft is written iff
 * `shouldPersist` still accepts it (re-evaluated at write time against current
 * state, so an edit reverted back to its stored value, or made invalid, is a
 * no-op). `cancel` discards the pending draft outright.
 */
export function createDraftPersister<T>(opts: {
  delayMs: number;
  persist: (draft: T) => void;
  shouldPersist: (draft: T) => boolean;
}): DraftPersister<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: { draft: T } | null = null;

  const clearTimer = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  const write = () => {
    clearTimer();
    if (pending === null) return;
    const { draft } = pending;
    pending = null;
    if (opts.shouldPersist(draft)) opts.persist(draft);
  };

  return {
    schedule(draft) {
      pending = { draft };
      clearTimer();
      timer = setTimeout(write, opts.delayMs);
    },
    flush: write,
    cancel() {
      clearTimer();
      pending = null;
    },
  };
}
