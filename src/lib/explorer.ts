// Pure helpers for the column explorer (F05): building filter specs from
// value/range selections without disturbing the user's existing filter tree.

import type { FilterCondition, FilterGroup } from "../types";

let conditionId = 0;
function nextId(): string {
  conditionId += 1;
  return `explorer-${conditionId}`;
}

/** `column == value` (or `!=` when excluding), case-sensitively exact. */
export function valueCondition(column: number, value: string, exclude: boolean): FilterCondition {
  return {
    type: "condition",
    id: nextId(),
    column,
    op: exclude ? "notEquals" : "equals",
    value,
    caseSensitive: true,
  };
}

/** `min <= column <= max` as gte/lte conditions (either bound optional). */
export function rangeConditions(
  column: number,
  min: string | null,
  max: string | null,
): FilterCondition[] {
  const conditions: FilterCondition[] = [];
  if (min !== null && min !== "") {
    conditions.push({
      type: "condition",
      id: nextId(),
      column,
      op: "gte",
      value: min,
      caseSensitive: false,
    });
  }
  if (max !== null && max !== "") {
    conditions.push({
      type: "condition",
      id: nextId(),
      column,
      op: "lte",
      value: max,
      caseSensitive: false,
    });
  }
  return conditions;
}

/** A fresh filter tree containing only `conditions` (AND-combined). */
export function specOf(conditions: FilterCondition[]): FilterGroup {
  return { type: "group", id: nextId(), conjunction: "and", nodes: conditions };
}

/**
 * AND the conditions into an existing filter tree WITHOUT deleting it: an
 * AND root gains the conditions as siblings; any other root is wrapped so
 * the previous tree survives intact as a subgroup.
 */
export function withAndConditions(spec: FilterGroup, conditions: FilterCondition[]): FilterGroup {
  if (conditions.length === 0) return spec;
  if (spec.conjunction === "and") {
    return { ...spec, nodes: [...spec.nodes, ...conditions] };
  }
  return {
    type: "group",
    id: nextId(),
    conjunction: "and",
    nodes: [spec, ...conditions],
  };
}
