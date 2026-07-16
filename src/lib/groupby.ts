// Pure helpers for group-by aggregations (F22).

import type { Aggregate, AggregateSpec } from "../types";

/** Every aggregate except the plain row count operates on a column. */
export function aggregateNeedsColumn(aggregate: Aggregate): boolean {
  return aggregate !== "count";
}

/** Whether any aggregate produces concatenated output (separator applies). */
export function usesConcat(aggregates: AggregateSpec[]): boolean {
  return aggregates.some((a) => a.aggregate === "concat" || a.aggregate === "concatDistinct");
}

/** Normalize a spec before submission: count drops its column. */
export function normalizeAggregates(aggregates: AggregateSpec[]): AggregateSpec[] {
  return aggregates.map((a) =>
    aggregateNeedsColumn(a.aggregate) ? { ...a, column: a.column ?? 0 } : { ...a, column: null },
  );
}
