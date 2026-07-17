// Pure, framework-free helpers for the JSON / JSON Lines import flow (F33).
// The path escaping mirrors the Rust `json_import` module EXACTLY so the paths
// the preview shows are the same ones the engine flattens to and the export
// stage rebuilds from. Everything here is synchronous and side-effect free so
// it can be unit-tested without a backend.

import type {
  ArrayFieldInfo,
  DetectedShape,
  JsonImportOptions,
  JsonImportPreview,
  PreviewColumn,
} from "../types";

/**
 * Escape one object key for use as a flattened path segment: `.` → `\.` and
 * `\` → `\\`. Mirrors `json_import::escape_key`.
 */
export function escapeJsonKey(key: string): string {
  if (!/[.\\]/.test(key)) return key;
  let out = "";
  for (const c of key) {
    if (c === "." || c === "\\") out += "\\";
    out += c;
  }
  return out;
}

/**
 * Split a flattened path back into its original key segments, undoing
 * {@link escapeJsonKey}. Mirrors `json_import::split_path` (including its edge
 * cases: `""` → `[""]`, a trailing lone `\` is dropped).
 */
export function splitJsonPath(path: string): string[] {
  const segments = [""];
  const chars = Array.from(path);
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i];
    if (c === "\\") {
      const escaped = chars[i + 1];
      if (escaped !== undefined) {
        segments[segments.length - 1] += escaped;
        i++;
      }
    } else if (c === ".") {
      segments.push("");
    } else {
      segments[segments.length - 1] += c;
    }
  }
  return segments;
}

/** The original (unescaped) key segments a flattened path decodes to. */
export function pathSegments(path: string): string[] {
  return splitJsonPath(path);
}

/** Sensible defaults for a fresh import, matching the Rust `JsonImportOptions`. */
export function defaultImportOptions(): JsonImportOptions {
  return {
    pointer: undefined,
    nestedPolicy: "flatten",
    ignorePaths: [],
    arrayPolicy: "preserveJson",
    joinSeparator: ", ",
    multiArray: undefined,
    nullToken: "null",
    missingToken: "",
    forceIndexed: false,
  };
}

const SHAPE_LABELS: Record<DetectedShape, string> = {
  objectArray: "Array of objects",
  arrayOfArrays: "Array of arrays",
  primitiveArray: "Array of primitives",
  jsonLines: "JSON Lines (NDJSON)",
  objectDocument: "Object document",
  scalarDocument: "Scalar document",
};

/** Human label for a detected input shape. */
export function describeShape(shape: DetectedShape): string {
  return SHAPE_LABELS[shape] ?? shape;
}

/**
 * Whether a flattened `path` is dropped by the given ignore list: a path is
 * ignored when it equals an ignore entry or nests under one (`a` ignores
 * `a.b`, but never `ab`). Used both for the live "will be dropped" hints and
 * the projected-column derivation below.
 */
export function isIgnored(path: string, ignorePaths: string[]): boolean {
  return ignorePaths.some((ig) => ig.length > 0 && (path === ig || path.startsWith(ig + ".")));
}

/**
 * Derive the columns the import will actually produce from a scanned preview
 * and the current ignore list, without a re-scan. The backend re-scan is
 * authoritative for counts/types; this drives the immediate UI feedback when
 * the user toggles an ignore path.
 */
export function projectColumns(columns: PreviewColumn[], ignorePaths: string[]): PreviewColumn[] {
  return columns.filter((c) => !isIgnored(c.name, ignorePaths));
}

/**
 * The array fields that would EXPLODE under the current policy. Only the
 * `explode` policy produces rows; every other policy keeps arrays in a single
 * cell, so nothing explodes.
 */
export function explodingFields(
  arrayFields: ArrayFieldInfo[],
  arrayPolicy: JsonImportOptions["arrayPolicy"],
  ignorePaths: string[],
): ArrayFieldInfo[] {
  if (arrayPolicy !== "explode") return [];
  return arrayFields.filter((f) => !isIgnored(f.path, ignorePaths));
}

/**
 * Whether the current options force an explicit cartesian-or-zip decision.
 *
 * This mirrors the engine's REAL, per-record condition: a `multiArray` mode is
 * required only when some SINGLE record explodes along two or more array
 * dimensions at once (`preview.maxRecordDims >= 2`), which the scan reports
 * under the current options. Two array fields that merely both exist in a
 * heterogeneous file but never co-occur in one record do NOT need a choice, so
 * they must not block the import. The `arrayPolicy === "explode"` guard makes
 * the gate release immediately when the user switches policy, before the
 * debounced re-scan refreshes `maxRecordDims`.
 */
export function needsMultiArrayChoice(
  preview: Pick<JsonImportPreview, "maxRecordDims">,
  options: Pick<JsonImportOptions, "arrayPolicy" | "multiArray">,
): boolean {
  return options.arrayPolicy === "explode" && preview.maxRecordDims >= 2 && !options.multiArray;
}

/**
 * Block-list of reasons the current options cannot be applied yet. An empty
 * array means the import is runnable. These mirror the engine's own
 * validation so the button disables before a doomed invoke, but the backend
 * remains the authority (it re-validates the whole file).
 */
export function validateImportOptions(
  options: JsonImportOptions,
  preview: JsonImportPreview | null,
): string[] {
  const errors: string[] = [];
  if (options.nullToken === options.missingToken) {
    errors.push(
      "The null token and the missing-value text must differ, so explicit nulls stay distinguishable from missing fields.",
    );
  }
  if (
    options.arrayPolicy === "join" &&
    (options.joinSeparator === undefined || options.joinSeparator === null)
  ) {
    errors.push("Choose a separator for the join array policy.");
  }
  if (preview) {
    if (preview.needsPointer && !hasPointer(options)) {
      errors.push("Choose the record-array location (a JSON Pointer) to scan first.");
    }
    if (needsMultiArrayChoice(preview, options)) {
      errors.push(
        "Two or more array fields explode in the same record — choose how to combine them (cartesian or zip).",
      );
    }
  }
  return errors;
}

/** Whether a usable record-array pointer has been chosen (root counts). */
export function hasPointer(options: Pick<JsonImportOptions, "pointer">): boolean {
  return options.pointer !== undefined && options.pointer !== null;
}

/**
 * Toggle a flattened path in the ignore list, returning a NEW list (reducer
 * helper for the per-path nested controls).
 */
export function toggleIgnorePath(ignorePaths: string[], path: string): string[] {
  return ignorePaths.includes(path)
    ? ignorePaths.filter((p) => p !== path)
    : [...ignorePaths, path];
}
