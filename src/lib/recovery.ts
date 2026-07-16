// Pure helpers for crash recovery (F16).

import type { RecoverableSession } from "../types";

/**
 * The primary action for a session: blind replay only when the source is
 * unchanged; Open Copy when it changed; nothing when the source is gone or
 * the journal version is incompatible (Show Location still works).
 */
export function recoveryAction(s: RecoverableSession): "recover" | "openCopy" | "none" {
  if (s.incompatible || s.sourceMissing) return "none";
  return s.sourceChanged ? "openCopy" : "recover";
}

/** Local date+time for a session's last edit. */
export function recoveryTime(epochSecs: number): string {
  if (epochSecs === 0) return "";
  return new Date(epochSecs * 1000).toLocaleString();
}
