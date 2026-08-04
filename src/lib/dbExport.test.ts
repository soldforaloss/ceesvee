import { describe, expect, it } from "vitest";

import {
  buildMappings,
  buildSpec,
  canRunExport,
  columnCompatibility,
  defaultSqlType,
  describeConflict,
  describeMode,
  exportBlockers,
  suggestTableName,
  type ExportForm,
} from "./dbExport";
import type { DbExportColumn, DbExportPreview } from "../types";

const form = (over: Partial<ExportForm> = {}): ExportForm => ({
  path: "/tmp/out.sqlite",
  table: "customers",
  mode: "create",
  conflictPolicy: "abort",
  confirmReplace: false,
  overrides: {},
  ...over,
});

const col = (over: Partial<DbExportColumn> = {}): DbExportColumn => ({
  columnId: "c0",
  name: "id",
  sqlName: "id",
  sqlType: "INTEGER",
  primaryKey: false,
  targetDeclType: null,
  ...over,
});

const preview = (over: Partial<DbExportPreview> = {}): DbExportPreview => ({
  revision: 3,
  tableExists: false,
  targetRows: null,
  columns: [col()],
  blocking: [],
  failures: [],
  failureCount: 0,
  rowsScanned: 10,
  scanComplete: true,
  ...over,
});

describe("defaultSqlType", () => {
  it("maps declared logical types to the TEXT-default SQL set", () => {
    expect(defaultSqlType("integer")).toBe("INTEGER");
    expect(defaultSqlType("float")).toBe("REAL");
    expect(defaultSqlType("decimal")).toBe("NUMERIC");
    expect(defaultSqlType("boolean")).toBe("BOOLEAN");
    expect(defaultSqlType("text")).toBe("TEXT");
    expect(defaultSqlType("date")).toBe("TEXT");
    expect(defaultSqlType("datetime")).toBe("TEXT");
    expect(defaultSqlType("uuid")).toBe("TEXT");
    expect(defaultSqlType("json")).toBe("TEXT");
    expect(defaultSqlType(undefined)).toBe("TEXT");
  });
});

describe("suggestTableName", () => {
  it("strips the extension and sanitises to word characters", () => {
    expect(suggestTableName("Q4 sales.csv")).toBe("Q4_sales");
    expect(suggestTableName("weird//name..tsv")).toBe("weird_name");
  });

  it("falls back for empty and de-reserves the sqlite_ prefix", () => {
    expect(suggestTableName(".csv")).toBe("exported_table");
    expect(suggestTableName("!!!.csv")).toBe("exported_table");
    expect(suggestTableName("sqlite_stat.csv")).toBe("t_sqlite_stat");
  });
});

describe("buildMappings", () => {
  it("emits only meaningfully-set overrides and drops blank renames", () => {
    const out = buildMappings({
      c0: { sqlName: "  ", sqlType: undefined, primaryKey: false },
      c1: { sqlName: "renamed" },
      c2: { sqlType: "TEXT" },
      c3: { primaryKey: true },
      c4: {},
    });
    expect(out).toEqual([
      { columnId: "c1", sqlName: "renamed" },
      { columnId: "c2", sqlType: "TEXT" },
      { columnId: "c3", primaryKey: true },
    ]);
  });

  it("trims a rename before sending it", () => {
    expect(buildMappings({ c0: { sqlName: "  spaced  " } })).toEqual([
      { columnId: "c0", sqlName: "spaced" },
    ]);
  });
});

describe("buildSpec", () => {
  it("trims the table and carries confirmReplace only for replace mode", () => {
    const spec = buildSpec(form({ table: "  t  ", mode: "replace", confirmReplace: true }));
    expect(spec.table).toBe("t");
    expect(spec.mode).toBe("replace");
    expect(spec.confirmReplace).toBe(true);
  });

  it("never leaks confirmReplace outside replace mode", () => {
    const spec = buildSpec(form({ mode: "append", confirmReplace: true }));
    expect(spec.confirmReplace).toBe(false);
  });

  it("substitutes an empty path when none is chosen", () => {
    expect(buildSpec(form({ path: null })).path).toBe("");
  });
});

describe("columnCompatibility", () => {
  it("shows the chosen type for create/replace", () => {
    expect(columnCompatibility(col({ sqlType: "NUMERIC" }), "create")).toEqual({
      ok: true,
      label: "NUMERIC",
    });
    expect(columnCompatibility(col({ sqlType: "REAL" }), "replace").ok).toBe(true);
  });

  it("flags a missing target column in append mode", () => {
    expect(columnCompatibility(col({ targetDeclType: "INTEGER" }), "append")).toEqual({
      ok: true,
      label: "matches INTEGER",
    });
    expect(columnCompatibility(col({ targetDeclType: null }), "append")).toEqual({
      ok: false,
      label: "no matching column",
    });
  });
});

describe("exportBlockers / canRunExport", () => {
  it("requires a path, a table name and a preview", () => {
    expect(exportBlockers(form({ path: null }), preview())).toContain(
      "Choose a target database file.",
    );
    expect(exportBlockers(form({ table: "   " }), preview())).toContain("Enter a table name.");
    expect(exportBlockers(form(), null)).toContain("Preview the export first.");
  });

  it("rejects reserved table names", () => {
    expect(exportBlockers(form({ table: "sqlite_x" }), preview())).toEqual([
      'Table names beginning "sqlite_" are reserved by SQLite.',
    ]);
  });

  it("demands confirmation before replacing an existing table", () => {
    const f = form({ mode: "replace", confirmReplace: false });
    const p = preview({ tableExists: true });
    expect(exportBlockers(f, p)).toContain("Confirm replacing the existing table.");
    expect(canRunExport(f, p)).toBe(false);
    expect(canRunExport({ ...f, confirmReplace: true }, p)).toBe(true);
  });

  it("does not demand confirmation when the replace target does not exist yet", () => {
    const f = form({ mode: "replace", confirmReplace: false });
    expect(canRunExport(f, preview({ tableExists: false }))).toBe(true);
  });

  it("surfaces backend blocking issues verbatim", () => {
    const p = preview({ blocking: ['column "surprise" does not exist'] });
    expect(exportBlockers(form(), p)).toContain('column "surprise" does not exist');
    expect(canRunExport(form(), p)).toBe(false);
  });

  it("blocks on conversion failures the write would abort on", () => {
    const p = preview({ failureCount: 2 });
    const blockers = exportBlockers(form(), p);
    expect(blockers.some((b) => b.includes("cannot convert"))).toBe(true);
    expect(canRunExport(form(), p)).toBe(false);
  });

  it("passes a clean preview", () => {
    expect(exportBlockers(form(), preview())).toEqual([]);
    expect(canRunExport(form(), preview())).toBe(true);
  });
});

describe("describe helpers", () => {
  it("labels every mode and policy", () => {
    expect(describeMode("create")).toMatch(/create/i);
    expect(describeMode("append")).toMatch(/append/i);
    expect(describeMode("replace")).toMatch(/replace/i);
    expect(describeConflict("abort")).toMatch(/abort/i);
    expect(describeConflict("skip")).toMatch(/skip/i);
    expect(describeConflict("replace")).toMatch(/replace/i);
  });
});
