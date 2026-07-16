import { describe, expect, it } from "vitest";

import { parseCellRef } from "./cellRef";
import { CommandRegistry, type AppCommand } from "./commands";
import { fuzzyMatch, fuzzyScore } from "./fuzzy";
import {
  bindingFromEvent,
  bindingLabel,
  effectiveBindings,
  findConflict,
  normalizeBinding,
} from "./shortcuts";

function cmd(id: string, title: string, extra?: Partial<AppCommand>): AppCommand {
  return { id, title, category: "File", run: () => undefined, ...extra };
}

describe("fuzzy matching (F11)", () => {
  it("matches subsequences case-insensitively and rejects non-matches", () => {
    expect(fuzzyMatch("sav", "Save As…")).not.toBeNull();
    expect(fuzzyMatch("xyz", "Save As…")).toBeNull();
    expect(fuzzyMatch("", "anything")?.score).toBe(0);
  });

  it("prefers prefix and word-boundary hits over scattered hits", () => {
    const prefix = fuzzyScore("sort", "Sort…")!;
    const boundary = fuzzyScore("sort", "Rows: sort selection")!;
    const scattered = fuzzyScore("sort", "Set orientation")!;
    expect(prefix).toBeGreaterThan(boundary);
    expect(boundary).toBeGreaterThan(scattered);
  });

  it("falls back to keywords at a small penalty", () => {
    const viaTitle = fuzzyScore("dedup", "Deduplicate rows")!;
    const viaKeyword = fuzzyScore("dedup", "Find duplicates…", ["deduplicate"])!;
    expect(viaKeyword).not.toBeNull();
    expect(viaKeyword).toBeLessThanOrEqual(viaTitle);
  });
});

describe("command registry (F11)", () => {
  it("rejects duplicate command ids", () => {
    const registry = new CommandRegistry();
    registry.register([cmd("a", "Alpha")]);
    expect(() => registry.register([cmd("a", "Alpha again")])).toThrow(/duplicate/);
  });

  it("annotates unavailable commands and ranks them below at equal score", () => {
    const registry = new CommandRegistry();
    registry.register([
      cmd("a", "Duplicate title", { unavailableReason: () => "No document is open" }),
      cmd("b", "Duplicate title"),
    ]);
    const results = registry.search("duplicate");
    expect(results).toHaveLength(2);
    expect(results[0].command.id).toBe("b");
    expect(results[1].command.id).toBe("a");
    expect(results[1].unavailable).toBe("No document is open");
    expect(results[0].unavailable).toBeNull();
  });

  it("includes dynamic provider entries in searches", () => {
    const registry = new CommandRegistry();
    registry.register([cmd("static", "Static command")]);
    registry.addProvider(() => [cmd("dyn.1", "Switch to tab: orders.csv")]);
    const results = registry.search("orders");
    expect(results.some((r) => r.command.id === "dyn.1")).toBe(true);
  });

  it("stays responsive with 500 registered commands", () => {
    const registry = new CommandRegistry();
    registry.register(
      Array.from({ length: 500 }, (_, i) =>
        cmd(`bulk.${i}`, `Bulk command number ${i}`, { keywords: [`kw${i}`, "shared"] }),
      ),
    );
    const start = performance.now();
    for (let i = 0; i < 20; i++) registry.search("bulk command 4");
    const elapsed = performance.now() - start;
    expect(registry.search("bulk command 42").length).toBeGreaterThan(0);
    // 20 searches over 500 commands well under a second keeps typing smooth.
    expect(elapsed).toBeLessThan(1000);
  });
});

describe("shortcut bindings (F11)", () => {
  it("normalizes modifier order and rejects malformed bindings", () => {
    expect(normalizeBinding("shift+mod+s")).toBe("mod+shift+s");
    expect(normalizeBinding("F2")).toBe("f2");
    expect(normalizeBinding("mod+banana")).toBeNull();
    expect(normalizeBinding("bogus+k")).toBeNull();
  });

  it("derives portable bindings from keyboard events", () => {
    const event = (init: Partial<KeyboardEvent> & { key: string }) =>
      ({
        ctrlKey: false,
        metaKey: false,
        altKey: false,
        shiftKey: false,
        ...init,
      }) as KeyboardEvent;
    expect(bindingFromEvent(event({ key: "k", ctrlKey: true }))).toBe("mod+k");
    expect(bindingFromEvent(event({ key: "S", ctrlKey: true, shiftKey: true }))).toBe(
      "mod+shift+s",
    );
    expect(bindingFromEvent(event({ key: "F2" }))).toBe("f2");
    // Plain letters without modifiers are typing, not shortcuts.
    expect(bindingFromEvent(event({ key: "a" }))).toBeNull();
    // Modifier keydowns themselves never form a binding.
    expect(bindingFromEvent(event({ key: "Control", ctrlKey: true }))).toBeNull();
  });

  it("labels bindings for the current platform", () => {
    expect(bindingLabel("mod+shift+s")).toMatch(/Ctrl\+Shift\+S|⌘⇧S/);
  });

  it("applies overrides over defaults, honours unbinds, ignores junk", () => {
    const defaults = new Map([
      ["file.save", "mod+s"],
      ["edit.undo", "mod+z"],
    ]);
    const effective = effectiveBindings(defaults, {
      "file.save": "mod+shift+x",
      "edit.undo": null,
      "edit.redo": "notakey+++",
    });
    expect(effective.get("file.save")).toBe("mod+shift+x");
    expect(effective.has("edit.undo")).toBe(false);
    expect(effective.has("edit.redo")).toBe(false);
  });

  it("finds binding conflicts excluding the command being edited", () => {
    const bindings = new Map([
      ["file.save", "mod+s"],
      ["edit.undo", "mod+z"],
    ]);
    expect(findConflict(bindings, "mod+s")).toBe("file.save");
    expect(findConflict(bindings, "mod+s", "file.save")).toBeNull();
    expect(findConflict(bindings, "mod+q")).toBeNull();
  });
});

describe("cell references (F11 go-to)", () => {
  it("parses rows, A1 refs, and row,column pairs as zero-based", () => {
    expect(parseCellRef("120")).toEqual({ row: 119, col: 0 });
    expect(parseCellRef("C7")).toEqual({ row: 6, col: 2 });
    expect(parseCellRef("aa3")).toEqual({ row: 2, col: 26 });
    expect(parseCellRef("4,2")).toEqual({ row: 3, col: 1 });
    expect(parseCellRef("4 : 2")).toEqual({ row: 3, col: 1 });
    expect(parseCellRef("")).toBeNull();
    expect(parseCellRef("nope")).toBeNull();
    // References are 1-based: zero must be rejected, never mapped to -1.
    expect(parseCellRef("0")).toBeNull();
    expect(parseCellRef("A0")).toBeNull();
    expect(parseCellRef("0,5")).toBeNull();
    expect(parseCellRef("5,0")).toBeNull();
  });

  it("hides shortcut-only aliases from search but keeps them registered", () => {
    const reg = new CommandRegistry();
    reg.register([
      cmd("edit.redo", "Redo", { defaultShortcut: "mod+y" }),
      cmd("edit.redoAlt", "Redo", { defaultShortcut: "mod+shift+z", hidden: true }),
    ]);
    expect(reg.search("").map((r) => r.command.id)).toEqual(["edit.redo"]);
    expect(reg.defaultBindings().get("edit.redoAlt")).toBe("mod+shift+z");
    expect(reg.byId("edit.redoAlt")).toBeDefined();
  });
});
