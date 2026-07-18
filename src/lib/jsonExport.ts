// Pure helpers for the JSON export flow (F33): rebuilding nested objects from
// flattened path column names and detecting the duplicate / conflicting output
// paths the engine rejects BEFORE writing. The path logic mirrors
// `json_export::check_output_paths` and `dotted` exactly so the dialog can
// surface the same rejection the backend would, immediately and offline.

import type { BackupPolicy, JsonExportFormat, JsonExportOptions } from "../types";
import { escapeJsonKey, splitJsonPath } from "./jsonImport";

/** Sensible defaults for a fresh JSON export, matching the Rust defaults. */
export function defaultJsonExportOptions(): JsonExportOptions {
  return {
    format: "objects",
    nullToken: "null",
    missingToken: "",
    rebuildNested: false,
    typed: true,
    includeHeaders: true,
    backup: "none",
  };
}

const FORMAT_LABELS: Record<JsonExportFormat, string> = {
  objects: "Array of objects",
  arrays: "Array of arrays",
  jsonLines: "JSON Lines (NDJSON)",
};

export function jsonFormatLabel(format: JsonExportFormat): string {
  return FORMAT_LABELS[format];
}

/** Default file extension for a format (`.jsonl` for JSON Lines). */
export function jsonFormatExtension(format: JsonExportFormat): string {
  return format === "jsonLines" ? "jsonl" : "json";
}

/** Replace a path's extension (or append) so the suggested name matches the format. */
export function suggestJsonFileName(base: string, format: JsonExportFormat): string {
  const ext = jsonFormatExtension(format);
  const stem = base.replace(/\.[^.\\/]+$/, "");
  return `${stem}.${ext}`;
}

/** Display form of a rebuilt path: segments re-escaped, joined with `.`. Mirrors `dotted`. */
export function dottedPath(segments: string[]): string {
  return segments.map((s) => escapeJsonKey(s)).join(".");
}

/**
 * The output-path segments for a column name under a given rebuild mode: split
 * on unescaped dots when rebuilding nested objects, else the literal name as a
 * single top-level key.
 */
export function outputPathFor(name: string, rebuildNested: boolean): string[] {
  return rebuildNested ? splitJsonPath(name) : [name];
}

/** One row of the nested-rebuild mapping preview. */
export interface RebuildRow {
  /** The source column header. */
  header: string;
  /** The nested key path it writes to (original, unescaped segments). */
  segments: string[];
  /** Display form of the path (re-escaped, dotted). */
  path: string;
  /** True when this column participates in a detected conflict. */
  conflict: boolean;
}

/** A rejected output-path configuration (mirrors the two backend checks). */
export interface PathConflict {
  kind: "duplicate" | "prefix";
  message: string;
  /** The column headers involved. */
  columns: [string, string];
}

/**
 * Detect the FIRST duplicate or prefix conflict across the output paths, in
 * the same order and with the same semantics as
 * `json_export::check_output_paths`. Returns null when the configuration is
 * writable. The positional `arrays` format has no keys, so it never conflicts
 * — callers skip this check for that format.
 */
export function findPathConflict(headers: string[], rebuildNested: boolean): PathConflict | null {
  const paths = headers.map((h) => outputPathFor(h, rebuildNested));
  const key = (segs: string[]) => JSON.stringify(segs);

  // 1. Exact duplicate output paths.
  const seen = new Map<string, string>();
  for (let i = 0; i < headers.length; i++) {
    const k = key(paths[i]);
    const first = seen.get(k);
    if (first !== undefined) {
      return {
        kind: "duplicate",
        message: `Duplicate JSON output path "${dottedPath(paths[i])}": columns "${first}" and "${headers[i]}" both write it — rename one of them.`,
        columns: [first, headers[i]],
      };
    }
    seen.set(k, headers[i]);
  }

  // 2. A full path landing on a PROPER prefix of another path (a value where
  //    another column nests an object) — checked in both directions.
  const prefixes = new Map<string, string>();
  for (let i = 0; i < headers.length; i++) {
    const path = paths[i];
    for (let len = 1; len < path.length; len++) {
      const k = key(path.slice(0, len));
      if (!prefixes.has(k)) prefixes.set(k, headers[i]);
    }
  }
  for (let i = 0; i < headers.length; i++) {
    const other = prefixes.get(key(paths[i]));
    if (other !== undefined) {
      return {
        kind: "prefix",
        message: `Conflicting JSON output paths: column "${headers[i]}" writes a value at "${dottedPath(paths[i])}", but column "${other}" nests an object under it — rename one of them.`,
        columns: [headers[i], other],
      };
    }
  }

  return null;
}

/**
 * Build the nested-rebuild mapping preview: one row per column, plus the first
 * conflict (if any). For the non-rebuild case every column is a flat top-level
 * key, so the only possible conflict is a genuinely duplicate column name.
 */
export function buildRebuildMapping(
  headers: string[],
  rebuildNested: boolean,
): { rows: RebuildRow[]; conflict: PathConflict | null } {
  const conflict = findPathConflict(headers, rebuildNested);
  const involved = conflict ? new Set(conflict.columns) : null;
  const rows: RebuildRow[] = headers.map((header) => {
    const segments = outputPathFor(header, rebuildNested);
    return {
      header,
      segments,
      path: dottedPath(segments),
      conflict: involved?.has(header) ?? false,
    };
  });
  return { rows, conflict };
}

/** Reducer helper: normalise the backup toggle. */
export function backupFromChecked(checked: boolean): BackupPolicy {
  return checked ? "single" : "none";
}
