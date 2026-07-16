import { describe, expect, it } from "vitest";

import { delimitedFilesInDir, isDelimitedFile } from "./append";

describe("isDelimitedFile", () => {
  it("accepts known extensions case-insensitively and rejects others", () => {
    expect(isDelimitedFile("orders.csv")).toBe(true);
    expect(isDelimitedFile("orders.TSV")).toBe(true);
    expect(isDelimitedFile("report.xlsx")).toBe(false);
    expect(isDelimitedFile("archive.zip")).toBe(false);
    expect(isDelimitedFile("no-extension")).toBe(false);
  });
});

describe("delimitedFilesInDir", () => {
  it("filters to delimited files, joins with the dir's separator, sorts", () => {
    const entries = [
      { name: "b.csv", isFile: true },
      { name: "a.csv", isFile: true },
      { name: "sub", isFile: false },
      { name: "notes.md", isFile: true },
    ];
    expect(delimitedFilesInDir("C:\\data", entries)).toEqual([
      "C:\\data\\a.csv",
      "C:\\data\\b.csv",
    ]);
    expect(delimitedFilesInDir("/data/", entries)).toEqual(["/data/a.csv", "/data/b.csv"]);
  });
});
