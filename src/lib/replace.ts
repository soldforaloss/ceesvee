// Single-cell replace used by the "Replace" (one match) action. Replace-all is
// handled in Rust; this mirrors it for the current match using a JS RegExp.

import type { FindOptions } from "../types";

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function buildRegExp(opts: FindOptions): RegExp {
  const flags = "g" + (opts.caseSensitive ? "" : "i");
  let pattern = opts.regex ? opts.query : escapeRegExp(opts.query);
  if (opts.wholeCell) pattern = `^(?:${pattern})$`;
  return new RegExp(pattern, flags);
}

/** Apply a replacement to a single cell's text. */
export function applyReplace(cell: string, opts: FindOptions, replacement: string): string {
  const re = buildRegExp(opts);
  if (opts.regex) {
    return cell.replace(re, replacement);
  }
  // Plain mode: the replacement is literal ($ has no special meaning).
  return cell.replace(re, () => replacement);
}
