import { describe, expect, it } from "vitest";

import {
  allParamsValid,
  applySuggestion,
  buildSuggestions,
  currentToken,
  detectParams,
  historyLabel,
  isStrictDecimal,
  matchSuggestions,
  mergeDetectedParams,
  paramErrors,
  pushHistoryEntry,
  validateParamValue,
} from "./sqlWorkspace";
import type { SqlHistoryEntry, SqlParam, SqlSchemaDto, SqlTableInfo } from "../types";

// ---------------------------------------------------------------------------
// Parameter detection
// ---------------------------------------------------------------------------

describe("detectParams", () => {
  it("finds :name parameters in first-appearance order, de-duplicated", () => {
    const sql = "SELECT * FROM t WHERE a = :min AND b < :max OR c = :min";
    expect(detectParams(sql)).toEqual(["min", "max"]);
  });

  it("ignores parameters inside string literals", () => {
    expect(detectParams("SELECT ':notaparam' AS x WHERE y = :real")).toEqual(["real"]);
  });

  it("honours doubled-quote escapes inside strings", () => {
    // The '' is an escaped quote, so the string runs to the second real quote;
    // :inside is data, :outside is the only parameter.
    const sql = "SELECT 'it''s :inside' WHERE y = :outside";
    expect(detectParams(sql)).toEqual(["outside"]);
  });

  it("ignores parameters inside quoted / bracketed identifiers", () => {
    expect(detectParams('SELECT "col:x", [b:y] FROM t WHERE z = :z')).toEqual(["z"]);
  });

  it("ignores parameters inside line and block comments", () => {
    expect(detectParams("SELECT 1 -- :nope\nWHERE a = :yes")).toEqual(["yes"]);
    expect(detectParams("SELECT /* :nope */ 1 WHERE a = :yes")).toEqual(["yes"]);
  });

  it("does not treat a :: cast as a parameter", () => {
    expect(detectParams("SELECT x::text, :real FROM t")).toEqual(["real"]);
  });

  it("does not treat positional or bare colons as parameters", () => {
    expect(detectParams("SELECT ? , : FROM t")).toEqual([]);
  });

  it("stops the name at the first non-identifier char", () => {
    expect(detectParams("WHERE a=:foo,b=:bar)")).toEqual(["foo", "bar"]);
  });
});

// ---------------------------------------------------------------------------
// Parameter merge (detection + typing carry-over)
// ---------------------------------------------------------------------------

describe("mergeDetectedParams", () => {
  const existing: SqlParam[] = [
    { name: "min", type: "integer", value: "5" },
    { name: "gone", type: "date", value: "2020-01-01" },
  ];

  it("keeps type and value for retained params, defaults new ones to text", () => {
    const merged = mergeDetectedParams(existing, "SELECT * WHERE a > :min AND b = :name");
    expect(merged).toEqual([
      { name: "min", type: "integer", value: "5" },
      { name: "name", type: "text", value: "" },
    ]);
  });

  it("drops params the query no longer references", () => {
    const merged = mergeDetectedParams(existing, "SELECT :min");
    expect(merged.map((p) => p.name)).toEqual(["min"]);
  });

  it("orders params by first appearance in the SQL", () => {
    const merged = mergeDetectedParams([], "SELECT :b, :a, :c");
    expect(merged.map((p) => p.name)).toEqual(["b", "a", "c"]);
  });
});

// ---------------------------------------------------------------------------
// Typed value validation
// ---------------------------------------------------------------------------

function p(type: SqlParam["type"], value: string | null): SqlParam {
  return { name: "x", type, value };
}

describe("validateParamValue", () => {
  it("accepts ANY string for text — including SQL-injection-looking values", () => {
    // A hostile value is still valid TEXT: the engine binds it as data, never
    // splices it into the statement, so the editor must not reject it.
    expect(validateParamValue(p("text", "'; DROP TABLE users; --"))).toBeNull();
    expect(validateParamValue(p("text", ""))).toBeNull();
  });

  it("validates integers (i64 range) and rejects non-integers", () => {
    expect(validateParamValue(p("integer", "42"))).toBeNull();
    expect(validateParamValue(p("integer", "-7"))).toBeNull();
    expect(validateParamValue(p("integer", "+7"))).toBeNull();
    expect(validateParamValue(p("integer", "3.5"))).not.toBeNull();
    expect(validateParamValue(p("integer", "1e3"))).not.toBeNull();
    expect(validateParamValue(p("integer", ""))).not.toBeNull();
    // Beyond i64 range.
    expect(validateParamValue(p("integer", "99999999999999999999"))).not.toBeNull();
  });

  it("validates decimals strictly (no exponent, both sides present)", () => {
    expect(isStrictDecimal("1.5")).toBe(true);
    expect(isStrictDecimal("-30")).toBe(true);
    expect(isStrictDecimal("1.")).toBe(false);
    expect(isStrictDecimal(".5")).toBe(false);
    expect(isStrictDecimal("1e3")).toBe(false);
    expect(validateParamValue(p("decimal", "1.25"))).toBeNull();
    expect(validateParamValue(p("decimal", "1.2.3"))).not.toBeNull();
  });

  it("validates floats (finite) and rejects Infinity/NaN/blank", () => {
    expect(validateParamValue(p("float", "1.5e3"))).toBeNull();
    expect(validateParamValue(p("float", "Infinity"))).not.toBeNull();
    expect(validateParamValue(p("float", "abc"))).not.toBeNull();
  });

  it("validates booleans (true/false/1/0, case-insensitive)", () => {
    for (const v of ["true", "FALSE", "1", "0"]) {
      expect(validateParamValue(p("boolean", v))).toBeNull();
    }
    expect(validateParamValue(p("boolean", "yes"))).not.toBeNull();
  });

  it("validates calendar dates and rejects impossible ones", () => {
    expect(validateParamValue(p("date", "2024-02-29"))).toBeNull(); // leap year
    expect(validateParamValue(p("date", "2023-02-29"))).not.toBeNull();
    expect(validateParamValue(p("date", "2024-13-01"))).not.toBeNull();
    expect(validateParamValue(p("date", "2024/01/01"))).not.toBeNull();
  });

  it("validates ISO datetimes in the accepted shapes", () => {
    expect(validateParamValue(p("datetime", "2024-01-02T03:04:05"))).toBeNull();
    expect(validateParamValue(p("datetime", "2024-01-02 03:04:05.123"))).toBeNull();
    expect(validateParamValue(p("datetime", "2024-01-02T03:04:05Z"))).toBeNull();
    expect(validateParamValue(p("datetime", "2024-01-02"))).not.toBeNull();
    expect(validateParamValue(p("datetime", "2024-01-02T99:00:00"))).not.toBeNull();
  });

  it("treats null-typed params as always valid regardless of value", () => {
    expect(validateParamValue(p("null", null))).toBeNull();
    expect(validateParamValue(p("null", "ignored"))).toBeNull();
  });

  it("paramErrors collects only the invalid entries; allParamsValid gates a run", () => {
    const params: SqlParam[] = [
      { name: "a", type: "integer", value: "10" },
      { name: "b", type: "integer", value: "oops" },
      { name: "c", type: "text", value: "anything" },
    ];
    expect(paramErrors(params)).toEqual({ b: "not a valid integer" });
    expect(allParamsValid(params)).toBe(false);
    expect(allParamsValid([params[0], params[2]])).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// History ring reducer
// ---------------------------------------------------------------------------

function entry(sql: string): SqlHistoryEntry {
  return { sql, params: [], sources: [], ranAtMs: 0, status: "done" };
}

describe("pushHistoryEntry", () => {
  it("prepends most-recent-first without mutating the input", () => {
    const a = [entry("SELECT 1")];
    const b = pushHistoryEntry(a, entry("SELECT 2"));
    expect(b.map((e) => e.sql)).toEqual(["SELECT 2", "SELECT 1"]);
    expect(a.map((e) => e.sql)).toEqual(["SELECT 1"]); // original untouched
  });

  it("caps the ring at the given size, dropping the oldest", () => {
    let list: SqlHistoryEntry[] = [];
    for (let i = 0; i < 5; i++) list = pushHistoryEntry(list, entry(`q${i}`), 3);
    expect(list.map((e) => e.sql)).toEqual(["q4", "q3", "q2"]);
  });

  it("historyLabel shows the first non-empty line, capped", () => {
    expect(historyLabel("\n\n  SELECT * FROM t  \nWHERE x")).toBe("SELECT * FROM t");
    expect(historyLabel("SELECT " + "a".repeat(200), 10)).toHaveLength(10);
    expect(historyLabel("   ")).toBe("(empty query)");
  });
});

// ---------------------------------------------------------------------------
// Autocomplete
// ---------------------------------------------------------------------------

function table(alias: string, kind: string, cols: string[]): SqlTableInfo {
  return {
    alias,
    label: `${alias}.file`,
    kind,
    columns: cols.map((name) => ({ name, declType: "text" })),
    columnsTruncated: false,
    rowCount: null,
    path: null,
  };
}

const schema: SqlSchemaDto = {
  documents: [table("orders", "document", ["order_id", "total", "customer"])],
  files: [table("customers", "csv", ["customer_id", "name"])],
  database: [table("audit", "table", ["ts", "action"])],
  databaseTruncated: false,
};

describe("buildSuggestions", () => {
  it("emits table, bare-column and qualified-column tokens, de-duplicated", () => {
    const s = buildSuggestions(schema);
    const texts = s.map((x) => x.text);
    expect(texts).toContain("orders");
    expect(texts).toContain("customers");
    expect(texts).toContain("audit");
    expect(texts).toContain("total");
    expect(texts).toContain("orders.total");
    // Table tokens are kind "table"; qualified names are columns.
    expect(s.find((x) => x.text === "orders")?.kind).toBe("table");
    expect(s.find((x) => x.text === "orders.total")?.kind).toBe("column");
  });

  it("returns nothing for a null schema", () => {
    expect(buildSuggestions(null)).toEqual([]);
  });
});

describe("currentToken", () => {
  it("returns the identifier fragment left of the caret and its start", () => {
    const text = "SELECT ord";
    expect(currentToken(text, text.length)).toEqual({ token: "ord", start: 7 });
  });

  it("includes a dot for qualified fragments", () => {
    const text = "SELECT orders.to";
    expect(currentToken(text, text.length)).toEqual({ token: "orders.to", start: 7 });
  });

  it("is empty right after a non-identifier char", () => {
    expect(currentToken("SELECT * ", 9)).toEqual({ token: "", start: 9 });
  });
});

describe("matchSuggestions", () => {
  const sugg = buildSuggestions(schema);

  it("returns nothing for an empty prefix", () => {
    expect(matchSuggestions(sugg, "")).toEqual([]);
    expect(matchSuggestions(sugg, "   ")).toEqual([]);
  });

  it("prefix-matches tables and bare columns case-insensitively", () => {
    const hits = matchSuggestions(sugg, "cust").map((s) => s.text);
    expect(hits).toContain("customers");
    expect(hits).toContain("customer"); // bare column on orders
    expect(hits).toContain("customer_id");
    // Qualified names are not surfaced by a bare (dot-less) prefix.
    expect(hits.every((t) => !t.includes("."))).toBe(true);
  });

  it("matches qualified columns for a dotted prefix", () => {
    const hits = matchSuggestions(sugg, "orders.").map((s) => s.text);
    expect(hits).toContain("orders.order_id");
    expect(hits).toContain("orders.total");
    expect(hits.every((t) => t.startsWith("orders."))).toBe(true);
  });

  it("honours the result limit and ranks shorter names first", () => {
    const hits = matchSuggestions(sugg, "o", 2);
    expect(hits.length).toBeLessThanOrEqual(2);
  });
});

describe("applySuggestion", () => {
  it("replaces the token at the caret and returns the new caret position", () => {
    const text = "SELECT ord FROM orders";
    const caret = 10; // just after "ord"
    const { text: next, caret: pos } = applySuggestion(text, caret, "orders");
    expect(next).toBe("SELECT orders FROM orders");
    expect(pos).toBe(13); // after the inserted "orders"
  });

  it("replaces a qualified fragment wholesale", () => {
    const text = "WHERE orders.to";
    const { text: next } = applySuggestion(text, text.length, "orders.total");
    expect(next).toBe("WHERE orders.total");
  });
});
