// @vitest-environment jsdom
/**
 * Regression test for the blank window on launch (issues #51, #52).
 *
 * QuitDialog and TagPickerDialog are mounted unconditionally by App. With
 * zustand v5 a selector that returns a fresh array/object on every call (e.g.
 * `s.tabs.filter(...)` or `x ?? []`) makes useSyncExternalStore see a changed
 * snapshot on each render, so React throws "Maximum update depth exceeded"
 * and the whole tree unmounts, leaving an empty window.
 */
import { createElement } from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { QuitDialog } from "./QuitDialog";
import { TagPickerDialog } from "./TagPickerDialog";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("dialogs mounted at startup", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = "";
  });

  it.each([
    ["QuitDialog", QuitDialog],
    ["TagPickerDialog", TagPickerDialog],
  ])("%s renders with the initial store without an update loop", (_name, Component) => {
    const errors: unknown[] = [];
    vi.spyOn(console, "error").mockImplementation((...args) => errors.push(args));
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);

    expect(() => act(() => root.render(createElement(Component)))).not.toThrow();
    act(() => root.unmount());

    expect(errors).toEqual([]);
  });
});
