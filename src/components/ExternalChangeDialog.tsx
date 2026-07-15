import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Prompt shown when a document's backing file changed (or vanished) outside
 * CEESVEE. A dirty document is never reloaded automatically: the user chooses
 * between inspecting the disk copy, saving elsewhere, or ignoring.
 */
export function ExternalChangeDialog() {
  const prompt = useStore((s) => s.externalPrompt);
  const meta = useStore((s) => s.tabs.find((t) => t.id === s.externalPrompt?.docId) ?? null);
  const resolve = useStore((s) => s.resolveExternalPrompt);

  if (!prompt || !meta) return null;
  const { change } = prompt;

  return (
    <Modal title="File changed on disk" onClose={() => void resolve("ignore")}>
      <div className="space-y-3 text-sm">
        <p>
          <span className="font-medium">{meta.fileName}</span>{" "}
          {change.exists
            ? "was modified outside CEESVEE."
            : "was deleted or moved outside CEESVEE."}
        </p>
        {meta.dirty ? (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            This tab has unsaved edits, so it will not be reloaded automatically.
          </p>
        ) : (
          change.exists && (
            <p className="text-xs text-zinc-500 dark:text-zinc-400">
              This tab has no unsaved edits — it can be reloaded safely.
            </p>
          )
        )}

        <div className="flex flex-wrap justify-end gap-2 pt-1">
          <button onClick={() => void resolve("ignore")} className={btnGhost}>
            Ignore
          </button>
          {meta.dirty ? (
            <>
              <button onClick={() => void resolve("saveAs")} className={btnGhost}>
                Save As…
              </button>
              {change.exists && (
                <button
                  onClick={() => void resolve("openDisk")}
                  className={btnPrimary}
                  title="Open the on-disk version in a new tab, next to your edited copy"
                >
                  Compare with disk
                </button>
              )}
            </>
          ) : (
            change.exists && (
              <button onClick={() => void resolve("reload")} className={btnPrimary}>
                Reload
              </button>
            )
          )}
        </div>
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary = "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500";
