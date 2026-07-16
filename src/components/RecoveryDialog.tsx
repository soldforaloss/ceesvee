import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { useState } from "react";

import { recoveryAction, recoveryTime } from "../lib/recovery";
import * as api from "../lib/tauri";
import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Crash recovery (F16): sessions found in the journal directory at startup.
 * Recovering replays the journaled operations onto a fresh parse of the
 * source — the source file is never written — and a changed source blocks
 * blind replay, defaulting to Open Copy. Journaling itself is OPT-IN and
 * carries a privacy disclosure (journals contain edited cell values).
 */
export function RecoveryDialog({ onClose }: { onClose: () => void }) {
  const sessions = useStore((s) => s.recoverySessions);
  const setSessions = useStore((s) => s.setRecoverySessions);
  const adoptRecovered = useStore((s) => s.adoptRecoveredDoc);
  const settings = useStore((s) => s.settings);
  const setRecoveryEnabled = useStore((s) => s.setRecoveryEnabled);

  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    setSessions(await api.listRecoverySessions().catch(() => []));
  };

  const recover = async (journalPath: string, openCopy: boolean) => {
    setWorking(true);
    setError(null);
    try {
      const meta = await api.recoverSession(journalPath, openCopy);
      adoptRecovered(meta);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const discard = async (journalPath: string) => {
    setWorking(true);
    setError(null);
    try {
      await api.discardRecoverySession(journalPath);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const deleteAll = async () => {
    setWorking(true);
    setError(null);
    try {
      await api.deleteAllRecovery();
      setSessions([]);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  return (
    <Modal
      title="Recover unsaved work"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <label className="mr-auto flex items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400">
            <input
              type="checkbox"
              checked={settings?.recoveryEnabled ?? false}
              onChange={(e) => void setRecoveryEnabled(e.target.checked)}
              className="accent-violet-600"
            />
            Keep local recovery journals (may contain edited cell values)
          </label>
          <button
            onClick={() => void deleteAll()}
            disabled={working}
            className="rounded px-3 py-1.5 text-sm text-red-600 hover:bg-red-50 disabled:opacity-40 dark:text-red-400 dark:hover:bg-red-500/10"
          >
            Delete all recovery data
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
      <div className="space-y-3 text-sm">
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          These journals survived an abnormal shutdown. Recovering replays your edits onto a fresh
          copy of the source file — the source itself is never modified.
        </p>

        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}

        {sessions.length === 0 && (
          <p className="py-4 text-center text-xs text-zinc-400">Nothing to recover.</p>
        )}

        <div className="max-h-[46vh] space-y-1.5 overflow-y-auto pr-1">
          {sessions.map((s) => {
            const action = recoveryAction(s);
            return (
              <div
                key={s.journalPath}
                className="rounded border border-zinc-200 p-2 text-xs dark:border-zinc-800"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{s.fileName}</span>
                  <span className="text-zinc-400">{recoveryTime(s.lastEditEpochSecs)}</span>
                  {s.operationCount > 0 && (
                    <span className="text-zinc-400">
                      {s.operationCount.toLocaleString()} operation
                      {s.operationCount === 1 ? "" : "s"}
                    </span>
                  )}
                  {s.incompatible && (
                    <span className="rounded bg-zinc-100 px-1.5 py-0.5 text-[11px] text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
                      incompatible version — kept for manual recovery
                    </span>
                  )}
                  {s.sourceChanged && (
                    <span className="rounded bg-amber-100 px-1.5 py-0.5 text-[11px] text-amber-800 dark:bg-amber-500/15 dark:text-amber-300">
                      source changed since journaling
                    </span>
                  )}
                  {s.sourceMissing && (
                    <span className="rounded bg-red-100 px-1.5 py-0.5 text-[11px] text-red-700 dark:bg-red-500/15 dark:text-red-300">
                      source missing
                    </span>
                  )}
                </div>
                <div className="mt-1.5 flex flex-wrap gap-1.5">
                  {action === "recover" && (
                    <button
                      onClick={() => void recover(s.journalPath, false)}
                      disabled={working}
                      className="rounded bg-violet-600 px-2 py-0.5 text-white hover:bg-violet-500 disabled:opacity-40"
                    >
                      Recover
                    </button>
                  )}
                  {(action === "recover" || action === "openCopy") && (
                    <button
                      onClick={() => void recover(s.journalPath, true)}
                      disabled={working}
                      className={
                        action === "openCopy"
                          ? "rounded bg-violet-600 px-2 py-0.5 text-white hover:bg-violet-500 disabled:opacity-40"
                          : chipBtn
                      }
                    >
                      Open Copy
                    </button>
                  )}
                  <button
                    onClick={() => void discard(s.journalPath)}
                    disabled={working}
                    className="rounded border border-zinc-200 px-1.5 py-0.5 text-red-600 hover:border-red-400 disabled:opacity-40 dark:border-zinc-700 dark:text-red-400"
                  >
                    Discard
                  </button>
                  <button
                    onClick={() => void revealItemInDir(s.journalPath).catch(() => {})}
                    className={chipBtn}
                  >
                    Show Location
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </Modal>
  );
}

const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
