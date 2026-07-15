import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DocumentMeta } from "../types";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ destroy: vi.fn(), onCloseRequested: vi.fn() }),
}));
vi.mock("../lib/tauri", () => ({
  closeDocument: vi.fn().mockResolvedValue(undefined),
}));

import { useStore } from "./useStore";

function meta(id: number): DocumentMeta {
  return {
    id,
    path: `C:/data/doc-${id}.csv`,
    fileName: `doc-${id}.csv`,
    rowCount: 100,
    totalRowCount: 100,
    filtered: false,
    colCount: 3,
    headers: ["a", "b", "c"],
    hasHeaderRow: true,
    delimiter: ",",
    encoding: "UTF-8",
    hadBom: false,
    lineEnding: "lf",
    dirty: false,
    canUndo: false,
    canRedo: false,
    revision: 1,
  };
}

describe("per-document UI state (F08)", () => {
  beforeEach(() => {
    useStore.setState({
      tabs: [meta(1), meta(2)],
      activeId: 1,
      uiStates: {},
      find: { ...useStore.getState().find, query: "", open: false, matches: [], index: 0 },
      columnWidths: {},
      frozenColumnCount: 0,
      selectionRect: null,
      selectedRows: [],
      selectedCols: [],
      scrollPosition: { row: 0, column: 0 },
    });
  });

  it("keeps find, widths, frozen, selection and scroll independent per tab", () => {
    const s = useStore.getState();

    // Configure document 1's view.
    s.updateFind({ query: "alpha", open: true });
    s.setColumnWidth(0, 333);
    s.setFrozenCols(2);
    useStore.setState({
      selectionRect: { x: 1, y: 2, width: 3, height: 4 },
      selectedRows: [2, 3],
      selectedCols: [1],
      scrollPosition: { row: 50, column: 3 },
    });

    // Switching to document 2 exposes fresh defaults, not document 1's state.
    useStore.getState().setActive(2);
    let now = useStore.getState();
    expect(now.find.query).toBe("");
    expect(now.columnWidths).toEqual({});
    expect(now.frozenColumnCount).toBe(0);
    expect(now.selectionRect).toBeNull();
    expect(now.scrollPosition).toEqual({ row: 0, column: 0 });

    // Configure document 2 differently.
    useStore.getState().updateFind({ query: "beta" });
    useStore.getState().setColumnWidth(1, 99);

    // Back to document 1: everything restored exactly.
    useStore.getState().setActive(1);
    now = useStore.getState();
    expect(now.find.query).toBe("alpha");
    expect(now.find.open).toBe(true);
    expect(now.columnWidths).toEqual({ 0: 333 });
    expect(now.frozenColumnCount).toBe(2);
    expect(now.selectionRect).toEqual({ x: 1, y: 2, width: 3, height: 4 });
    expect(now.selectedRows).toEqual([2, 3]);
    expect(now.selectedCols).toEqual([1]);
    expect(now.scrollPosition).toEqual({ row: 50, column: 3 });

    // And document 2's own state is intact too.
    useStore.getState().setActive(2);
    now = useStore.getState();
    expect(now.find.query).toBe("beta");
    expect(now.columnWidths).toEqual({ 1: 99 });
  });

  it("filter-builder contents do not leak between documents", () => {
    const s = useStore.getState();
    s.updateFilterSpec({
      type: "group",
      id: "root",
      conjunction: "and",
      nodes: [
        {
          type: "condition",
          id: "c0",
          column: 2,
          op: "equals",
          value: "doc1-only",
          caseSensitive: false,
        },
      ],
    });
    useStore.getState().setActive(2);
    const specB = useStore.getState().filter.spec;
    expect(JSON.stringify(specB)).not.toContain("doc1-only");
    useStore.getState().setActive(1);
    const specA = useStore.getState().filter.spec;
    expect(JSON.stringify(specA)).toContain("doc1-only");
  });

  it("closing a tab removes its transient state", async () => {
    const s = useStore.getState();
    s.updateFind({ query: "alpha" });
    s.setActive(2);
    expect(useStore.getState().uiStates[1]).toBeDefined();

    await useStore.getState().closeTab(1);
    const now = useStore.getState();
    expect(now.uiStates[1]).toBeUndefined();
    expect(now.tabs.map((t) => t.id)).toEqual([2]);
    expect(now.activeId).toBe(2);
  });

  it("closing the active tab restores the neighbour's state", async () => {
    // Configure doc 1, switch to doc 2, configure it, then close doc 2.
    useStore.getState().updateFind({ query: "alpha" });
    useStore.getState().setActive(2);
    useStore.getState().updateFind({ query: "beta" });

    await useStore.getState().closeTab(2);
    const now = useStore.getState();
    expect(now.activeId).toBe(1);
    expect(now.find.query).toBe("alpha");
    expect(now.uiStates[2]).toBeUndefined();
  });
});
