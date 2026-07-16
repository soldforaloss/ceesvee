import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Shown when the window close was intercepted while tabs have unsaved edits.
 * "Save all and quit" aborts quitting if any save fails or is cancelled.
 */
export function QuitDialog() {
  const open = useStore((s) => s.quitPromptOpen);
  const dirtyTabs = useStore((s) => s.tabs.filter((t) => t.dirty));
  const setOpen = useStore((s) => s.setQuitPromptOpen);
  const confirmQuit = useStore((s) => s.confirmQuit);

  if (!open) return null;

  return (
    <Modal
      title="Unsaved changes"
      onClose={() => setOpen(false)}
      footer={
        <>
          <button onClick={() => setOpen(false)} className={btnGhost}>
            Cancel
          </button>
          <button onClick={() => void confirmQuit("discard")} className={btnDanger}>
            Discard all and quit
          </button>
          <button onClick={() => void confirmQuit("save")} className={btnPrimary}>
            Save all and quit
          </button>
        </>
      }
    >
      <div className="space-y-2 text-sm">
        <p>
          {dirtyTabs.length === 1
            ? "One document has unsaved changes:"
            : `${dirtyTabs.length} documents have unsaved changes:`}
        </p>
        <ul className="max-h-40 overflow-y-auto rounded border border-zinc-200 px-3 py-1.5 text-xs dark:border-zinc-800">
          {dirtyTabs.map((t) => (
            <li key={t.id} className="truncate py-0.5" title={t.path ?? t.fileName}>
              ● {t.fileName}
            </li>
          ))}
        </ul>
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          “Save all” asks for a location for any document that has never been saved, and quitting is
          cancelled if a save fails.
        </p>
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary = "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500";
const btnDanger =
  "rounded border border-red-300 px-3 py-1.5 text-sm text-red-700 hover:bg-red-50 dark:border-red-500/40 dark:text-red-300 dark:hover:bg-red-500/10";
