// Keyboard-shortcut plumbing for the command registry (F11).
//
// Bindings are stored as normalized strings: modifiers in the fixed order
// `mod+ctrl+alt+shift`, then one key name, joined by `+` (e.g. "mod+shift+s",
// "f2"). "mod" means Ctrl on Windows/Linux and Cmd on macOS, so defaults stay
// portable; user overrides captured on this machine record what was pressed.

export const IS_MAC = typeof navigator !== "undefined" && /mac/i.test(navigator.userAgent ?? "");

/** Keys we accept as chord terminators, by their normalized name. */
const NAMED_KEYS = new Set([
  "enter",
  "escape",
  "backspace",
  "delete",
  "tab",
  "space",
  "arrowup",
  "arrowdown",
  "arrowleft",
  "arrowright",
  "home",
  "end",
  "pageup",
  "pagedown",
  ...Array.from({ length: 12 }, (_, i) => `f${i + 1}`),
]);

/** Parse a stored binding into its parts; null for anything malformed. */
export function parseBinding(binding: string): { mods: Set<string>; key: string } | null {
  const parts = binding.toLowerCase().split("+");
  const key = parts.pop() ?? "";
  const mods = new Set<string>();
  for (const part of parts) {
    if (
      part === "mod" ||
      part === "ctrl" ||
      part === "alt" ||
      part === "shift" ||
      part === "meta"
    ) {
      mods.add(part);
    } else {
      return null;
    }
  }
  if (key.length === 1) return { mods, key };
  if (NAMED_KEYS.has(key)) return { mods, key };
  return null;
}

/** Normalize a binding string to canonical order/case; null if invalid. */
export function normalizeBinding(binding: string): string | null {
  const parsed = parseBinding(binding);
  if (!parsed) return null;
  const ordered = ["mod", "ctrl", "meta", "alt", "shift"].filter((m) => parsed.mods.has(m));
  return [...ordered, parsed.key].join("+");
}

/** The normalized binding a KeyboardEvent represents, or null (bare key
 * presses without modifiers are only bindings for named keys like F2). */
export function bindingFromEvent(e: KeyboardEvent): string | null {
  const key = e.key.toLowerCase();
  if (key === "control" || key === "shift" || key === "alt" || key === "meta") return null;
  const mods: string[] = [];
  // Collapse the platform-primary modifier back to "mod" so captured
  // overrides stay portable across machines.
  const primary = IS_MAC ? e.metaKey : e.ctrlKey;
  const secondaryMeta = IS_MAC ? e.ctrlKey : e.metaKey;
  if (primary) mods.push("mod");
  if (secondaryMeta) mods.push(IS_MAC ? "ctrl" : "meta");
  if (e.altKey) mods.push("alt");
  if (e.shiftKey) mods.push("shift");

  const name = key === " " ? "space" : key;
  if (name.length === 1) {
    if (mods.length === 0) return null; // plain typing, never a shortcut
    return normalizeBinding([...mods, name].join("+"));
  }
  if (NAMED_KEYS.has(name)) return normalizeBinding([...mods, name].join("+"));
  return null;
}

/** Human label for a binding ("Ctrl+Shift+S" / "⌘⇧S" style, per platform). */
export function bindingLabel(binding: string): string {
  const parsed = parseBinding(binding);
  if (!parsed) return binding;
  const label = (part: string): string => {
    switch (part) {
      case "mod":
        return IS_MAC ? "⌘" : "Ctrl";
      case "ctrl":
        return IS_MAC ? "⌃" : "Ctrl";
      case "meta":
        return IS_MAC ? "⌘" : "Win";
      case "alt":
        return IS_MAC ? "⌥" : "Alt";
      case "shift":
        return IS_MAC ? "⇧" : "Shift";
      default:
        return part.length === 1
          ? part.toUpperCase()
          : part === "space"
            ? "Space"
            : part.charAt(0).toUpperCase() + part.slice(1);
    }
  };
  const ordered = ["mod", "ctrl", "meta", "alt", "shift"].filter((m) => parsed.mods.has(m));
  const parts = [...ordered.map(label), label(parsed.key)];
  return parts.join(IS_MAC ? "" : "+");
}

/**
 * Effective binding per command id: defaults overlaid with user overrides
 * (`null` override = explicitly unbound). Invalid overrides are ignored.
 */
export function effectiveBindings(
  defaults: ReadonlyMap<string, string>,
  overrides: Record<string, string | null> | undefined,
): Map<string, string> {
  const out = new Map<string, string>();
  for (const [id, binding] of defaults) out.set(id, binding);
  for (const [id, binding] of Object.entries(overrides ?? {})) {
    if (binding === null) {
      out.delete(id);
    } else {
      const normalized = normalizeBinding(binding);
      if (normalized) out.set(id, normalized);
    }
  }
  return out;
}

/** The command id currently holding `binding`, if any. */
export function findConflict(
  bindings: ReadonlyMap<string, string>,
  binding: string,
  excludeCommandId?: string,
): string | null {
  for (const [id, bound] of bindings) {
    if (bound === binding && id !== excludeCommandId) return id;
  }
  return null;
}
