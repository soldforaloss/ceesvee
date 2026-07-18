import { useEffect, useState } from "react";

import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Row / cell note editor (F40). Driven by the store's `annotationNoteTarget`:
 * a row note when `columnId` is null, else a per-column cell note. Saving an
 * empty note clears it (the backend prunes an emptied annotation). Notes are
 * pure metadata — they never enter the document or its exports.
 */
export function NoteEditorDialog() {
  const target = useStore((s) => s.annotationNoteTarget);
  const close = useStore((s) => s.closeNoteEditor);
  const setRowNote = useStore((s) => s.setRowNote);
  const setCellNote = useStore((s) => s.setCellNote);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  // Sync the editor to each freshly-opened target (the dialog stays mounted).
  useEffect(() => {
    if (target) setText(target.initialText);
  }, [target?.displayRow, target?.columnId, target?.initialText, target]);

  if (!target) return null;
  const isCell = target.columnId != null;
  const hadNote = (target.initialText ?? "").length > 0;

  const save = async (value: string | null) => {
    setBusy(true);
    const ok = isCell
      ? await setCellNote(target.displayRow, target.columnId!, value)
      : await setRowNote(target.displayRow, value);
    setBusy(false);
    if (ok) close();
  };

  return (
    <Modal
      title={`${isCell ? "Cell note" : "Row note"} — ${target.label}`}
      onClose={close}
      footer={
        <>
          {hadNote && (
            <button
              onClick={() => void save(null)}
              disabled={busy}
              className="mr-auto rounded border border-red-200 px-3 py-1.5 text-sm text-red-600 hover:bg-red-50 disabled:opacity-50 dark:border-red-900/60 dark:hover:bg-red-950/50"
            >
              Delete note
            </button>
          )}
          <button
            onClick={close}
            className="rounded border border-zinc-200 px-3 py-1.5 text-sm hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          <button
            onClick={() => void save(text.trim() ? text : null)}
            disabled={busy}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-50"
          >
            Save
          </button>
        </>
      }
    >
      <textarea
        autoFocus
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            void save(text.trim() ? text : null);
          }
        }}
        rows={5}
        placeholder="Write a note…"
        className="w-full resize-y rounded border border-zinc-300 bg-transparent px-2 py-1.5 text-sm outline-none focus:border-violet-400 dark:border-zinc-700"
      />
      <p className="mt-1 text-xs text-zinc-400">
        Metadata only — this never appears in an ordinary data export. Cmd/Ctrl+Enter to save.
      </p>
    </Modal>
  );
}
