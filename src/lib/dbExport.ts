// Pure logic for the F35 database export dialog: the mapping-editor state, the
// document-column → SQL-type defaults, spec construction and the client-side
// gating that decides whether an export can run. Kept free of React and Tauri
// so it is unit-testable in isolation (see dbExport.test.ts); the dialog and
// the store only render/dispatch what these functions compute.
//
// The backend (`db_export.rs`) is the source of truth for the resolved mapping
// and every conversion decision; these helpers mirror its defaults so the
// dialog can prefill and gate before the first preview round-trips, and hand it
// a spec it will accept.

import type {
  DbColumnMapIn,
  DbConflictPolicy,
  DbExportColumn,
  DbExportMode,
  DbExportPreview,
  DbExportSpec,
  DbSqlType,
  LogicalType,
} from "../types";

/** The five writable SQL column types, in the order the type picker offers. */
export const SQL_TYPES: DbSqlType[] = ["TEXT", "INTEGER", "REAL", "NUMERIC", "BOOLEAN"];

/** One per-column override the mapping editor holds, keyed by stable column id.
 * Every field is optional: an unset field means "use the backend default". */
export interface ColumnOverride {
  /** Renamed SQL column name; empty/blank is treated as "use the default". */
  sqlName?: string;
  /** Explicit SQL type (ignored by the backend in append mode). */
  sqlType?: DbSqlType;
  /** Part of the new table's PRIMARY KEY (create/replace only). */
  primaryKey?: boolean;
}

/** The editable export-dialog form state (decoupled from the store). */
export interface ExportForm {
  /** Chosen target database file; null until the user picks one. */
  path: string | null;
  table: string;
  mode: DbExportMode;
  conflictPolicy: DbConflictPolicy;
  confirmReplace: boolean;
  /** Per-column overrides, keyed by stable column id. */
  overrides: Record<string, ColumnOverride>;
}

/**
 * The default SQL type CEESVEE writes for a declared F31 logical type — the
 * TEXT-default rule the backend's `SqlType::for_logical` applies. Columns with
 * no declared schema (undefined) also default to TEXT.
 */
export function defaultSqlType(lt: LogicalType | undefined): DbSqlType {
  switch (lt) {
    case "integer":
      return "INTEGER";
    case "float":
      return "REAL";
    case "decimal":
      return "NUMERIC";
    case "boolean":
      return "BOOLEAN";
    default:
      // text, date, datetime, uuid, json and undeclared → TEXT.
      return "TEXT";
  }
}

/**
 * Suggest a SQLite-safe table name from a document file name: drop the
 * extension, replace anything but ASCII word characters with "_", collapse
 * runs, trim edge underscores. Falls back to a generic name, and prefixes the
 * reserved "sqlite_" so the backend's guard never rejects the default.
 */
export function suggestTableName(fileName: string): string {
  const stem = fileName.replace(/\.[^.]+$/, "");
  let name = stem
    .replace(/[^A-Za-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "");
  if (name === "") name = "exported_table";
  if (/^sqlite_/i.test(name)) name = `t_${name}`;
  return name;
}

/** Human label for an export mode. */
export function describeMode(mode: DbExportMode): string {
  switch (mode) {
    case "create":
      return "Create new table";
    case "append":
      return "Append to existing table";
    case "replace":
      return "Replace table";
  }
}

/** Human label for a conflict policy. */
export function describeConflict(policy: DbConflictPolicy): string {
  switch (policy) {
    case "abort":
      return "Abort on conflict";
    case "skip":
      return "Skip conflicting rows";
    case "replace":
      return "Replace conflicting rows";
  }
}

/**
 * Per-column compatibility, for the mapping editor's status column. In append
 * mode the existing table's type wins, so a column is "compatible" iff it maps
 * onto an existing target column; in create/replace the chosen SQL type is
 * always written.
 */
export function columnCompatibility(
  col: DbExportColumn,
  mode: DbExportMode,
): { ok: boolean; label: string } {
  if (mode === "append") {
    return col.targetDeclType != null
      ? { ok: true, label: `matches ${col.targetDeclType || "column"}` }
      : { ok: false, label: "no matching column" };
  }
  return { ok: true, label: col.sqlType };
}

/**
 * Build the wire {@link DbColumnMapIn} list from the editor overrides. Only
 * meaningfully-set fields are emitted (a blank rename is dropped so the backend
 * keeps the default and never sees an empty name); the rest of a document's
 * columns pick up defaults server-side.
 */
export function buildMappings(overrides: Record<string, ColumnOverride>): DbColumnMapIn[] {
  const out: DbColumnMapIn[] = [];
  for (const [columnId, ov] of Object.entries(overrides)) {
    const sqlName = ov.sqlName?.trim();
    const entry: DbColumnMapIn = { columnId };
    let meaningful = false;
    if (sqlName) {
      entry.sqlName = sqlName;
      meaningful = true;
    }
    if (ov.sqlType) {
      entry.sqlType = ov.sqlType;
      meaningful = true;
    }
    if (ov.primaryKey) {
      entry.primaryKey = true;
      meaningful = true;
    }
    if (meaningful) out.push(entry);
  }
  return out;
}

/** Construct the export spec the preview and the write both consume. */
export function buildSpec(form: ExportForm): DbExportSpec {
  return {
    path: form.path ?? "",
    table: form.table.trim(),
    mode: form.mode,
    mappings: buildMappings(form.overrides),
    conflictPolicy: form.conflictPolicy,
    // The confirmation only means anything for replace; never leak it otherwise.
    confirmReplace: form.mode === "replace" ? form.confirmReplace : false,
  };
}

/**
 * Every reason the export cannot run right now, most fundamental first. An
 * empty list means the run button is enabled. Mirrors the backend's own
 * refusals so the button is disabled rather than the invoke rejecting — a
 * conversion failure is included because the write aborts on the first one.
 */
export function exportBlockers(form: ExportForm, preview: DbExportPreview | null): string[] {
  const reasons: string[] = [];
  if (!form.path) reasons.push("Choose a target database file.");
  const table = form.table.trim();
  if (table === "") {
    reasons.push("Enter a table name.");
  } else if (/^sqlite_/i.test(table)) {
    reasons.push('Table names beginning "sqlite_" are reserved by SQLite.');
  }
  if (form.mode === "replace" && preview?.tableExists && !form.confirmReplace) {
    reasons.push("Confirm replacing the existing table.");
  }
  if (preview) {
    for (const issue of preview.blocking) reasons.push(issue);
    if (preview.failureCount > 0) {
      reasons.push(
        `${preview.failureCount.toLocaleString()} cell${
          preview.failureCount === 1 ? "" : "s"
        } cannot convert to the mapped SQL type — the write would abort. ` +
          "Map the affected column to TEXT, or fix the data.",
      );
    }
  } else {
    // No preview yet: still require a target so the button reads correctly.
    if (form.path && table !== "") reasons.push("Preview the export first.");
  }
  return reasons;
}

/** Whether the export can be started (no blockers and a preview to guard on). */
export function canRunExport(form: ExportForm, preview: DbExportPreview | null): boolean {
  return preview != null && exportBlockers(form, preview).length === 0;
}
