import { useEffect, useRef } from "react";

import { INDEXED_FIND_LIMIT, useActiveMeta, useStore } from "../store/useStore";
import { ChevronDown, ChevronUp, Close } from "./Icons";

export function FindBar() {
  const find = useStore((s) => s.find);
  const meta = useActiveMeta();
  const updateFind = useStore((s) => s.updateFind);
  const runFind = useStore((s) => s.runFind);
  const gotoMatch = useStore((s) => s.gotoMatch);
  const replaceCurrent = useStore((s) => s.replaceCurrent);
  const replaceAllMatches = useStore((s) => s.replaceAllMatches);
  const setFindOpen = useStore((s) => s.setFindOpen);
  const selectionRect = useStore((s) => s.selectionRect);
  const inputRef = useRef<HTMLInputElement>(null);
  // Indexed read-only documents: replacement is a mutation, and matches cap
  // at the streaming limit (F10).
  const readOnly = meta?.backing === "indexedReadOnly";

  useEffect(() => {
    if (find.open) inputRef.current?.focus();
  }, [find.open]);

  // Re-run the search shortly after the query or options change — and, when
  // scoping to a selection, whenever that selection changes.
  const selScope = find.inSelection ? selectionRect : null;
  useEffect(() => {
    if (!find.open) return;
    const handle = setTimeout(() => void runFind(), 200);
    return () => clearTimeout(handle);
  }, [
    find.open,
    find.query,
    find.regex,
    find.caseSensitive,
    find.wholeCell,
    find.inSelection,
    selScope,
    runFind,
  ]);

  if (!find.open) return null;

  const total = find.matches.length;
  const position = total ? find.index + 1 : 0;

  return (
    <div className="flex shrink-0 flex-col gap-2 border-b border-zinc-200 bg-zinc-50 px-3 py-2 text-sm dark:border-zinc-800 dark:bg-zinc-900 sm:flex-row sm:items-center">
      <div className="flex items-center gap-2">
        <input
          ref={inputRef}
          value={find.query}
          placeholder="Find"
          onChange={(e) => updateFind({ query: e.target.value })}
          onKeyDown={(e) => {
            if (e.key === "Enter") gotoMatch(e.shiftKey ? -1 : 1);
            if (e.key === "Escape") setFindOpen(false);
          }}
          className="w-44 rounded border border-zinc-300 bg-white px-2 py-1 outline-none focus:border-violet-500 dark:border-zinc-700 dark:bg-zinc-950"
        />
        <span
          className="w-16 shrink-0 text-center text-xs tabular-nums text-zinc-500"
          title={
            readOnly && total >= INDEXED_FIND_LIMIT
              ? `Showing the first ${INDEXED_FIND_LIMIT.toLocaleString()} matches`
              : undefined
          }
        >
          {position} / {total}
          {readOnly && total >= INDEXED_FIND_LIMIT ? "+" : ""}
        </span>
        <button
          onClick={() => gotoMatch(-1)}
          disabled={!total}
          title="Previous (Shift+Enter)"
          className="rounded p-1 text-zinc-500 hover:bg-zinc-200 disabled:opacity-30 dark:hover:bg-zinc-700"
        >
          <ChevronUp className="h-4 w-4" />
        </button>
        <button
          onClick={() => gotoMatch(1)}
          disabled={!total}
          title="Next (Enter)"
          className="rounded p-1 text-zinc-500 hover:bg-zinc-200 disabled:opacity-30 dark:hover:bg-zinc-700"
        >
          <ChevronDown className="h-4 w-4" />
        </button>

        <Toggle
          label="Aa"
          title="Case sensitive"
          on={find.caseSensitive}
          onClick={() => updateFind({ caseSensitive: !find.caseSensitive })}
        />
        <Toggle
          label="\\b"
          title="Whole cell"
          on={find.wholeCell}
          onClick={() => updateFind({ wholeCell: !find.wholeCell })}
        />
        <Toggle
          label=".*"
          title="Regex"
          on={find.regex}
          onClick={() => updateFind({ regex: !find.regex })}
        />
        <Toggle
          label="⛶"
          title="In selection"
          on={find.inSelection}
          onClick={() => updateFind({ inSelection: !find.inSelection })}
        />
      </div>

      {!readOnly && (
        <div className="flex items-center gap-2">
          <input
            value={find.replacement}
            placeholder="Replace with"
            onChange={(e) => updateFind({ replacement: e.target.value })}
            className="w-44 rounded border border-zinc-300 bg-white px-2 py-1 outline-none focus:border-violet-500 dark:border-zinc-700 dark:bg-zinc-950"
          />
          <button
            onClick={() => void replaceCurrent()}
            disabled={!total}
            className="rounded border border-zinc-300 px-2 py-1 text-xs hover:bg-zinc-200 disabled:opacity-30 dark:border-zinc-700 dark:hover:bg-zinc-700"
          >
            Replace
          </button>
          <button
            onClick={() => void replaceAllMatches()}
            disabled={!find.query}
            className="rounded bg-violet-600 px-2 py-1 text-xs text-white hover:bg-violet-500 disabled:opacity-30"
          >
            Replace all
          </button>
        </div>
      )}

      <button
        onClick={() => setFindOpen(false)}
        className="ml-auto rounded p-1 text-zinc-400 hover:bg-zinc-200 hover:text-zinc-700 dark:hover:bg-zinc-700 dark:hover:text-zinc-200"
        title="Close (Esc)"
      >
        <Close className="h-4 w-4" />
      </button>
    </div>
  );
}

function Toggle({
  label,
  title,
  on,
  onClick,
}: {
  label: string;
  title: string;
  on: boolean;
  onClick: () => void;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      className={`h-7 w-7 rounded font-mono text-xs ${
        on ? "bg-violet-600 text-white" : "text-zinc-500 hover:bg-zinc-200 dark:hover:bg-zinc-700"
      }`}
    >
      {label}
    </button>
  );
}
