import { useState } from "react";

import { ENCODING_OPTIONS } from "../lib/labels";
import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * Shown when a save targeting a legacy encoding would lose characters (F03).
 * Nothing is ever substituted silently: the user switches to UTF-8, picks a
 * different encoding, or cancels the save.
 */
export function EncodingIssuesDialog() {
  const prompt = useStore((s) => s.encodingIssues);
  const meta = useStore((s) => s.tabs.find((t) => t.id === s.encodingIssues?.docId) ?? null);
  const resolve = useStore((s) => s.resolveEncodingIssues);
  const jumpToCell = useStore((s) => s.jumpToCell);
  const [encoding, setEncoding] = useState("UTF-16LE");

  if (!prompt || !meta) return null;
  const { compat } = prompt;

  return (
    <Modal
      title={`Characters not supported by ${compat.encoding}`}
      onClose={() => void resolve(null)}
      size="lg"
      footer={
        <>
          <button onClick={() => void resolve(null)} className={btnGhost}>
            Cancel save
          </button>
          <span className="flex items-center gap-1.5">
            <select
              value={encoding}
              onChange={(e) => setEncoding(e.target.value)}
              className={selectCls}
            >
              {ENCODING_OPTIONS.filter((o) => o.value !== compat.encoding).map((o) => (
                <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                  {o.label}
                </option>
              ))}
            </select>
            <button onClick={() => void resolve(encoding)} className={btnGhost}>
              Use this encoding
            </button>
          </span>
          <button onClick={() => void resolve("UTF-8")} className={btnPrimary}>
            Switch to UTF-8
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <p>
          <span className="font-medium tabular-nums">{compat.affectedCells.toLocaleString()}</span>{" "}
          cell{compat.affectedCells === 1 ? "" : "s"} in{" "}
          <span className="font-medium">{meta.fileName}</span> contain characters that{" "}
          {compat.encoding} cannot represent. Saving would corrupt them, so the save was blocked.
        </p>

        <div className="max-h-52 overflow-y-auto rounded border border-zinc-200 dark:border-zinc-800">
          <ul className="divide-y divide-zinc-100 text-xs dark:divide-zinc-800/60">
            {compat.samples.map((s, i) => (
              <li key={i}>
                <button
                  className="flex w-full items-baseline gap-2 px-2 py-1 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
                  title="Jump to cell"
                  onClick={() => void jumpToCell(s.row ?? 0, s.col)}
                >
                  <span className="shrink-0 tabular-nums text-zinc-400">
                    {s.row === null ? "header" : `row ${s.row + 1}`} ·{" "}
                    {meta.headers[s.col]?.trim() || `col ${s.col + 1}`}
                  </span>
                  <span className="truncate font-mono">“{s.value}”</span>
                </button>
              </li>
            ))}
          </ul>
          {compat.affectedCells > compat.samples.length && (
            <p className="border-t border-zinc-100 px-2 py-1 text-center text-[11px] text-zinc-400 dark:border-zinc-800">
              Showing the first {compat.samples.length} of {compat.affectedCells.toLocaleString()}{" "}
              affected cells
            </p>
          )}
        </div>
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary = "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
