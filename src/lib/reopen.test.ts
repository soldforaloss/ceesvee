import { describe, expect, it } from "vitest";

import type { DocumentMeta } from "../types";
import { currentOpenOptions, describeDiff, fingerprintKey } from "./reopen";

describe("fingerprintKey", () => {
  it("is stable and distinguishes size/mtime", () => {
    expect(fingerprintKey({ size: 10, modifiedAtMs: 999 })).toBe("10:999");
    expect(fingerprintKey({ size: 10, modifiedAtMs: 999 })).toBe(
      fingerprintKey({ size: 10, modifiedAtMs: 999 }),
    );
    expect(fingerprintKey({ size: 11, modifiedAtMs: 999 })).not.toBe(
      fingerprintKey({ size: 10, modifiedAtMs: 999 }),
    );
    expect(fingerprintKey(null)).toBe("missing");
  });
});

describe("currentOpenOptions", () => {
  it("pins the document's current settings as explicit overrides", () => {
    const meta = {
      delimiter: ";",
      encoding: "windows-1252",
      hasHeaderRow: false,
    } as DocumentMeta;
    expect(currentOpenOptions(meta)).toEqual({
      delimiter: ";",
      encoding: "windows-1252",
      hasHeaderRow: false,
    });
  });
});

describe("describeDiff", () => {
  it("prettifies known fields", () => {
    expect(describeDiff({ field: "delimiter", current: ",", proposed: "\t" })).toBe(
      "Delimiter: Comma → Tab",
    );
    expect(describeDiff({ field: "headerMode", current: "true", proposed: "false" })).toBe(
      "First row is header: yes → no",
    );
    expect(describeDiff({ field: "lineEnding", current: "lf", proposed: "crlf" })).toBe(
      "Line endings: LF → CRLF",
    );
    expect(describeDiff({ field: "rowCount", current: "10", proposed: "12" })).toBe(
      "Rows: 10 → 12",
    );
  });

  it("falls back to the raw field name for unknown fields", () => {
    expect(describeDiff({ field: "mystery", current: "a", proposed: "b" })).toBe("mystery: a → b");
  });
});
