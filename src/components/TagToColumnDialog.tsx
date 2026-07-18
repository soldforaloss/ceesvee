import { useEffect, useState } from "react";

import { useActiveMeta, useStore } from "../store/useStore";
import type { TagToColumnPreview, TagToColumnTarget } from "../types";
import { Modal } from "./Modal";

/**
 * Copy a tag into a real column (F40). Previews how many matched rows carry the
 * tag and what is skipped as ambiguous / orphaned, then applies as ONE undoable
 * document operation — a fresh column or writes into an existing one. The notes
 * themselves are untouched; this materialises a copy on request.
 */
export function TagToColumnDialog() {
  const meta = useActiveMeta();
  const tag = useStore((s) => s.tagToColumnTag);
  const close = useStore((s) => s.closeTagToColumn);
  const preview = useStore((s) => s.previewTagToColumn);
  const apply = useStore((s) => s.applyTagToColumn);

  const [info, setInfo] = useState<TagToColumnPreview | null>(null);
  const [mode, setMode] = useState<"new" | "existing">("new");
  const [name, setName] = useState(tag ?? "");
  const [column, setColumn] = useState(0);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!tag) return;
    setName(tag);
    let cancelled = false;
    void preview(tag).then((p) => {
      if (!cancelled) setInfo(p);
    });
    return () => {
      cancelled = true;
    };
  }, [tag, preview]);

  if (!tag || !meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";

  const doApply = async () => {
    const target: TagToColumnTarget =
      mode === "new"
        ? { type: "newColumn", name: name.trim() || tag }
        : { type: "existingColumn", column };
    setBusy(true);
    const ok = await apply(tag, target);
    setBusy(false);
    if (ok) close();
  };

  return (
    <Modal
      title={`Copy tag “${tag}” to a column`}
      onClose={close}
      size="lg"
      footer={
        <>
          <button
            onClick={close}
            className="rounded border border-zinc-200 px-3 py-1.5 text-sm hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          <button
            onClick={() => void doApply()}
            disabled={busy || readOnly || !info || info.rowsAffected === 0}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-50"
          >
            Apply (one undo step)
          </button>
        </>
      }
    >
      {readOnly && (
        <p className="mb-2 rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
          This document is read-only (indexed). Convert it to editable to write a column.
        </p>
      )}

      <div className="space-y-3 text-sm">
        <div className="rounded border border-zinc-200 p-2 text-xs dark:border-zinc-800">
          {info ? (
            <>
              <div>
                <span className="font-medium">{info.rowsAffected}</span> matched row
                {info.rowsAffected === 1 ? "" : "s"} carry this tag.
              </div>
              {(info.ambiguousSkipped > 0 || info.orphanedSkipped > 0) && (
                <div className="mt-0.5 text-amber-600 dark:text-amber-400">
                  Skipping {info.ambiguousSkipped} ambiguous and {info.orphanedSkipped} orphaned
                  annotation{info.ambiguousSkipped + info.orphanedSkipped === 1 ? "" : "s"} (never
                  written to an uncertain row).
                </div>
              )}
              {info.sample.length > 0 && (
                <div className="mt-1 font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                  e.g. row {info.sample[0].record + 1} → “{info.sample[0].value}”
                </div>
              )}
            </>
          ) : (
            <span className="text-zinc-400">Computing preview…</span>
          )}
        </div>

        <label className="flex items-center gap-2">
          <input
            type="radio"
            checked={mode === "new"}
            onChange={() => setMode("new")}
            disabled={readOnly}
          />
          <span>New column</span>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            disabled={mode !== "new" || readOnly}
            className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-400 disabled:opacity-50 dark:border-zinc-700"
          />
        </label>

        <label className="flex items-center gap-2">
          <input
            type="radio"
            checked={mode === "existing"}
            onChange={() => setMode("existing")}
            disabled={readOnly}
          />
          <span>Existing column</span>
          <select
            value={column}
            onChange={(e) => setColumn(Number(e.target.value))}
            disabled={mode !== "existing" || readOnly}
            className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-400 disabled:opacity-50 dark:border-zinc-700"
          >
            {meta.headers.map((h, i) => (
              <option key={i} value={i} className="dark:bg-zinc-800">
                {h || `Column ${i + 1}`}
              </option>
            ))}
          </select>
        </label>
        <p className="text-xs text-zinc-400">
          Tagged rows get the tag name; other rows stay blank (new column) or unchanged (existing).
        </p>
      </div>
    </Modal>
  );
}
