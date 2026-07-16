import { describe, expect, it } from "vitest";

import { formatBytes, isLegacyEncoding } from "./save";

describe("isLegacyEncoding", () => {
  it("treats Unicode encodings as safe", () => {
    expect(isLegacyEncoding("UTF-8")).toBe(false);
    expect(isLegacyEncoding("utf-8")).toBe(false);
    expect(isLegacyEncoding("UTF-16LE")).toBe(false);
    expect(isLegacyEncoding("UTF-16BE")).toBe(false);
  });

  it("flags single-byte encodings for the compatibility scan", () => {
    expect(isLegacyEncoding("windows-1252")).toBe(true);
    expect(isLegacyEncoding("ISO-8859-1")).toBe(true);
  });
});

describe("formatBytes", () => {
  it("scales through units", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(formatBytes(250 * 1024 * 1024)).toBe("250 MB");
  });
});
