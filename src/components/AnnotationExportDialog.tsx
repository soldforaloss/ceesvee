import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Explicit annotation export (F40). Notes, tags and marks never leave through
 * an ordinary data export, so exporting them is a deliberate action offered
 * here in two formats: versioned JSON (round-trippable) or flat CSV (one row
 * per marked record / cell note, for a spreadsheet).
 */
export function AnnotationExportDialog({ onClose }: { onClose: () => void }) {
  const exportToFile = useStore((s) => s.exportAnnotationsToFile);
  const view = useStore((s) => s.annotationsView);
  const total = view?.entries.length ?? 0;

  const run = (format: "json" | "csv") => {
    void exportToFile(format);
    onClose();
  };

  return (
    <Modal
      title="Export annotations"
      onClose={onClose}
      footer={
        <button
          onClick={onClose}
          className="rounded border border-zinc-200 px-3 py-1.5 text-sm hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
        >
          Cancel
        </button>
      }
    >
      <p className="mb-3 text-sm text-zinc-600 dark:text-zinc-300">
        Save this document’s {total} annotation{total === 1 ? "" : "s"} to a file. This is the only
        way annotations leave CEESVEE — they are never written into an ordinary data export.
      </p>
      <div className="flex gap-2">
        <button
          onClick={() => run("json")}
          disabled={total === 0}
          className="flex-1 rounded border border-zinc-200 px-3 py-2 text-sm hover:border-violet-400 disabled:opacity-50 dark:border-zinc-700"
        >
          <span className="font-medium">JSON</span>
          <span className="block text-xs text-zinc-400">Versioned, round-trippable</span>
        </button>
        <button
          onClick={() => run("csv")}
          disabled={total === 0}
          className="flex-1 rounded border border-zinc-200 px-3 py-2 text-sm hover:border-violet-400 disabled:opacity-50 dark:border-zinc-700"
        >
          <span className="font-medium">CSV</span>
          <span className="block text-xs text-zinc-400">Flat, one row per note</span>
        </button>
      </div>
    </Modal>
  );
}
