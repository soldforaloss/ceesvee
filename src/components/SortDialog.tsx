import { useState } from "react";

import { useActiveMeta, useStore } from "../store/useStore";
import type { SortKey } from "../types";
import { Close } from "./Icons";
import { Modal } from "./Modal";

export function SortDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const sortBy = useStore((s) => s.sortBy);
  const [keys, setKeys] = useState<SortKey[]>([{ column: 0, descending: false }]);

  if (!meta) return null;

  const update = (i: number, patch: Partial<SortKey>) =>
    setKeys((ks) => ks.map((k, idx) => (idx === i ? { ...k, ...patch } : k)));

  const remove = (i: number) => setKeys((ks) => ks.filter((_, idx) => idx !== i));

  const addKey = () =>
    setKeys((ks) => [...ks, { column: nextUnusedColumn(ks, meta.colCount), descending: false }]);

  const apply = () => {
    void sortBy(keys);
    onClose();
  };

  return (
    <Modal
      title="Sort"
      onClose={onClose}
      footer={
        <>
          <button
            onClick={onClose}
            className="rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          <button
            onClick={apply}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500"
          >
            Apply
          </button>
        </>
      }
    >
      <div className="space-y-2">
        {keys.map((key, i) => (
          <div key={i} className="flex items-center gap-2">
            <span className="w-12 text-xs text-zinc-400">{i === 0 ? "Sort by" : "then by"}</span>
            <select
              value={key.column}
              onChange={(e) => update(i, { column: Number(e.target.value) })}
              className="flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700"
            >
              {meta.headers.map((h, c) => (
                <option key={c} value={c} className="dark:bg-zinc-800">
                  {h || `Column ${c + 1}`}
                </option>
              ))}
            </select>
            <div className="flex overflow-hidden rounded border border-zinc-300 text-xs dark:border-zinc-700">
              <button
                onClick={() => update(i, { descending: false })}
                className={`px-2 py-1 ${!key.descending ? "bg-violet-600 text-white" : "text-zinc-500"}`}
              >
                Asc
              </button>
              <button
                onClick={() => update(i, { descending: true })}
                className={`px-2 py-1 ${key.descending ? "bg-violet-600 text-white" : "text-zinc-500"}`}
              >
                Desc
              </button>
            </div>
            <button
              onClick={() => remove(i)}
              disabled={keys.length === 1}
              className="rounded p-1 text-zinc-400 hover:bg-zinc-100 disabled:opacity-30 dark:hover:bg-zinc-800"
              title="Remove"
            >
              <Close className="h-4 w-4" />
            </button>
          </div>
        ))}
        <button
          onClick={addKey}
          disabled={keys.length >= meta.colCount}
          className="text-sm text-violet-600 hover:underline disabled:opacity-40 dark:text-violet-400"
        >
          + Add another column
        </button>
      </div>
    </Modal>
  );
}

function nextUnusedColumn(keys: SortKey[], colCount: number): number {
  const used = new Set(keys.map((k) => k.column));
  for (let c = 0; c < colCount; c++) if (!used.has(c)) return c;
  return 0;
}
