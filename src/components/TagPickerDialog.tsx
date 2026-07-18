import { useState } from "react";

import { normalizeTagName, tagColor } from "../lib/annotations";
import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Tag picker (F40). Applies or removes named tags across the selected DISPLAY
 * rows, with an inline "create a new tag" field. Applying a tag auto-registers
 * it in the per-document namespace (the backend ensures it). Tagging never
 * touches source data.
 */
export function TagPickerDialog() {
  const target = useStore((s) => s.annotationTagTarget);
  const close = useStore((s) => s.closeTagPicker);
  const tags = useStore((s) => s.annotationsView?.tags ?? []);
  const applyRowMarks = useStore((s) => s.applyRowMarks);
  const [newTag, setNewTag] = useState("");
  const [busy, setBusy] = useState(false);

  if (!target) return null;
  const rows = target.displayRows;
  const count = rows.length;

  const run = async (patch: { addTags?: string[]; removeTags?: string[] }) => {
    setBusy(true);
    await applyRowMarks(rows, patch);
    setBusy(false);
  };

  const createAndAdd = async () => {
    const name = normalizeTagName(newTag);
    if (!name) return;
    await run({ addTags: [name] });
    setNewTag("");
  };

  return (
    <Modal
      title={`Tag ${count} row${count === 1 ? "" : "s"}`}
      onClose={close}
      footer={
        <button
          onClick={close}
          className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500"
        >
          Done
        </button>
      }
    >
      <div className="space-y-2">
        {tags.length === 0 && (
          <p className="text-xs text-zinc-500">No tags yet — create one below.</p>
        )}
        {tags.map((t) => (
          <div key={t.name} className="flex items-center gap-2 text-sm">
            <span
              className="h-2.5 w-2.5 shrink-0 rounded-full"
              style={{ background: tagColor(t.name, t.color) }}
            />
            <span className="truncate">{t.name}</span>
            <span className="text-xs text-zinc-400">{t.count}</span>
            <span className="flex-1" />
            <button
              onClick={() => void run({ addTags: [t.name] })}
              disabled={busy}
              className="rounded border border-zinc-200 px-2 py-0.5 text-xs hover:border-violet-400 disabled:opacity-50 dark:border-zinc-700"
            >
              Add
            </button>
            <button
              onClick={() => void run({ removeTags: [t.name] })}
              disabled={busy}
              className="rounded border border-zinc-200 px-2 py-0.5 text-xs hover:border-red-400 disabled:opacity-50 dark:border-zinc-700"
            >
              Remove
            </button>
          </div>
        ))}

        <form
          onSubmit={(e) => {
            e.preventDefault();
            void createAndAdd();
          }}
          className="flex items-center gap-1.5 border-t border-zinc-200 pt-2 dark:border-zinc-800"
        >
          <input
            autoFocus
            value={newTag}
            onChange={(e) => setNewTag(e.target.value)}
            placeholder="New tag name…"
            className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-400 dark:border-zinc-700"
          />
          <button
            type="submit"
            disabled={busy || !normalizeTagName(newTag)}
            className="rounded bg-violet-600 px-3 py-1 text-sm text-white hover:bg-violet-500 disabled:opacity-50"
          >
            Create &amp; add
          </button>
        </form>
      </div>
    </Modal>
  );
}
