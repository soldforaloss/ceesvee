import { describe, expect, it } from "vitest";

import {
  displayFormatOptions,
  formatCellValue,
  gateCellEdit,
  isNumericType,
  isTemporalType,
} from "./schema";
import type { CellEditValidation, ColumnSchema } from "../types";

function schema(patch: Partial<ColumnSchema>): ColumnSchema {
  return {
    columnId: "c0",
    name: "col",
    logicalType: "text",
    nullable: true,
    nullTokens: [],
    validationMode: "advisory",
    ...patch,
  };
}

describe("gateCellEdit", () => {
  it("passes a valid value or an undeclared column", () => {
    expect(gateCellEdit(null)).toEqual({ block: false, warn: false, message: null });
    const ok: CellEditValidation = { valid: true };
    expect(gateCellEdit(ok)).toEqual({ block: false, warn: false, message: null });
  });

  it("blocks a strict violation and surfaces the reason", () => {
    const v: CellEditValidation = { valid: false, mode: "strict", reason: "not an integer" };
    expect(gateCellEdit(v)).toEqual({ block: true, warn: false, message: "not an integer" });
  });

  it("warns but allows an advisory violation", () => {
    const v: CellEditValidation = { valid: false, mode: "advisory", reason: "not an integer" };
    const gate = gateCellEdit(v);
    expect(gate.block).toBe(false);
    expect(gate.warn).toBe(true);
    expect(gate.message).toBe("not an integer");
  });

  it("falls back to a generic message when none is given", () => {
    const v: CellEditValidation = { valid: false, mode: "strict" };
    expect(gateCellEdit(v).message).toMatch(/declared type/);
  });
});

describe("displayFormatOptions", () => {
  it("offers number formats for numeric types and none for text-like types", () => {
    expect(displayFormatOptions("integer").map((o) => o.value)).toContain("thousands");
    expect(displayFormatOptions("date").map((o) => o.value)).toContain("iso");
    expect(displayFormatOptions("text")).toEqual([]);
    expect(displayFormatOptions("uuid")).toEqual([]);
    expect(displayFormatOptions("json")).toEqual([]);
    expect(isNumericType("decimal")).toBe(true);
    expect(isTemporalType("datetime")).toBe(true);
  });
});

describe("formatCellValue — passthrough", () => {
  it("returns raw when there is no display format", () => {
    expect(formatCellValue(schema({ logicalType: "integer" }), "1500")).toBe("1500");
  });

  it("preserves leading zeroes for a declared-text ZIP column (acceptance)", () => {
    // Text has no display catalogue, so the raw text is always shown verbatim.
    const zip = schema({ logicalType: "text", displayFormat: "thousands" });
    expect(formatCellValue(zip, "00501")).toBe("00501");
  });

  it("returns raw for invalid / non-parsing values (never corrupts data)", () => {
    const s = schema({ logicalType: "integer", displayFormat: "thousands" });
    expect(formatCellValue(s, "abc")).toBe("abc");
    expect(formatCellValue(s, "")).toBe("");
  });

  it("returns raw for an unknown display pattern", () => {
    const s = schema({ logicalType: "integer", displayFormat: "nonsense" });
    expect(formatCellValue(s, "1500")).toBe("1500");
  });
});

describe("formatCellValue — integers", () => {
  const thousands = schema({ logicalType: "integer", displayFormat: "thousands" });

  it("groups thousands (acceptance: 1500 → 1,500)", () => {
    expect(formatCellValue(thousands, "1500")).toBe("1,500");
    expect(formatCellValue(thousands, "1234567")).toBe("1,234,567");
    expect(formatCellValue(thousands, "-42")).toBe("-42");
  });

  it("applies fixed decimals and percent", () => {
    expect(formatCellValue(schema({ logicalType: "integer", displayFormat: "fixed:2" }), "7")).toBe(
      "7.00",
    );
    expect(formatCellValue(schema({ logicalType: "integer", displayFormat: "percent" }), "3")).toBe(
      "300%",
    );
  });
});

describe("formatCellValue — decimals", () => {
  it("preserves scale under thousands grouping", () => {
    const s = schema({ logicalType: "decimal", displayFormat: "thousands" });
    expect(formatCellValue(s, "1234.50")).toBe("1,234.50");
  });

  it("rounds half-away-from-zero for fixed", () => {
    const s = schema({ logicalType: "decimal", displayFormat: "fixed:2" });
    expect(formatCellValue(s, "1.005")).toBe("1.01");
    expect(formatCellValue(s, "2.5")).toBe("2.50");
    expect(
      formatCellValue(schema({ logicalType: "decimal", displayFormat: "fixed:0" }), "2.5"),
    ).toBe("3");
  });

  it("computes percent exactly by shifting the point", () => {
    const s = schema({ logicalType: "decimal", displayFormat: "percent" });
    expect(formatCellValue(s, "0.5")).toBe("50%");
    expect(formatCellValue(s, "1.2345")).toBe("123.45%");
  });
});

describe("formatCellValue — locale numbers", () => {
  it("reads and writes de-DE grouping", () => {
    const s = schema({ logicalType: "decimal", locale: "de-DE", displayFormat: "thousands" });
    // 1.234,5 (de) → grouped de output 1.234,5
    expect(formatCellValue(s, "1.234,5")).toBe("1.234,5");
  });

  it("rejects a bad grouping under the declared locale (stays raw)", () => {
    const s = schema({ logicalType: "decimal", locale: "de-DE", displayFormat: "thousands" });
    // "1.5" under de-DE is not a valid group of three → raw, never 15.
    expect(formatCellValue(s, "1.5")).toBe("1.5");
  });

  it("groups Swiss numbers with an apostrophe", () => {
    const s = schema({ logicalType: "integer", locale: "de-CH", displayFormat: "thousands" });
    expect(formatCellValue(s, "1234567")).toBe("1'234'567");
  });
});

describe("formatCellValue — floats", () => {
  it("groups and fixes floats", () => {
    expect(
      formatCellValue(schema({ logicalType: "float", displayFormat: "thousands" }), "1234.5"),
    ).toBe("1,234.5");
    expect(
      formatCellValue(schema({ logicalType: "float", displayFormat: "fixed:1" }), "2.25"),
    ).toBe("2.3");
  });
});

describe("formatCellValue — dates", () => {
  it("reformats an ISO date across the catalogue", () => {
    const iso = schema({ logicalType: "date", displayFormat: "iso" });
    const eu = schema({ logicalType: "date", displayFormat: "eu" });
    const us = schema({ logicalType: "date", displayFormat: "us" });
    const long = schema({ logicalType: "date", displayFormat: "long" });
    expect(formatCellValue(iso, "2024-01-31")).toBe("2024-01-31");
    expect(formatCellValue(eu, "2024-01-31")).toBe("31.01.2024");
    expect(formatCellValue(us, "2024-01-31")).toBe("01/31/2024");
    expect(formatCellValue(long, "2024-01-31")).toBe("Jan 31, 2024");
  });

  it("reformats an ISO datetime and drops seconds in long form", () => {
    const iso = schema({ logicalType: "datetime", displayFormat: "iso" });
    const long = schema({ logicalType: "datetime", displayFormat: "long" });
    expect(formatCellValue(iso, "2024-01-31 15:04:05")).toBe("2024-01-31 15:04:05");
    expect(formatCellValue(iso, "2024-01-31T15:04:05")).toBe("2024-01-31 15:04:05");
    expect(formatCellValue(long, "2024-01-31 15:04:05")).toBe("Jan 31, 2024 15:04");
  });

  it("leaves zoned or non-ISO dates untouched", () => {
    const iso = schema({ logicalType: "datetime", displayFormat: "iso" });
    expect(formatCellValue(iso, "2024-01-31T15:04:05Z")).toBe("2024-01-31T15:04:05Z");
    expect(
      formatCellValue(schema({ logicalType: "date", displayFormat: "eu" }), "31/01/2024"),
    ).toBe("31/01/2024");
  });
});
