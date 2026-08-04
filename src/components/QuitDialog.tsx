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
  const isProjectDirty = useStore((s) => s.isProjectDirty);
  const project = useStore((s) => s.project);
  const projectDirty = isProjectDirty();

  if (!open) return null;

  const docCount = dirtyTabs.length;

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
        {docCount > 0 && (
          <>
            <p>
              {docCount === 1
                ? "One document has unsaved changes:"
                : `${docCount} documents have unsaved changes:`}
            </p>
            <ul className="max-h-40 overflow-y-auto rounded border border-zinc-200 px-3 py-1.5 text-xs dark:border-zinc-800">
              {dirtyTabs.map((t) => (
                <li key={t.id} className="truncate py-0.5" title={t.path ?? t.fileName}>
                  ● {t.fileName}
                </li>
              ))}
            </ul>
          </>
        )}
        {projectDirty && project && (
          <p className="rounded border border-violet-200 bg-violet-50 px-3 py-1.5 text-xs text-violet-800 dark:border-violet-500/30 dark:bg-violet-500/10 dark:text-violet-200">
            The project “{project.name}” also has unsaved workspace changes.
          </p>
        )}
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          “Save all” asks for a location for any document (or project) that has never been saved,
          and quitting is cancelled if a save fails.
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
