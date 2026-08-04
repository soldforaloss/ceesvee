// Pure helpers for the Parquet / Arrow interop UI (F32): format/compression
// labels, suggested export names, complex-field policy state, and the
// open-mode plan (which policies force an editable open, which are outright
// invalid). The policy/mode rules mirror the read side exactly so the
// ParquetInspectDialog surfaces the same constraints the backend enforces —
// immediately and offline. No I/O here; everything is testable in isolation.

import { splitJsonPath } from "./jsonImport";
import { formatBytes } from "./save";
import type {
  ColumnarCompression,
  ColumnarExportOptions,
  ColumnarFormat,
  ColumnarInspection,
  ColumnarOpenOptions,
  ComplexPolicy,
} from "../types";

// ----- formats & compression ------------------------------------------------

const FORMAT_LABELS: Record<ColumnarFormat, string> = {
  parquet: "Apache Parquet",
  // Feather v2 IS the Arrow IPC file container — keep the alias visible.
  arrowFile: "Arrow IPC file (Feather v2)",
  arrowStream: "Arrow IPC stream",
};

export function columnarFormatLabel(format: ColumnarFormat): string {
  return FORMAT_LABELS[format];
}

const COMPRESSION_LABELS: Record<ColumnarCompression, string> = {
  uncompressed: "None (uncompressed)",
  snappy: "Snappy",
  zstd: "Zstandard (zstd)",
};

export function compressionLabel(compression: ColumnarCompression): string {
  return COMPRESSION_LABELS[compression];
}

/** File extension (no dot) the backend writes for each container. */
export function columnarFormatExtension(format: ColumnarFormat): string {
  switch (format) {
    case "parquet":
      return "parquet";
    case "arrowFile":
      return "arrow";
    case "arrowStream":
      return "arrows";
  }
}

/** Replace a name's extension (or append) so it matches the chosen format. */
export function suggestColumnarFileName(base: string, format: ColumnarFormat): string {
  const ext = columnarFormatExtension(format);
  const stem = base.replace(/\.[^.\\/]+$/, "");
  return `${stem}.${ext}`;
}

/** File extensions that route through the columnar inspect/open pipeline. */
export const COLUMNAR_EXTENSIONS = ["parquet", "arrow", "feather", "ipc", "arrows"] as const;

/** Whether a path should open through the F32 inspect dialog. */
export function isColumnarPath(path: string): boolean {
  const lower = path.toLowerCase();
  return COLUMNAR_EXTENSIONS.some((ext) => lower.endsWith(`.${ext}`));
}

// ----- defaults -------------------------------------------------------------

/** Fresh open options: complex fields preserved as JSON, backend cache budget. */
export function defaultColumnarOpenOptions(): ColumnarOpenOptions {
  return { complexPolicy: "preserveJson", fieldPolicies: {}, cacheBudgetBytes: 0 };
}

/** Fresh export options, matching the Rust `ColumnarExportOptions` defaults. */
export function defaultColumnarExportOptions(): ColumnarExportOptions {
  return {
    format: "parquet",
    compression: "snappy",
    typed: true,
    rowGroupRows: 0,
    backup: "none",
  };
}

// ----- complex-field policy state -------------------------------------------

/** The policy in effect for one complex field: its override, else the default. */
export function effectivePolicy(options: ColumnarOpenOptions, path: string): ComplexPolicy {
  return options.fieldPolicies?.[path] ?? options.complexPolicy ?? "preserveJson";
}

/** Return new options with `path` overridden to `policy` (immutably). */
export function setFieldPolicy(
  options: ColumnarOpenOptions,
  path: string,
  policy: ComplexPolicy,
): ColumnarOpenOptions {
  return {
    ...options,
    fieldPolicies: { ...(options.fieldPolicies ?? {}), [path]: policy },
  };
}

/** The complex fields whose effective policy is `explode`, in the given order. */
export function explodeFields(options: ColumnarOpenOptions, complexFields: string[]): string[] {
  return complexFields.filter((path) => effectivePolicy(options, path) === "explode");
}

// ----- open-mode plan -------------------------------------------------------

/**
 * The consequences of the current policy selection for the two open modes.
 * Mirrors the read side: exploding a list changes the row count, so it is
 * editable-open only and at most ONE column may explode per open.
 */
export interface ColumnarOpenPlan {
  /** Complex fields currently set to explode. */
  exploded: string[];
  /** More than one explode selected — invalid for BOTH modes. */
  tooManyExplode: boolean;
  /** Any explode selected — the indexed (read-only) mode cannot represent it. */
  requiresEditable: boolean;
  /** Why the indexed button is unavailable, or null when it is available. */
  indexedDisabledReason: string | null;
  /** Blocking errors that make either open invalid. */
  errors: string[];
}

export function columnarOpenPlan(
  inspection: ColumnarInspection,
  options: ColumnarOpenOptions,
): ColumnarOpenPlan {
  const exploded = explodeFields(options, inspection.complexFields);
  const tooManyExplode = exploded.length > 1;
  const requiresEditable = exploded.length > 0;
  const errors: string[] = [];
  if (tooManyExplode) {
    errors.push(
      `Only one field can be exploded into rows per open (${exploded.length} selected) — set the others to keep-as-JSON or drop.`,
    );
  }
  const indexedDisabledReason = requiresEditable
    ? "Exploding a list multiplies the row count, which a read-only index can't represent — convert to editable instead."
    : null;
  return { exploded, tooManyExplode, requiresEditable, indexedDisabledReason, errors };
}

// ----- inspection display ---------------------------------------------------

/** Human label for the chunk unit: Parquet row groups vs Arrow record batches. */
export function chunkUnitLabel(format: ColumnarFormat, count: number): string {
  if (format === "parquet") return count === 1 ? "row group" : "row groups";
  return count === 1 ? "record batch" : "record batches";
}

/** "12.3 MB"-style label for the editable-memory estimate. */
export function estimatedMemoryLabel(inspection: ColumnarInspection): string {
  return formatBytes(inspection.estimatedMemory);
}

/** Nesting depth of a flattened path (0 = top level), for indented rendering. */
export function columnDepth(name: string): number {
  return splitJsonPath(name).length - 1;
}

/** The last (leaf) segment of a flattened path, for the nested tree display. */
export function leafName(name: string): string {
  const segments = splitJsonPath(name);
  return segments[segments.length - 1] ?? name;
}
