import { describe, expect, it } from "vitest";

import type { DocumentMeta } from "../types";
import {
  globToRegExp,
  matchingProfiles,
  profileFromDocument,
  profileMatches,
  profileSettingsDiffer,
} from "./profiles";

describe("profileMatches", () => {
  it("matches exact paths across separators and case", () => {
    expect(
      profileMatches({ type: "exactPath", path: "C:\\Data\\Orders.csv" }, "c:/data/orders.csv"),
    ).toBe(true);
    expect(profileMatches({ type: "exactPath", path: "C:/data/a.csv" }, "C:/data/b.csv")).toBe(
      false,
    );
  });

  it("matches directories including subdirectories", () => {
    const m = { type: "directory", directory: "C:/exports" } as const;
    expect(profileMatches(m, "C:/exports/june.csv")).toBe(true);
    expect(profileMatches(m, "C:/exports/2026/june.csv")).toBe(true);
    expect(profileMatches(m, "C:/exports-old/june.csv")).toBe(false);
  });

  it("matches extensions with or without the dot", () => {
    expect(profileMatches({ type: "extension", extension: ".TSV" }, "a/b/data.tsv")).toBe(true);
    expect(profileMatches({ type: "extension", extension: "csv" }, "a/b/data.tsv")).toBe(false);
  });

  it("matches globs against the basename, or full path when it has a slash", () => {
    expect(profileMatches({ type: "glob", pattern: "orders-*.csv" }, "C:/x/orders-06.csv")).toBe(
      true,
    );
    expect(profileMatches({ type: "glob", pattern: "orders-??.csv" }, "C:/x/orders-6.csv")).toBe(
      false,
    );
    expect(
      profileMatches({ type: "glob", pattern: "**/reports/*.csv" }, "C:/a/b/reports/q1.csv"),
    ).toBe(true);
  });
});

describe("globToRegExp", () => {
  it("escapes regex metacharacters", () => {
    const re = globToRegExp("a+b(1).csv");
    expect(re.test("a+b(1).csv")).toBe(true);
    expect(re.test("aXb(1).csv")).toBe(false);
  });

  it("keeps * within a path segment and ** across segments", () => {
    expect(globToRegExp("a/*.csv").test("a/x.csv")).toBe(true);
    expect(globToRegExp("a/*.csv").test("a/b/x.csv")).toBe(false);
    expect(globToRegExp("a/**.csv").test("a/b/x.csv")).toBe(true);
  });
});

describe("profile assembly and matching lists", () => {
  const meta = {
    path: "C:/data/orders.csv",
    delimiter: ";",
    encoding: "windows-1252",
    hasHeaderRow: true,
    headers: ["id", "amount"],
  } as unknown as DocumentMeta;

  it("captures the document's settings and columns", () => {
    const p = profileFromDocument("Orders", meta);
    expect(p.matcher).toEqual({ type: "exactPath", path: "C:/data/orders.csv" });
    expect(p.delimiter).toBe(";");
    expect(p.expectedColumns).toEqual(["id", "amount"]);
    expect(p.autoApply).toBe(false);
  });

  it("detects when profile settings differ from the document", () => {
    const p = profileFromDocument("Orders", meta);
    expect(profileSettingsDiffer(p, meta)).toBe(false);
    expect(profileSettingsDiffer({ ...p, delimiter: "," }, meta)).toBe(true);
    expect(profileSettingsDiffer({ ...p, delimiter: null }, meta)).toBe(false);
  });

  it("lists matching profiles in stored order", () => {
    const a = profileFromDocument("A", meta);
    const b = {
      ...profileFromDocument("B", meta),
      matcher: { type: "extension", extension: "csv" } as const,
    };
    const c = {
      ...profileFromDocument("C", meta),
      matcher: { type: "extension", extension: "tsv" } as const,
    };
    expect(matchingProfiles([a, b, c], "C:/data/orders.csv").map((p) => p.name)).toEqual([
      "A",
      "B",
    ]);
  });
});
