// Pure helpers for reshape (F23).

import type { ReshapeSpec } from "../types";

/** Client-side shape check mirroring the backend validation. */
export function reshapeProblem(spec: ReshapeSpec): string | null {
  switch (spec.type) {
    case "unpivot":
      if (spec.valueColumns.length === 0) return "pick at least one column to unpivot";
      if (spec.idColumns.some((c) => spec.valueColumns.includes(c)))
        return "a column cannot be both an identifier and a value column";
      if (spec.attributeName.trim() === "" || spec.valueName.trim() === "")
        return "attribute and value output names are required";
      return null;
    case "pivot":
      if (spec.rowKeys.length === 0) return "pick at least one row-key column";
      if (spec.rowKeys.includes(spec.headerColumn) || spec.rowKeys.includes(spec.valueColumn))
        return "the header/value columns cannot also be row keys";
      return null;
    case "transpose":
      return null;
  }
}
