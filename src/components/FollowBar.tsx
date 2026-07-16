import { followAlertMessage } from "../lib/follow";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";

/**
 * The follow-mode banner (F19): live row counter, pause/resume, jump to
 * newest, filter to new rows, and stop. Alerts (truncation/rotation/width/
 * encoding) pause the watcher and surface here with restart options.
 */
export function FollowBar() {
  const meta = useActiveMeta();
  const followState = useStore((s) => s.followState);
  const togglePause = useStore((s) => s.toggleFollowPause);
  const stopFollowing = useStore((s) => s.stopFollowing);
  const startFollowFile = useStore((s) => s.startFollowFile);
  const closeTab = useStore((s) => s.closeTab);
  const jumpToCell = useStore((s) => s.jumpToCell);
  const refresh = useStore((s) => s.refreshActiveDoc);

  if (!meta?.follow) return null;
  const state = followState[meta.id];
  if (!state) return null;

  const restart = async () => {
    const path = meta.path;
    await closeTab(meta.id);
    if (path) await startFollowFile(path);
  };

  const filterNew = async () => {
    await api.setRowRangeFilter(meta.id, state.baselineRows);
    await refresh();
  };

  return (
    <div
      className={`flex flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs ${
        state.alert
          ? "border-red-200 bg-red-50 text-red-700 dark:border-red-900/60 dark:bg-red-950/40 dark:text-red-300"
          : "border-sky-200 bg-sky-50 text-sky-700 dark:border-sky-900/60 dark:bg-sky-950/40 dark:text-sky-300"
      }`}
    >
      <span className="font-medium">Following</span>
      {state.alert ? (
        <>
          <span>{followAlertMessage(state.alert)}</span>
          <span className="flex-1" />
          <button onClick={() => void restart()} className={chip}>
            Restart from the new file
          </button>
          <button onClick={() => void stopFollowing(meta.id)} className={chip}>
            Stop following
          </button>
        </>
      ) : (
        <>
          <span>
            {state.newRows.toLocaleString()} new row{state.newRows === 1 ? "" : "s"} since open
            {state.paused && " · paused"}
          </span>
          <span className="flex-1" />
          <button onClick={() => void togglePause(meta.id)} className={chip}>
            {state.paused ? "Resume" : "Pause"}
          </button>
          <button
            onClick={() => void jumpToCell(Math.max(0, meta.totalRowCount - 1), 0)}
            className={chip}
          >
            Jump to newest
          </button>
          <button onClick={() => void filterNew()} disabled={state.newRows === 0} className={chip}>
            Only new rows
          </button>
          <button onClick={() => void stopFollowing(meta.id)} className={chip}>
            Stop
          </button>
        </>
      )}
    </div>
  );
}

const chip =
  "rounded border border-current/30 px-1.5 py-0.5 hover:bg-white/40 disabled:opacity-40 dark:hover:bg-black/20";
