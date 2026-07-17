// Pure helpers for sampling & partitioning (F48). The authoritative counts and
// selection come from the backend (`preview_sample` / `start_sample`); these
// functions cover the client-side concerns the dialog needs before a preview:
// generating a reproducible seed, projecting counts for a live estimate,
// normalizing partition weights for display, and validating parameters so the
// UI can disable Preview/Run with a clear reason (mirrors the backend guards).

import type { PartitionOutput, PartitionSpec, SamplingMethod } from "../types";

/** The eight method discriminants, for the picker. */
export type MethodKey = SamplingMethod["type"];

export const METHOD_LABELS: Record<MethodKey, string> = {
  head: "First N rows",
  tail: "Last N rows",
  randomCount: "Random fixed count",
  randomPercentage: "Random percentage",
  systematic: "Systematic (every Nth)",
  stratified: "Stratified (proportional)",
  balanced: "Balanced (equal per group)",
  hashDeterministic: "Hash-based (deterministic)",
};

/**
 * A crypto-random seed as a 53-bit SAFE integer. The backend seed is a u64,
 * but a value above 2^53 loses precision crossing the JSON IPC boundary, which
 * would silently break reproducibility. Bounding generation to a safe integer
 * guarantees the seed the UI shows is exactly the seed the backend runs — so
 * "same seed ⇒ identical outputs" holds end to end. Still crypto-random.
 */
export function generateSeed(): number {
  const buf = new Uint32Array(2);
  crypto.getRandomValues(buf);
  // 21 high bits + 32 low bits = 53 bits, uniform in [0, 2^53).
  return (buf[0] >>> 11) * 2 ** 32 + buf[1];
}

/** Whether a string is a valid non-negative integer seed within safe range. */
export function parseSeed(text: string): number | null {
  const t = text.trim();
  if (!/^\d+$/.test(t)) return null;
  const n = Number(t);
  if (!Number.isSafeInteger(n)) return null;
  return n;
}

/** Sum of partition weights (raw, unnormalized). */
export function weightSum(parts: PartitionOutput[]): number {
  return parts.reduce((acc, p) => acc + (p.weight > 0 ? p.weight : 0), 0);
}

/** Each partition's weight as a fraction of the total (0 when the sum is 0). */
export function normalizeWeights(parts: PartitionOutput[]): number[] {
  const sum = weightSum(parts);
  if (sum <= 0) return parts.map(() => 0);
  return parts.map((p) => (p.weight > 0 ? p.weight : 0) / sum);
}

/** A partition's share as a percentage string for display (one decimal). */
export function weightPercentLabel(parts: PartitionOutput[], index: number): string {
  const fractions = normalizeWeights(parts);
  return `${(fractions[index] * 100).toFixed(1)}%`;
}

/**
 * Largest-remainder apportionment of `total` across `weights`: the parts sum to
 * exactly `total`, remainders broken deterministically (largest first, then
 * lowest index). Mirrors the backend `largest_remainder`, so the projected
 * partition counts match the exact counts for a plain weighted split.
 */
export function largestRemainder(total: number, weights: number[]): number[] {
  const sum = weights.reduce((a, w) => a + Math.max(0, w), 0);
  if (sum <= 0) return weights.map(() => 0);
  const base: number[] = [];
  const remainders: { frac: number; index: number }[] = [];
  let assigned = 0;
  weights.forEach((w, i) => {
    const exact = (total * Math.max(0, w)) / sum;
    const floor = Math.floor(exact);
    base.push(floor);
    remainders.push({ frac: exact - floor, index: i });
    assigned += floor;
  });
  let leftover = Math.max(0, total - assigned);
  remainders.sort((a, b) => b.frac - a.frac || a.index - b.index);
  for (const r of remainders) {
    if (leftover === 0) break;
    base[r.index] += 1;
    leftover -= 1;
  }
  return base;
}

/**
 * The count a sampling method's formula predicts, for a live pre-preview
 * estimate. Mirrors the backend `project_sample`. Stratified/balanced need
 * per-stratum sizes, so they return `null` (the preview supplies exact counts).
 */
export function projectSampleCount(method: SamplingMethod, total: number): number | null {
  switch (method.type) {
    case "head":
    case "tail":
    case "randomCount":
      return Math.min(Math.max(0, Math.floor(method.n)), total);
    case "randomPercentage":
    case "hashDeterministic":
      return Math.round((total * method.percent) / 100);
    case "systematic": {
      const step = Math.max(1, Math.floor(method.step));
      const offset = method.offset == null ? 0 : Math.max(0, Math.floor(method.offset));
      return total > offset ? Math.floor((total - 1 - offset) / step) + 1 : 0;
    }
    case "stratified":
    case "balanced":
      return null;
  }
}

/** Projected per-partition counts for a plain weighted split (display only). */
export function projectPartitionCounts(parts: PartitionOutput[], total: number): number[] {
  return largestRemainder(
    total,
    parts.map((p) => p.weight),
  );
}

const INT_METHODS: MethodKey[] = ["head", "tail", "randomCount"];

/**
 * Validate one method's parameters, returning a human-readable problem or
 * `null` when valid. Mirrors the backend argument checks so the UI can gate
 * Preview/Run before a round-trip.
 */
export function methodProblem(method: SamplingMethod): string | null {
  switch (method.type) {
    case "head":
    case "tail":
    case "randomCount":
      if (!Number.isFinite(method.n) || method.n < 1) return "Count must be at least 1";
      return null;
    case "randomPercentage":
      return percentProblem(method.percent);
    case "hashDeterministic":
      return percentProblem(method.percent);
    case "systematic":
      if (!Number.isFinite(method.step) || method.step < 1) return "Step must be at least 1";
      if (method.offset != null && (!Number.isFinite(method.offset) || method.offset < 0))
        return "Offset cannot be negative";
      return null;
    case "stratified":
      if (method.columns.length === 0) return "Pick at least one stratify column";
      if (!Number.isFinite(method.fraction) || method.fraction < 0 || method.fraction > 1)
        return "Fraction must be between 0 and 1";
      if (!Number.isFinite(method.tolerance) || method.tolerance < 0)
        return "Tolerance cannot be negative";
      return null;
    case "balanced":
      if (method.columns.length === 0) return "Pick at least one group column";
      if (!Number.isFinite(method.perStratum) || method.perStratum < 1)
        return "Rows per group must be at least 1";
      return null;
  }
}

function percentProblem(percent: number): string | null {
  if (!Number.isFinite(percent) || percent < 0 || percent > 100)
    return "Percentage must be between 0 and 100";
  return null;
}

/** Whether a method's count discriminant is an integer field (for the input). */
export function isIntegerCountMethod(type: MethodKey): boolean {
  return INT_METHODS.includes(type);
}

/**
 * Validate a partition spec, mirroring the backend `plan_partition` guards.
 * Returns a problem string or `null`.
 */
export function partitionProblem(spec: PartitionSpec): string | null {
  if (spec.allowOverlap) return "Overlapping partitions are not yet supported";
  if (spec.parts.length < 2) return "A split needs at least two partitions";
  const names = new Set<string>();
  for (const p of spec.parts) {
    if (!Number.isFinite(p.weight) || p.weight < 0) return "Weights cannot be negative";
    const name = p.name.trim();
    if (name === "") return "Partition names cannot be blank";
    if (names.has(name)) return `Duplicate partition name "${name}"`;
    names.add(name);
  }
  if (weightSum(spec.parts) <= 0) return "At least one weight must be positive";
  if (spec.groupBy.length > 0 && spec.stratifyBy.length > 0)
    return "Group-preserving and stratified partitioning cannot be combined";
  return null;
}

/** A named partition preset (train/validation/test and friends). */
export interface PartitionPreset {
  id: string;
  label: string;
  parts: PartitionOutput[];
}

function folds(n: number): PartitionOutput[] {
  return Array.from({ length: n }, (_, i) => ({ name: `fold${i + 1}`, weight: 1 }));
}

export const PARTITION_PRESETS: PartitionPreset[] = [
  {
    id: "trainTest",
    label: "Train / Test — 80 / 20",
    parts: [
      { name: "train", weight: 80 },
      { name: "test", weight: 20 },
    ],
  },
  {
    id: "trainValTest",
    label: "Train / Validation / Test — 70 / 15 / 15",
    parts: [
      { name: "train", weight: 70 },
      { name: "validation", weight: 15 },
      { name: "test", weight: 15 },
    ],
  },
  {
    id: "halves",
    label: "Two equal halves",
    parts: [
      { name: "a", weight: 50 },
      { name: "b", weight: 50 },
    ],
  },
  { id: "kfold5", label: "5 equal folds", parts: folds(5) },
  { id: "kfold10", label: "10 equal folds", parts: folds(10) },
];
