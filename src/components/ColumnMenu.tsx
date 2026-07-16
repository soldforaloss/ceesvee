import { useEffect, useRef, useState } from "react";

import { useStore } from "../store/useStore";

export interface ColumnMenuState {
  col: number;
  x: number;
  y: number;
}

interface ColumnMenuProps {
  state: ColumnMenuState;
  headers: string[];
  onClose: () => void;
}

export function ColumnMenu({ state, headers, onClose }: ColumnMenuProps) {
  const { col, x, y } = state;
  const ref = useRef<HTMLDivElement>(null);
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(headers[col] ?? "");

  const sortBy = useStore((s) => s.sortBy);
  const renameColumn = useStore((s) => s.renameColumn);
  const insertColumn = useStore((s) => s.insertColumn);
  const deleteColumns = useStore((s) => s.deleteColumns);
  const setFrozenCols = useStore((s) => s.setFrozenCols);
  const frozenCols = useStore((s) => s.frozenColumnCount);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const run = (fn: () => void) => {
    fn();
    onClose();
  };

  const commitRename = () => {
    const trimmed = name.trim();
    if (trimmed) void renameColumn(col, trimmed);
    onClose();
  };

  return (
    <div
      ref={ref}
      className="fixed z-50 w-52 overflow-hidden rounded-lg border border-zinc-200 bg-white py-1 text-sm shadow-xl dark:border-zinc-700 dark:bg-zinc-800"
      style={{ left: Math.max(8, x - 200), top: y + 2 }}
    >
      {renaming ? (
        <div className="px-2 py-1.5">
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitRename();
            }}
            className="w-full rounded border border-zinc-300 bg-white px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-600 dark:bg-zinc-900"
            placeholder="Column name"
          />
          <div className="mt-1.5 flex justify-end gap-1">
            <button
              className="rounded px-2 py-0.5 text-xs text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-700"
              onClick={onClose}
            >
              Cancel
            </button>
            <button
              className="rounded bg-violet-600 px-2 py-0.5 text-xs text-white hover:bg-violet-500"
              onClick={commitRename}
            >
              Rename
            </button>
          </div>
        </div>
      ) : (
        <>
          <MenuItem onClick={() => run(() => void sortBy([{ column: col, descending: false }]))}>
            Sort ascending
          </MenuItem>
          <MenuItem onClick={() => run(() => void sortBy([{ column: col, descending: true }]))}>
            Sort descending
          </MenuItem>
          <Divider />
          <MenuItem onClick={() => run(() => setFrozenCols(col + 1))}>Freeze up to here</MenuItem>
          {frozenCols > 0 && (
            <MenuItem onClick={() => run(() => setFrozenCols(0))}>Unfreeze columns</MenuItem>
          )}
          <Divider />
          <MenuItem onClick={() => setRenaming(true)}>Rename…</MenuItem>
          <MenuItem
            onClick={() => run(() => void insertColumn(col, `Column ${headers.length + 1}`))}
          >
            Insert column left
          </MenuItem>
          <MenuItem
            onClick={() => run(() => void insertColumn(col + 1, `Column ${headers.length + 1}`))}
          >
            Insert column right
          </MenuItem>
          <Divider />
          <MenuItem danger onClick={() => run(() => void deleteColumns([col]))}>
            Delete column
          </MenuItem>
        </>
      )}
    </div>
  );
}

function MenuItem({
  children,
  onClick,
  danger,
}: {
  children: React.ReactNode;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={`block w-full px-3 py-1.5 text-left hover:bg-zinc-100 dark:hover:bg-zinc-700 ${
        danger ? "text-red-600 dark:text-red-400" : "text-zinc-700 dark:text-zinc-200"
      }`}
    >
      {children}
    </button>
  );
}

function Divider() {
  return <div className="my-1 border-t border-zinc-200 dark:border-zinc-700/60" />;
}
