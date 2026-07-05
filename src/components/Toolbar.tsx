import { useState } from "react";

import { checkForUpdates } from "../lib/updater";
import { useActiveMeta, useStore } from "../store/useStore";
import {
  ChevronDown,
  ColumnPlus,
  Dots,
  Download,
  FilePlus,
  Filter,
  FolderOpen,
  Moon,
  Redo,
  Refresh,
  RowPlus,
  Save,
  Search,
  SortIcon,
  Stats,
  Sun,
  Trash,
  Undo,
} from "./Icons";
import { Logo } from "./Logo";

interface ToolbarProps {
  onSort: () => void;
  onExport: () => void;
  onSummaries: () => void;
  onFilter: () => void;
}

interface ToolItem {
  label: string;
  title?: string;
  icon: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
}

export function Toolbar({ onSort, onExport, onSummaries, onFilter }: ToolbarProps) {
  const meta = useActiveMeta();
  const theme = useStore((s) => s.theme);
  const recent = useStore((s) => s.recent);
  const [recentOpen, setRecentOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);

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

  // These two groups render inline on wide windows and collapse into the
  // "More tools" menu on narrow ones, so nothing gets clipped.
  const rowColumnTools: ToolItem[] = [
    { label: "Insert row", icon: <RowPlus />, onClick: addRow, disabled: !hasDoc },
    { label: "Delete selected rows", icon: <Trash />, onClick: removeRows, disabled: !hasDoc },
    { label: "Add column", icon: <ColumnPlus />, onClick: addColumn, disabled: !hasDoc },
  ];

  const dataTools: ToolItem[] = [
    {
      label: "Find & replace",
      title: "Find & replace (Ctrl+F)",
      icon: <Search />,
      onClick: () => setFindOpen(!findIsOpen),
      active: findIsOpen,
      disabled: !hasDoc,
    },
    {
      label: "Filter rows…",
      icon: <Filter />,
      onClick: onFilter,
      active: !!meta?.filtered,
      disabled: !hasDoc,
    },
    { label: "Sort…", icon: <SortIcon />, onClick: onSort, disabled: !hasDoc },
    { label: "Column summaries", icon: <Stats />, onClick: onSummaries, disabled: !hasDoc },
    { label: "Export / Save As…", icon: <Download />, onClick: onExport, disabled: !hasDoc },
  ];

  return (
    <div className="z-30 flex h-11 shrink-0 items-center gap-1 border-b border-zinc-200 bg-zinc-50/80 px-2 backdrop-blur dark:border-zinc-800 dark:bg-zinc-900/80">
      <span className="mr-1 flex select-none items-center gap-1.5 px-1 font-semibold tracking-tight text-violet-600 dark:text-violet-400">
        <Logo className="h-5 w-5" />
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

      <div className="hidden items-center gap-1 md:flex">
        <Divider />
        {rowColumnTools.map((t) => (
          <Tool
            key={t.label}
            title={t.title ?? t.label}
            onClick={t.onClick}
            disabled={t.disabled}
            active={t.active}
          >
            {t.icon}
          </Tool>
        ))}
        <Divider />
        {dataTools.map((t) => (
          <Tool
            key={t.label}
            title={t.title ?? t.label}
            onClick={t.onClick}
            disabled={t.disabled}
            active={t.active}
          >
            {t.icon}
          </Tool>
        ))}
      </div>

      <div className="flex items-center gap-1 md:hidden">
        <Divider />
        <div className="relative flex">
          <Tool title="More tools" onClick={() => setMoreOpen((o) => !o)} active={moreOpen}>
            <Dots />
          </Tool>
          {moreOpen && (
            <div
              className="absolute left-0 top-10 z-40 w-60 overflow-hidden rounded-lg border border-zinc-200 bg-white py-1 text-sm shadow-xl dark:border-zinc-700 dark:bg-zinc-800"
              onMouseLeave={() => setMoreOpen(false)}
            >
              <MenuGroup
                label="Rows & columns"
                items={rowColumnTools}
                onPick={() => setMoreOpen(false)}
              />
              <MenuGroup label="Data" items={dataTools} onPick={() => setMoreOpen(false)} />
            </div>
          )}
        </div>
      </div>

      <div className="flex-1" />

      <Tool title="Check for updates" onClick={() => void checkForUpdates({ silent: false })}>
        <Refresh />
      </Tool>
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

function MenuGroup({
  label,
  items,
  onPick,
}: {
  label: string;
  items: ToolItem[];
  onPick: () => void;
}) {
  return (
    <>
      <div className="px-3 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500">
        {label}
      </div>
      {items.map((t) => (
        <button
          key={t.label}
          title={t.title ?? t.label}
          disabled={t.disabled}
          onClick={() => {
            onPick();
            t.onClick();
          }}
          className={`flex w-full items-center gap-2.5 px-3 py-1.5 text-left transition-colors hover:bg-zinc-100 disabled:cursor-default disabled:opacity-30 dark:hover:bg-zinc-700 ${
            t.active ? "text-violet-700 dark:text-violet-300" : "text-zinc-700 dark:text-zinc-200"
          }`}
        >
          {t.icon}
          {t.label}
        </button>
      ))}
    </>
  );
}

function Divider() {
  return <div className="mx-1 h-5 w-px bg-zinc-300 dark:bg-zinc-700" />;
}
