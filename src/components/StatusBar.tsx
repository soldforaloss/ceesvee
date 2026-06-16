import { formatNumber } from "../lib/format";
import { delimiterLabel, encodingLabel } from "../lib/labels";
import { useActiveMeta, useStore } from "../store/useStore";

export function StatusBar() {
  const meta = useActiveMeta();
  const selection = useStore((s) => s.selection);
  const busy = useStore((s) => s.busy);
  const clearFilter = useStore((s) => s.clearFilter);

  return (
    <div className="flex h-7 shrink-0 items-center gap-3 border-t border-zinc-200 bg-zinc-50 px-3 text-xs text-zinc-500 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-400">
      {meta ? (
        <>
          <span className="tabular-nums">
            {meta.filtered ? (
              <>
                <span className="text-violet-600 dark:text-violet-400">
                  {meta.rowCount.toLocaleString()}
                </span>{" "}
                of {meta.totalRowCount.toLocaleString()} rows
              </>
            ) : (
              <>{meta.rowCount.toLocaleString()} rows</>
            )}{" "}
            × {meta.colCount} cols
          </span>
          {meta.filtered && (
            <button
              onClick={() => void clearFilter()}
              title="Clear filter"
              className="rounded px-1.5 text-violet-600 hover:bg-violet-100 dark:text-violet-400 dark:hover:bg-violet-500/15"
            >
              filtered ✕
            </button>
          )}
          <Sep />
          <span>{encodingLabel(meta.encoding)}</span>
          {meta.hadBom && <span className="text-zinc-400 dark:text-zinc-500">BOM</span>}
          <Sep />
          <span>{delimiterLabel(meta.delimiter)}</span>
          <Sep />
          <span className="uppercase">{meta.lineEnding}</span>
          {meta.dirty && (
            <>
              <Sep />
              <span className="text-violet-600 dark:text-violet-400">● unsaved</span>
            </>
          )}
        </>
      ) : (
        <span>No file open</span>
      )}

      <div className="flex-1" />

      {selection && selection.count > 1 && (
        <span className="tabular-nums">
          {selection.count.toLocaleString()} selected
          {selection.numericCount > 0 && (
            <>
              {"  ·  "}sum {formatNumber(selection.sum)}
              {selection.avg !== null && (
                <>
                  {"  ·  "}avg {formatNumber(selection.avg)}
                </>
              )}
              {selection.min !== null && (
                <>
                  {"  ·  "}min {formatNumber(selection.min)}
                </>
              )}
              {selection.max !== null && (
                <>
                  {"  ·  "}max {formatNumber(selection.max)}
                </>
              )}
            </>
          )}
        </span>
      )}

      {busy && <span className="text-violet-600 dark:text-violet-400">working…</span>}
    </div>
  );
}

function Sep() {
  return <span className="text-zinc-300 dark:text-zinc-700">|</span>;
}
