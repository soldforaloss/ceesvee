import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Confirmation shown when closing a project with unsaved workspace changes
 * (F37). "Save and close" saves the project first (prompting for a location if
 * it was never saved) and only closes if that succeeds.
 */
export function ProjectCloseDialog() {
  const open = useStore((s) => s.projectClosePromptOpen);
  const project = useStore((s) => s.project);
  const set = useStore.setState;
  const projectSave = useStore((s) => s.projectSave);
  const closeNow = useStore((s) => s.closeProjectNow);

  if (!open || !project) return null;

  const saveAndClose = async () => {
    const saved = await projectSave(false);
    if (saved) await closeNow();
  };

  return (
    <Modal
      title="Close project?"
      onClose={() => set({ projectClosePromptOpen: false })}
      footer={
        <>
          <button onClick={() => set({ projectClosePromptOpen: false })} className={btnGhost}>
            Cancel
          </button>
          <button onClick={() => void closeNow()} className={btnDanger}>
            Discard changes
          </button>
          <button onClick={() => void saveAndClose()} className={btnPrimary}>
            Save and close
          </button>
        </>
      }
    >
      <div className="space-y-2 text-sm">
        <p>
          The project <span className="font-medium">{project.name}</span> has unsaved workspace
          changes (documents, tab order, or layout).
        </p>
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          Closing keeps your documents open — it only ends their association with this project.
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
