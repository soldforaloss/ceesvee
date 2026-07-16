// Catalog of the data-cleaning transforms (F06) and the pure builder that
// turns dialog inputs into a validated TransformSpec.

import type { TransformSpec } from "../types";

export type TransformKind = TransformSpec["type"];

export interface TransformParam {
  key: string;
  label: string;
  kind: "text" | "checkbox" | "column" | "columns";
  placeholder?: string;
  defaultValue?: string | boolean;
}

export interface TransformDef {
  type: TransformKind;
  label: string;
  /** Split/merge change the column structure (affects every row's shape). */
  structural: boolean;
  params: TransformParam[];
}

export const TRANSFORMS: TransformDef[] = [
  { type: "trim", label: "Trim whitespace", structural: false, params: [] },
  {
    type: "collapseWhitespace",
    label: "Collapse repeated whitespace",
    structural: false,
    params: [],
  },
  { type: "uppercase", label: "UPPERCASE", structural: false, params: [] },
  { type: "lowercase", label: "lowercase", structural: false, params: [] },
  { type: "titleCase", label: "Title Case", structural: false, params: [] },
  {
    type: "replaceText",
    label: "Replace text",
    structural: false,
    params: [
      { key: "find", label: "Find", kind: "text" },
      { key: "replace", label: "Replace with", kind: "text" },
      { key: "caseSensitive", label: "Case sensitive", kind: "checkbox", defaultValue: true },
    ],
  },
  {
    type: "replaceRegex",
    label: "Replace by regex",
    structural: false,
    params: [
      { key: "pattern", label: "Pattern", kind: "text", placeholder: "e.g. (\\d+)-(\\d+)" },
      { key: "replace", label: "Replace with", kind: "text", placeholder: "e.g. $2/$1" },
    ],
  },
  {
    type: "fillBlank",
    label: "Fill blank cells",
    structural: false,
    params: [{ key: "value", label: "Fill with", kind: "text" }],
  },
  {
    type: "normalizeBooleans",
    label: "Normalize booleans",
    structural: false,
    params: [
      { key: "trueValue", label: "True becomes", kind: "text", defaultValue: "true" },
      { key: "falseValue", label: "False becomes", kind: "text", defaultValue: "false" },
    ],
  },
  {
    type: "normalizeDates",
    label: "Normalize dates",
    structural: false,
    params: [{ key: "format", label: "Output format", kind: "text", defaultValue: "%Y-%m-%d" }],
  },
  {
    type: "normalizeNumbers",
    label: "Normalize numeric separators",
    structural: false,
    params: [
      {
        key: "decimalComma",
        label: "Source uses decimal comma (1.234,56)",
        kind: "checkbox",
        defaultValue: false,
      },
    ],
  },
  {
    type: "addPrefix",
    label: "Add prefix",
    structural: false,
    params: [{ key: "prefix", label: "Prefix", kind: "text" }],
  },
  {
    type: "addSuffix",
    label: "Add suffix",
    structural: false,
    params: [{ key: "suffix", label: "Suffix", kind: "text" }],
  },
  {
    type: "splitByDelimiter",
    label: "Split a column by delimiter",
    structural: true,
    params: [
      { key: "column", label: "Column", kind: "column" },
      { key: "delimiter", label: "Delimiter", kind: "text", defaultValue: "," },
    ],
  },
  {
    type: "splitByRegex",
    label: "Split a column by regex",
    structural: true,
    params: [
      { key: "column", label: "Column", kind: "column" },
      { key: "pattern", label: "Pattern", kind: "text", placeholder: "e.g. \\s+" },
    ],
  },
  {
    type: "mergeColumns",
    label: "Merge columns",
    structural: true,
    params: [
      { key: "columns", label: "Columns (in order)", kind: "columns" },
      { key: "separator", label: "Separator", kind: "text", defaultValue: " " },
    ],
  },
];

export type ParamValues = Record<string, string | boolean | number | number[]>;

/** Default values for a transform's parameters. */
export function defaultValues(def: TransformDef): ParamValues {
  const values: ParamValues = {};
  for (const p of def.params) {
    if (p.kind === "checkbox") values[p.key] = p.defaultValue === true;
    else if (p.kind === "column") values[p.key] = 0;
    else if (p.kind === "columns") values[p.key] = [];
    else values[p.key] = typeof p.defaultValue === "string" ? p.defaultValue : "";
  }
  return values;
}

/** Assemble and validate a spec from dialog inputs. */
export function buildTransformSpec(
  type: TransformKind,
  values: ParamValues,
): TransformSpec | { error: string } {
  const text = (key: string) => String(values[key] ?? "");
  const flag = (key: string) => values[key] === true;
  const column = (key: string) => Number(values[key] ?? 0);

  switch (type) {
    case "trim":
    case "collapseWhitespace":
    case "uppercase":
    case "lowercase":
    case "titleCase":
      return { type };
    case "replaceText":
      if (!text("find")) return { error: "Enter the text to find" };
      return {
        type,
        find: text("find"),
        replace: text("replace"),
        caseSensitive: flag("caseSensitive"),
      };
    case "replaceRegex":
      if (!text("pattern")) return { error: "Enter a pattern" };
      return { type, pattern: text("pattern"), replace: text("replace") };
    case "fillBlank":
      if (!text("value")) return { error: "Enter a fill value" };
      return { type, value: text("value") };
    case "normalizeBooleans":
      return { type, trueValue: text("trueValue"), falseValue: text("falseValue") };
    case "normalizeDates":
      if (!text("format")) return { error: "Enter an output format" };
      return { type, format: text("format") };
    case "normalizeNumbers":
      return { type, decimalComma: flag("decimalComma") };
    case "addPrefix":
      if (!text("prefix")) return { error: "Enter a prefix" };
      return { type, prefix: text("prefix") };
    case "addSuffix":
      if (!text("suffix")) return { error: "Enter a suffix" };
      return { type, suffix: text("suffix") };
    case "splitByDelimiter":
      if (!text("delimiter")) return { error: "Enter a delimiter" };
      return { type, column: column("column"), delimiter: text("delimiter") };
    case "splitByRegex":
      if (!text("pattern")) return { error: "Enter a pattern" };
      return { type, column: column("column"), pattern: text("pattern") };
    case "mergeColumns": {
      const columns = Array.isArray(values.columns) ? (values.columns as number[]) : [];
      if (columns.length < 2) return { error: "Pick at least two columns to merge" };
      return { type, columns, separator: text("separator") };
    }
  }
}
