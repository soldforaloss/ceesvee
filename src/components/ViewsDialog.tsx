import { useState } from "react";

import { describeView } from "../lib/views";
import { useActiveMeta, useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Named views (F12): save, apply, rename, duplicate, replace and delete
 * reusable non-destructive views of the current document. Views persist in
 * the file's profile and the last-applied one is restored on reopen.
 */
export function ViewsDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const viewsForActive = useStore((s) => s.viewsForActive);
  const applyNamedView = useStore((s) => s.applyNamedView);
  const saveCurrentViewAs = useStore((s) => s.saveCurrentViewAs);
  const replaceNamedView = useStore((s) => s.replaceNamedView);
  const renameNamedView = useStore((s) => s.renameNamedView);
  const duplicateNamedView = useStore((s) => s.duplicateNamedView);
  const deleteNamedView = useStore((s) => s.deleteNamedView);
  const resetView = useStore((s) => s.resetView);
  const activeViewId = useStore((s) => s.activeViewId);
  const viewWarning = useStore((s) => s.viewWarning);
  const dismissViewWarning = useStore((s) => s.dismissViewWarning);
  // Subscribe so the list re-renders after saves persist into settings.
  useStore((s) => s.settings);

  const [newName, setNewName] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");

  if (!meta) return null;
  const { views } = viewsForActive();

  const saveNew = () => {
    void saveCurrentViewAs(newName.trim() || "Untitled view");
    setNewName("");
  };

  const commitRename = (id: string) => {
    const name = renameValue.trim();
    if (name) void renameNamedView(id, name);
    setRenamingId(null);
  };

  const small =
    "rounded px-2 py-0.5 text-xs text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-700";

  return (
    <Modal
      title="Named views"
      onClose={onClose}
      footer={
        <>
          <button
            onClick={() => void resetView()}
            className="mr-auto rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
            title="Clear the filter, view sort, hidden/pinned columns, widths and wrap. Never changes data."
          >
            Reset view
          </button>
          <button
            onClick={onClose}
            className="rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            Close
          </button>
        </>
      }
    >
      <div className="space-y-3">
        {viewWarning && (
          <div className="flex items-start justify-between gap-2 rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-200">
            <span>{viewWarning}</span>
            <button onClick={dismissViewWarning} className="shrink-0 underline">
              Dismiss
            </button>
          </div>
        )}

        <div className="flex gap-2">
          <input
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") saveNew();
            }}
            placeholder="Save current view as…"
            className="flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1.5 text-sm outline-none focus:border-violet-500 dark:border-zinc-700"
          />
          <button
            onClick={saveNew}
            disabled={!meta.path}
            title={meta.path ? undefined : "Save the file first — views are stored per source file"}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            Save view
          </button>
        </div>

        {views.length === 0 ? (
          <p className="py-2 text-sm text-zinc-500 dark:text-zinc-400">
            No saved views for this file yet. A view captures the filter, the non-destructive sort,
            hidden/pinned/reordered columns, widths and wrap — applying one never changes your data.
          </p>
        ) : (
          <ul className="max-h-72 space-y-1 overflow-y-auto">
            {views.map((view) => (
              <li
                key={view.id}
                className={`rounded border px-3 py-2 ${
                  view.id === activeViewId
                    ? "border-violet-400 bg-violet-50/60 dark:border-violet-500/50 dark:bg-violet-500/10"
                    : "border-zinc-200 dark:border-zinc-700/60"
                }`}
              >
                {renamingId === view.id ? (
                  <div className="flex gap-2">
                    <input
                      autoFocus
                      value={renameValue}
                      onChange={(e) => setRenameValue(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") commitRename(view.id);
                        if (e.key === "Escape") setRenamingId(null);
                      }}
                      className="flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-600"
                    />
                    <button className={small} onClick={() => commitRename(view.id)}>
                      Rename
                    </button>
                  </div>
                ) : (
                  <>
                    <div className="flex items-center justify-between gap-2">
                      <div className="min-w-0">
                        <div className="truncate text-sm font-medium">
                          {view.name}
                          {view.id === activeViewId && (
                            <span className="ml-2 text-xs font-normal text-violet-600 dark:text-violet-400">
                              applied
                            </span>
                          )}
                        </div>
                        <div className="truncate text-xs text-zinc-500 dark:text-zinc-400">
                          {describeView(view)}
                        </div>
                      </div>
                      <button
                        onClick={() => void applyNamedView(view)}
                        className="shrink-0 rounded bg-violet-600 px-2.5 py-1 text-xs text-white hover:bg-violet-500"
                      >
                        Apply
                      </button>
                    </div>
                    <div className="mt-1.5 flex gap-1">
                      <button className={small} onClick={() => void replaceNamedView(view.id)}>
                        Replace with current
                      </button>
                      <button
                        className={small}
                        onClick={() => {
                          setRenamingId(view.id);
                          setRenameValue(view.name);
                        }}
                      >
                        Rename
                      </button>
                      <button className={small} onClick={() => void duplicateNamedView(view.id)}>
                        Duplicate
                      </button>
                      <button
                        className="rounded px-2 py-0.5 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
                        onClick={() => void deleteNamedView(view.id)}
                      >
                        Delete
                      </button>
                    </div>
                  </>
                )}
              </li>
            ))}
          </ul>
        )}

        <p className="border-t border-zinc-200 pt-2 text-xs text-zinc-500 dark:border-zinc-700/60 dark:text-zinc-400">
          Save keeps the file's own row and column order. Exports ask explicitly whether to respect
          the view's hidden columns and sort order.
        </p>
      </div>
    </Modal>
  );
}
