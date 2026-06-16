import { useState } from "react";

import { useActiveMeta, useStore } from "../store/useStore";
import {
  ChevronDown,
  ColumnPlus,
  Download,
  FilePlus,
  FolderOpen,
  Moon,
  Redo,
  RowPlus,
  Save,
  Search,
  SortIcon,
  Sun,
  Trash,
  Undo,
} from "./Icons";

interface ToolbarProps {
  onSort: () => void;
  onExport: () => void;
}

export function Toolbar({ onSort, onExport }: ToolbarProps) {
  const meta = useActiveMeta();
  const theme = useStore((s) => s.theme);
  const recent = useStore((s) => s.recent);
  const [recentOpen, setRecentOpen] = useState(false);

  const newDoc = useStore((s) => s.newDoc);
  const openDialog = useStore((s) => s.openDialog);
  const openPath = useStore((s) => s.openPath);
  const saveActive = useStore((s) => s.saveActive);
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);
  const insertRows = useStore((s) => s.insertRows);
  const deleteRows = useStore((s) => s.deleteRows);
  const insertColumn = useStore((s) => s.insertColumn);
  const setFindOpen = useStore((s) => s.setFindOpen);
  const findIsOpen = useStore((s) => s.find.open);
  const setTheme = useStore((s) => s.setTheme);

  const hasDoc = meta !== null;

  const addRow = () => {
    const { selectedRows } = useStore.getState();
    const at = selectedRows.length ? Math.max(...selectedRows) + 1 : (meta?.rowCount ?? 0);
    void insertRows(at, 1);
  };

  const removeRows = () => {
    const { selectedRows } = useStore.getState();
    if (selectedRows.length) void deleteRows(selectedRows);
  };

  const addColumn = () => {
    if (!meta) return;
    void insertColumn(meta.colCount, `Column ${meta.colCount + 1}`);
  };

  const cycleTheme = () => {
    const next = theme === "dark" ? "light" : "dark";
    setTheme(next);
  };

  return (
    <div className="flex h-11 shrink-0 items-center gap-1 border-b border-zinc-200 bg-zinc-50/80 px-2 backdrop-blur dark:border-zinc-800 dark:bg-zinc-900/80">
      <span className="mr-1 select-none px-1 font-semibold tracking-tight text-violet-600 dark:text-violet-400">
        CEESVEE
      </span>

      <Tool title="New (Ctrl+N)" onClick={() => void newDoc()}>
        <FilePlus />
      </Tool>

      <div className="relative flex">
        <Tool title="Open (Ctrl+O)" onClick={() => void openDialog()}>
          <FolderOpen />
        </Tool>
        <button
          title="Recent files"
          onClick={() => setRecentOpen((o) => !o)}
          disabled={recent.length === 0}
          className="flex items-center rounded px-0.5 text-zinc-500 hover:bg-zinc-200 disabled:opacity-30 dark:hover:bg-zinc-700"
        >
          <ChevronDown className="h-3.5 w-3.5" />
        </button>
        {recentOpen && recent.length > 0 && (
          <div
            className="absolute left-0 top-10 z-40 w-80 overflow-hidden rounded-lg border border-zinc-200 bg-white py-1 text-sm shadow-xl dark:border-zinc-700 dark:bg-zinc-800"
            onMouseLeave={() => setRecentOpen(false)}
          >
            {recent.map((path) => (
              <button
                key={path}
                onClick={() => {
                  setRecentOpen(false);
                  void openPath(path);
                }}
                className="block w-full truncate px-3 py-1.5 text-left text-zinc-700 hover:bg-zinc-100 dark:text-zinc-200 dark:hover:bg-zinc-700"
                dir="rtl"
                title={path}
              >
                {path}
              </button>
            ))}
          </div>
        )}
      </div>

      <Tool title="Save (Ctrl+S)" onClick={() => void saveActive(false)} disabled={!hasDoc}>
        <Save />
      </Tool>

      <Divider />

      <Tool title="Undo (Ctrl+Z)" onClick={() => void undo()} disabled={!meta?.canUndo}>
        <Undo />
      </Tool>
      <Tool title="Redo (Ctrl+Y)" onClick={() => void redo()} disabled={!meta?.canRedo}>
        <Redo />
      </Tool>

      <Divider />

      <Tool title="Insert row" onClick={addRow} disabled={!hasDoc}>
        <RowPlus />
      </Tool>
      <Tool title="Delete selected rows" onClick={removeRows} disabled={!hasDoc}>
        <Trash />
      </Tool>
      <Tool title="Add column" onClick={addColumn} disabled={!hasDoc}>
        <ColumnPlus />
      </Tool>

      <Divider />

      <Tool
        title="Find & replace (Ctrl+F)"
        onClick={() => setFindOpen(!findIsOpen)}
        active={findIsOpen}
        disabled={!hasDoc}
      >
        <Search />
      </Tool>
      <Tool title="Sort…" onClick={onSort} disabled={!hasDoc}>
        <SortIcon />
      </Tool>
      <Tool title="Export / Save As…" onClick={onExport} disabled={!hasDoc}>
        <Download />
      </Tool>

      <div className="flex-1" />

      <Tool title="Toggle theme" onClick={cycleTheme}>
        {theme === "dark" ? <Sun /> : <Moon />}
      </Tool>
    </div>
  );
}

function Tool({
  children,
  title,
  onClick,
  disabled,
  active,
}: {
  children: React.ReactNode;
  title: string;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={`flex h-8 w-8 items-center justify-center rounded text-zinc-600 transition-colors hover:bg-zinc-200 disabled:cursor-default disabled:opacity-30 dark:text-zinc-300 dark:hover:bg-zinc-700 ${
        active ? "bg-violet-100 text-violet-700 dark:bg-violet-500/20 dark:text-violet-300" : ""
      }`}
    >
      {children}
    </button>
  );
}

function Divider() {
  return <div className="mx-1 h-5 w-px bg-zinc-300 dark:bg-zinc-700" />;
}
