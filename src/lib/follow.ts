// Pure helpers + event wiring for follow mode (F19).

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { FollowAlert, FollowAlertKind, FollowUpdate } from "../types";

export function followAlertMessage(kind: FollowAlertKind): string {
  switch (kind) {
    case "truncatedOrRotated":
      return "The file was truncated, replaced, or rotated — old and new content are never combined silently.";
    case "widthChanged":
      return "New records are wider than this document — the schema changed.";
    case "encodingChanged":
      return "New bytes do not match the opened encoding.";
    case "missing":
      return "The file disappeared.";
  }
}

/** Subscribe to watcher events; returns the unlisten functions. */
export async function onFollowEvents(
  onUpdate: (update: FollowUpdate) => void,
  onAlert: (alert: FollowAlert) => void,
): Promise<UnlistenFn[]> {
  return Promise.all([
    listen<FollowUpdate>("follow-update", (e) => onUpdate(e.payload)),
    listen<FollowAlert>("follow-alert", (e) => onAlert(e.payload)),
  ]);
}
