import { save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { useEffect, useState } from "react";

import { changeKindLabel, changeReportJson, changeTime } from "../lib/changes";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { ChangeSummary } from "../types";
import { Close } from "./Icons";

/**
 * Change inspector (F15): everything changed since the last save, with
 * selective revert. Every revert is a NEW operation on the ordinary undo
 * stack, so reverting is itself undoable; structural operations block
 * earlier selective reverts (Revert all stays available). Saving clears
 * the list.
 */
export function ChangesPanel() {
  const meta = useActiveMeta();
  const setOpen = useStore((s) => s.setChangesOpen);
  const refresh = useStore((s) => s.refreshActiveDoc);
  const jumpToCell = useStore((s) => s.jumpToCell);

  const [changes, setChanges] = useState<ChangeSummary[]>([]);
  const [savedInRedo, setSavedInRedo] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  const [revertColumn, setRevertColumn] = useState(0);

  const docId = meta?.id;
  const revision = meta?.revision;
  useEffect(() => {
    if (docId == null) return;
    let cancelled = false;
    api
      .getChanges(docId)
      .then((report) => {
        if (cancelled) return;
        setChanges(report.changes);
        setSavedInRedo(report.savedInRedo);
      })
      .catch((e) => !cancelled && setError(String(e)));
    return () => {
      cancelled = true;
    };
  }, [docId, revision]);

  if (!meta) return null;
  const headers = meta.headers.map((h, i) => h || `Column ${i + 1}`);

  const act = async (call: () => Promise<unknown>) => {
    setWorking(true);
    setError(null);
    try {
      await call();
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const exportReport = async () => {
    const chosen = await saveFileDialog({
      defaultPath: "change-report.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof chosen === "string") {
      await writeTextFile(chosen, changeReportJson(meta.fileName, changes));
    }
  };

  return (
    <aside className="flex w-80 shrink-0 flex-col border-l border-zinc-200 bg-white text-sm dark:border-zinc-800 dark:bg-zinc-900">
      <div className="flex items-center gap-2 border-b border-zinc-200 px-3 py-2 dark:border-zinc-800">
        <span className="font-medium">Changes since save</span>
        <span className="text-xs text-zinc-400">{changes.length}</span>
        <span className="flex-1" />
        <button
          onClick={() => void exportReport()}
          disabled={changes.length === 0}
          title="Export a JSON change report"
          className="rounded px-1.5 py-0.5 text-xs text-zinc-500 hover:bg-zinc-100 disabled:opacity-40 dark:hover:bg-zinc-800"
        >
          Export…
        </button>
        <button
          onClick={() => setOpen(false)}
          className="rounded p-0.5 text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          <Close className="h-4 w-4" />
        </button>
      </div>

      {error && <p className="px-3 py-2 text-xs text-red-600 dark:text-red-400">{error}</p>}

      <div className="min-h-0 flex-1 space-y-1.5 overflow-y-auto p-2">
        {changes.length === 0 && savedInRedo && (
          <p className="rounded bg-amber-50 px-2 py-2 text-center text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            You undid past the last save — the saved state is ahead of the current history. Redo
            returns to it; a new edit will branch away from it permanently.
          </p>
        )}
        {changes.length === 0 && !savedInRedo && (
          <p className="py-6 text-center text-xs text-zinc-400">
            No unsaved changes — the document matches its last save.
          </p>
        )}
        {changes.map((c) => (
          <div
            key={c.id}
            className="rounded border border-zinc-200 p-2 text-xs dark:border-zinc-800"
          >
            <div className="flex items-center gap-2">
              <span
                className={`rounded px-1.5 py-0.5 text-[11px] ${
                  c.structural
                    ? "bg-amber-100 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300"
                    : "bg-violet-100 text-violet-700 dark:bg-violet-500/15 dark:text-violet-300"
                }`}
              >
                {changeKindLabel(c.kind)}
              </span>
              <span className="text-zinc-400">{changeTime(c.epochSecs)}</span>
              {c.cellsAffected > 0 && (
                <span className="text-zinc-400">
                  {c.cellsAffected.toLocaleString()} cell{c.cellsAffected === 1 ? "" : "s"}
                </span>
              )}
              <span className="flex-1" />
              <button
                onClick={() => void act(() => api.revertChange(meta.id, c.id, meta.revision))}
                disabled={working || !c.revertible}
                title={c.blockedReason ?? "Revert this whole operation (undoable)"}
                className="rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700"
              >
                Revert
              </button>
            </div>
            {c.sample.length > 0 && (
              <div className="mt-1 space-y-0.5">
                {c.sample.map((cell, i) => (
                  <div key={i} className="flex items-center gap-1.5 font-mono text-[11px]">
                    <button
                      onClick={() => void jumpToCell(cell.row, cell.col)}
                      title="Jump to this cell"
                      className="rounded border border-zinc-200 px-1 py-0 hover:border-violet-400 dark:border-zinc-700"
                    >
                      {cell.row + 1}:{headers[cell.col] ?? cell.col}
                    </button>
                    <span className="truncate text-zinc-400" title={`${cell.old} → ${cell.new}`}>
                      {cell.old === "" ? "∅" : cell.old} → {cell.new === "" ? "∅" : cell.new}
                    </span>
                    <button
                      onClick={() =>
                        navigator.clipboard.writeText(`${cell.old}\t${cell.new}`).catch(() => {})
                      }
                      title="Copy before/after"
                      className="text-zinc-400 hover:text-violet-500"
                    >
                      ⧉
                    </button>
                    {c.revertible && !c.structural && (
                      <button
                        onClick={() =>
                          void act(() =>
                            api.revertChangeCells(
                              meta.id,
                              c.id,
                              [[cell.row, cell.col]],
                              meta.revision,
                            ),
                          )
                        }
                        disabled={working}
                        title="Revert just this cell (undoable)"
                        className="text-zinc-400 hover:text-red-500"
                      >
                        ↩
                      </button>
                    )}
                  </div>
                ))}
                {c.cellsAffected > c.sample.length && (
                  <p className="text-[11px] text-zinc-400">
                    …and {(c.cellsAffected - c.sample.length).toLocaleString()} more
                  </p>
                )}
              </div>
            )}
          </div>
        ))}
      </div>

      <div className="space-y-1.5 border-t border-zinc-200 p-2 text-xs dark:border-zinc-800">
        <div className="flex items-center gap-1.5">
          <select
            value={revertColumn}
            onChange={(e) => setRevertColumn(Number(e.target.value))}
            className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-1 py-0.5 outline-none dark:border-zinc-700"
          >
            {headers.map((h, i) => (
              <option key={i} value={i} className="dark:bg-zinc-800">
                {h}
              </option>
            ))}
          </select>
          <button
            onClick={() =>
              void act(() => api.revertColumnChanges(meta.id, revertColumn, meta.revision))
            }
            disabled={working || changes.length === 0}
            className="rounded border border-zinc-200 px-1.5 py-0.5 hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700"
          >
            Revert column
          </button>
        </div>
        <button
          onClick={() => void act(() => api.revertAllChanges(meta.id, meta.revision))}
          disabled={working || changes.length === 0}
          className="w-full rounded bg-red-600 px-2 py-1 text-white hover:bg-red-500 disabled:opacity-40"
        >
          Revert all changes (one undo step)
        </button>
      </div>
    </aside>
  );
}
