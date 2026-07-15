import { describe, expect, it } from "vitest";

import { autoMapColumns } from "./compare";

describe("autoMapColumns", () => {
  it("pairs same-name columns even when reordered", () => {
    expect(autoMapColumns(["id", "name", "age"], ["name", "age", "id"])).toEqual([
      [0, 2],
      [1, 0],
      [2, 1],
    ]);
  });

  it("matches names case-insensitively and trimmed", () => {
    expect(autoMapColumns(["ID ", "Name"], ["name", "id"])).toEqual([
      [0, 1],
      [1, 0],
    ]);
  });

  it("falls back to positional pairing for unmatched columns", () => {
    // Blank headers can't match by name; they pair positionally.
    expect(autoMapColumns(["", "b"], ["", "b"])).toEqual([
      [0, 0],
      [1, 1],
    ]);
  });

  it("leaves extra columns unmapped", () => {
    expect(autoMapColumns(["a", "b", "extra"], ["b", "a"])).toEqual([
      [0, 1],
      [1, 0],
    ]);
  });
});
