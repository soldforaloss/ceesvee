import { useEffect } from "react";

import { formatNumber } from "../lib/format";
import { useActiveMeta, useStore } from "../store/useStore";
import type { ColumnKind } from "../types";
import { Modal } from "./Modal";

const KIND_STYLES: Record<ColumnKind, string> = {
  number: "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300",
  date: "bg-sky-100 text-sky-700 dark:bg-sky-500/15 dark:text-sky-300",
  bool: "bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300",
  text: "bg-zinc-100 text-zinc-600 dark:bg-zinc-700/40 dark:text-zinc-300",
};

export function SummaryPanel({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const summaries = useStore((s) => s.summaries);
  const summariesDocId = useStore((s) => s.summariesDocId);
  const loadSummaries = useStore((s) => s.loadSummaries);

  // Ensure summaries are fresh for this document when the panel opens.
  useEffect(() => {
    loadSummaries();
  }, [loadSummaries]);

  if (!meta) return null;
  const rows = summariesDocId === meta.id ? summaries : null;

  return (
    <Modal title="Column summaries" onClose={onClose} size="xl">
      {meta.filtered && (
        <p className="mb-3 rounded bg-violet-50 px-2 py-1.5 text-xs text-violet-700 dark:bg-violet-500/10 dark:text-violet-300">
          A filter is active. Summaries are computed over all {meta.totalRowCount.toLocaleString()}{" "}
          rows, not just the visible ones.
        </p>
      )}
      {!rows ? (
        <p className="py-8 text-center text-sm text-zinc-400">Analysing columns…</p>
      ) : (
        <div className="max-h-[60vh] overflow-auto">
          <table className="w-full border-collapse text-sm">
            <thead className="sticky top-0 bg-white dark:bg-zinc-900">
              <tr className="text-left text-xs uppercase tracking-wide text-zinc-400">
                <th className="py-1.5 pr-3 font-medium">Column</th>
                <th className="py-1.5 pr-3 font-medium">Type</th>
                <th className="py-1.5 pr-3 text-right font-medium">Filled</th>
                <th className="py-1.5 pr-3 text-right font-medium">Empty</th>
                <th className="py-1.5 pr-3 text-right font-medium">Unique</th>
                <th className="py-1.5 pr-3 text-right font-medium">Min</th>
                <th className="py-1.5 pr-3 text-right font-medium">Max</th>
                <th className="py-1.5 text-right font-medium">Mean</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((cs) => {
                const name = meta.headers[cs.column] || `Column ${cs.column + 1}`;
                const filled = cs.count - cs.nulls;
                return (
                  <tr key={cs.column} className="border-t border-zinc-100 dark:border-zinc-800">
                    <td
                      className="max-w-[14rem] truncate py-1.5 pr-3 font-medium text-zinc-700 dark:text-zinc-200"
                      title={name}
                    >
                      {name}
                    </td>
                    <td className="py-1.5 pr-3">
                      <span
                        className={`rounded px-1.5 py-0.5 text-[11px] font-medium ${KIND_STYLES[cs.kind]}`}
                      >
                        {cs.kind}
                      </span>
                    </td>
                    <td className="py-1.5 pr-3 text-right tabular-nums text-zinc-600 dark:text-zinc-300">
                      {formatNumber(filled)}
                    </td>
                    <td className="py-1.5 pr-3 text-right tabular-nums text-zinc-500">
                      {formatNumber(cs.nulls)}
                    </td>
                    <td className="py-1.5 pr-3 text-right tabular-nums text-zinc-600 dark:text-zinc-300">
                      {formatNumber(cs.unique)}
                    </td>
                    <td className="py-1.5 pr-3 text-right tabular-nums text-zinc-600 dark:text-zinc-300">
                      {cs.numeric ? formatNumber(cs.numeric.min) : "—"}
                    </td>
                    <td className="py-1.5 pr-3 text-right tabular-nums text-zinc-600 dark:text-zinc-300">
                      {cs.numeric ? formatNumber(cs.numeric.max) : "—"}
                    </td>
                    <td className="py-1.5 text-right tabular-nums text-zinc-600 dark:text-zinc-300">
                      {cs.numeric ? formatNumber(cs.numeric.mean) : "—"}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Modal>
  );
}
