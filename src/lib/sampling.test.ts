import { describe, expect, it } from "vitest";

import {
  PARTITION_PRESETS,
  generateSeed,
  isIntegerCountMethod,
  largestRemainder,
  methodProblem,
  normalizeWeights,
  parseSeed,
  partitionConstraintProblem,
  partitionProblem,
  projectPartitionCounts,
  projectSampleCount,
  weightPercentLabel,
  weightSum,
} from "./sampling";
import type { PartitionOutput, PartitionSpec, SamplingMethod } from "../types";

describe("seed handling", () => {
  it("generates safe integers in [0, 2^53)", () => {
    for (let i = 0; i < 500; i += 1) {
      const s = generateSeed();
      expect(Number.isSafeInteger(s)).toBe(true);
      expect(s).toBeGreaterThanOrEqual(0);
      expect(s).toBeLessThan(2 ** 53);
    }
  });

  it("produces varied seeds", () => {
    const seeds = new Set(Array.from({ length: 50 }, () => generateSeed()));
    // Collisions are astronomically unlikely across 53 bits.
    expect(seeds.size).toBeGreaterThan(45);
  });

  it("parses only non-negative safe integers", () => {
    expect(parseSeed("0")).toBe(0);
    expect(parseSeed("  42 ")).toBe(42);
    expect(parseSeed("9007199254740991")).toBe(9007199254740991); // MAX_SAFE_INTEGER
    expect(parseSeed("")).toBeNull();
    expect(parseSeed("-1")).toBeNull();
    expect(parseSeed("1.5")).toBeNull();
    expect(parseSeed("abc")).toBeNull();
    expect(parseSeed("9007199254740993")).toBeNull(); // above MAX_SAFE_INTEGER
  });
});

describe("weight normalization", () => {
  const parts = (weights: number[]): PartitionOutput[] =>
    weights.map((w, i) => ({ name: `p${i}`, weight: w }));

  it("sums weights, ignoring negatives", () => {
    expect(weightSum(parts([70, 15, 15]))).toBe(100);
    expect(weightSum(parts([1, -3, 2]))).toBe(3);
    expect(weightSum(parts([0, 0]))).toBe(0);
  });

  it("normalizes to fractions summing to ~1", () => {
    const f = normalizeWeights(parts([70, 15, 15]));
    expect(f[0]).toBeCloseTo(0.7, 10);
    expect(f[1]).toBeCloseTo(0.15, 10);
    expect(f.reduce((a, b) => a + b, 0)).toBeCloseTo(1, 10);
  });

  it("yields zeros when the total weight is zero", () => {
    expect(normalizeWeights(parts([0, 0]))).toEqual([0, 0]);
  });

  it("formats a percentage label", () => {
    expect(weightPercentLabel(parts([80, 20]), 0)).toBe("80.0%");
    expect(weightPercentLabel(parts([1, 1, 1]), 2)).toBe("33.3%");
  });
});

describe("largest-remainder apportionment", () => {
  it("sums exactly to the total", () => {
    for (const total of [0, 1, 7, 100, 1001, 999983]) {
      const counts = largestRemainder(total, [70, 15, 15]);
      expect(counts.reduce((a, b) => a + b, 0)).toBe(total);
    }
  });

  it("breaks ties by largest remainder then lowest index", () => {
    // 10 rows / 3 equal parts => 4,3,3 (the first part takes the extra).
    expect(largestRemainder(10, [1, 1, 1])).toEqual([4, 3, 3]);
    // 7 rows / [1,1] => 4,3.
    expect(largestRemainder(7, [1, 1])).toEqual([4, 3]);
  });

  it("returns zeros when weights are non-positive", () => {
    expect(largestRemainder(10, [0, 0])).toEqual([0, 0]);
  });
});

describe("count projection", () => {
  const total = 1000;

  it("projects fixed-count methods as min(n, total)", () => {
    expect(projectSampleCount({ type: "head", n: 50 }, total)).toBe(50);
    expect(projectSampleCount({ type: "tail", n: 5000 }, total)).toBe(1000);
    expect(projectSampleCount({ type: "randomCount", n: 250 }, total)).toBe(250);
  });

  it("projects percentage methods by rounding", () => {
    expect(projectSampleCount({ type: "randomPercentage", percent: 10 }, total)).toBe(100);
    expect(
      projectSampleCount({ type: "hashDeterministic", columns: null, percent: 12.5 }, total),
    ).toBe(125);
  });

  it("projects systematic every-Nth with an offset", () => {
    // rows 0,10,...,990 => 100 rows.
    expect(projectSampleCount({ type: "systematic", step: 10, offset: null }, total)).toBe(100);
    // offset 5: rows 5,15,...,995 => 100 rows.
    expect(projectSampleCount({ type: "systematic", step: 10, offset: 5 }, total)).toBe(100);
    // offset beyond the data => 0.
    expect(projectSampleCount({ type: "systematic", step: 10, offset: 2000 }, total)).toBe(0);
  });

  it("cannot project stratified/balanced without per-stratum sizes", () => {
    expect(
      projectSampleCount(
        { type: "stratified", columns: ["c0"], fraction: 0.1, tolerance: 0 },
        total,
      ),
    ).toBeNull();
    expect(
      projectSampleCount({ type: "balanced", columns: ["c0"], perStratum: 10 }, total),
    ).toBeNull();
  });

  it("projects partition counts via largest remainder", () => {
    const counts = projectPartitionCounts(
      [
        { name: "train", weight: 70 },
        { name: "test", weight: 30 },
      ],
      1000,
    );
    expect(counts).toEqual([700, 300]);
    expect(counts.reduce((a, b) => a + b, 0)).toBe(1000);
  });
});

describe("method parameter validation", () => {
  const ok = (m: SamplingMethod) => expect(methodProblem(m)).toBeNull();
  const bad = (m: SamplingMethod) => expect(methodProblem(m)).not.toBeNull();

  it("requires a positive count", () => {
    ok({ type: "head", n: 1 });
    bad({ type: "head", n: 0 });
    bad({ type: "randomCount", n: -5 });
  });

  it("bounds percentages to 0..100", () => {
    ok({ type: "randomPercentage", percent: 0 });
    ok({ type: "randomPercentage", percent: 100 });
    bad({ type: "randomPercentage", percent: 100.1 });
    bad({ type: "hashDeterministic", columns: null, percent: -1 });
  });

  it("requires a step of at least 1 and a non-negative offset", () => {
    ok({ type: "systematic", step: 1, offset: null });
    ok({ type: "systematic", step: 5, offset: 2 });
    bad({ type: "systematic", step: 0, offset: null });
    bad({ type: "systematic", step: 5, offset: -1 });
  });

  it("requires columns and a valid fraction for stratified", () => {
    ok({ type: "stratified", columns: ["c0"], fraction: 0.5, tolerance: 0.01 });
    bad({ type: "stratified", columns: [], fraction: 0.5, tolerance: 0.01 });
    bad({ type: "stratified", columns: ["c0"], fraction: 1.5, tolerance: 0.01 });
    bad({ type: "stratified", columns: ["c0"], fraction: 0.5, tolerance: -0.1 });
  });

  it("requires columns and a positive per-stratum count for balanced", () => {
    ok({ type: "balanced", columns: ["c0"], perStratum: 10 });
    bad({ type: "balanced", columns: [], perStratum: 10 });
    bad({ type: "balanced", columns: ["c0"], perStratum: 0 });
  });

  it("marks fixed-count methods as integer-count", () => {
    expect(isIntegerCountMethod("head")).toBe(true);
    expect(isIntegerCountMethod("randomCount")).toBe(true);
    expect(isIntegerCountMethod("randomPercentage")).toBe(false);
    expect(isIntegerCountMethod("stratified")).toBe(false);
  });
});

describe("partition spec validation", () => {
  const spec = (over: Partial<PartitionSpec>): PartitionSpec => ({
    parts: [
      { name: "train", weight: 80 },
      { name: "test", weight: 20 },
    ],
    stratifyBy: [],
    groupBy: [],
    allowOverlap: false,
    ...over,
  });

  it("accepts a valid two-way split", () => {
    expect(partitionProblem(spec({}))).toBeNull();
  });

  it("rejects fewer than two partitions", () => {
    expect(partitionProblem(spec({ parts: [{ name: "only", weight: 1 }] }))).not.toBeNull();
  });

  it("rejects negative weights and an all-zero total", () => {
    expect(
      partitionProblem(
        spec({
          parts: [
            { name: "a", weight: -1 },
            { name: "b", weight: 2 },
          ],
        }),
      ),
    ).not.toBeNull();
    expect(
      partitionProblem(
        spec({
          parts: [
            { name: "a", weight: 0 },
            { name: "b", weight: 0 },
          ],
        }),
      ),
    ).not.toBeNull();
  });

  it("rejects blank or duplicate names", () => {
    expect(
      partitionProblem(
        spec({
          parts: [
            { name: "  ", weight: 1 },
            { name: "b", weight: 1 },
          ],
        }),
      ),
    ).not.toBeNull();
    expect(
      partitionProblem(
        spec({
          parts: [
            { name: "dup", weight: 1 },
            { name: "dup", weight: 1 },
          ],
        }),
      ),
    ).not.toBeNull();
  });

  it("rejects combining stratified and group-preserving", () => {
    expect(partitionProblem(spec({ stratifyBy: ["c0"], groupBy: ["c1"] }))).not.toBeNull();
  });

  it("rejects the unsupported overlap flag", () => {
    expect(partitionProblem(spec({ allowOverlap: true }))).not.toBeNull();
  });
});

describe("partition constraint validation", () => {
  it("accepts the plain constraint with no columns", () => {
    expect(partitionConstraintProblem("plain", [])).toBeNull();
    expect(partitionConstraintProblem("plain", ["c0"])).toBeNull();
  });

  it("requires at least one column for a stratified or group constraint", () => {
    // The bug this guards: a stratified/group constraint with no columns picked
    // sends empty stratifyBy AND groupBy, which the backend cannot distinguish
    // from a plain split — so it must be rejected before Preview/Run.
    expect(partitionConstraintProblem("stratified", [])).not.toBeNull();
    expect(partitionConstraintProblem("group", [])).not.toBeNull();
    expect(partitionConstraintProblem("stratified", ["c0"])).toBeNull();
    expect(partitionConstraintProblem("group", ["c1", "c2"])).toBeNull();
  });
});

describe("partition presets", () => {
  it("every preset is itself a valid partition spec", () => {
    for (const preset of PARTITION_PRESETS) {
      const problem = partitionProblem({
        parts: preset.parts,
        stratifyBy: [],
        groupBy: [],
        allowOverlap: false,
      });
      expect(problem, preset.id).toBeNull();
    }
  });

  it("includes the train/validation/test split", () => {
    const preset = PARTITION_PRESETS.find((p) => p.id === "trainValTest");
    expect(preset?.parts.map((p) => p.name)).toEqual(["train", "validation", "test"]);
  });
});
