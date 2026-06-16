import { describe, expect, it } from "vitest";
import { applyReplace } from "./replace";
import type { FindOptions } from "../types";

const opts = (over: Partial<FindOptions>): FindOptions => ({ query: "", ...over });

describe("applyReplace", () => {
  it("treats the replacement literally in plain mode ($ is not a group ref)", () => {
    expect(applyReplace("price $5 and $5", opts({ query: "$5" }), "$10")).toBe("price $10 and $10");
  });

  it("is case-insensitive by default", () => {
    expect(applyReplace("Hello HELLO", opts({ query: "hello" }), "hi")).toBe("hi hi");
  });

  it("respects case sensitivity", () => {
    expect(applyReplace("Hello hello", opts({ query: "hello", caseSensitive: true }), "hi")).toBe(
      "Hello hi",
    );
  });

  it("whole-cell only matches the entire cell", () => {
    expect(applyReplace("category", opts({ query: "cat", wholeCell: true }), "X")).toBe("category");
    expect(applyReplace("cat", opts({ query: "cat", wholeCell: true }), "X")).toBe("X");
  });

  it("supports regex capture groups", () => {
    expect(
      applyReplace(
        "2026-01-02",
        opts({ query: "(\\d{4})-(\\d{2})-(\\d{2})", regex: true }),
        "$3/$2/$1",
      ),
    ).toBe("02/01/2026");
  });
});
