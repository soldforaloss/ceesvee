import { DELIMITER_OPTIONS, ENCODING_OPTIONS } from "../lib/labels";
import { describeDiff } from "../lib/reopen";
import { useActiveMeta, useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * The "Reopen with settings" workflow (F02): every delimiter/encoding/header
 * change re-reads the file, so it is previewed here and — when the document
 * has unsaved edits — explicitly saved or discarded, never silently lost.
 */
export function ReopenDialog() {
  const meta = useActiveMeta();
  const reopen = useStore((s) => s.reopen);
  const close = useStore((s) => s.closeReopenDialog);
  const setOptions = useStore((s) => s.setReopenOptions);
  const confirm = useStore((s) => s.confirmReopen);

  if (!meta || !reopen.open) return null;
  const { preview, options, loading, error } = reopen;

  const delimiter = options.delimiter ?? preview?.delimiter ?? meta.delimiter;
  const encoding = options.encoding ?? preview?.encoding ?? meta.encoding;
  const hasHeader = options.hasHeaderRow ?? preview?.hasHeaderRow ?? meta.hasHeaderRow;
  const canConfirm = !loading && preview !== null;

  return (
    <Modal
      title="Reopen with settings"
      onClose={close}
      size="xl"
      footer={
        <>
          <button onClick={close} className={btnGhost}>
            Cancel
          </button>
          {meta.dirty ? (
            <>
              <button
                onClick={() => void confirm(true)}
                disabled={!canConfirm}
                className={btnDanger}
                title="Throw away the unsaved edits and re-read the file"
              >
                Discard changes and reopen
              </button>
              <button
                onClick={() => void confirm(false)}
                disabled={!canConfirm}
                className={btnPrimary}
                title="Save the current document first, then re-read the file"
              >
                Save and reopen
              </button>
            </>
          ) : (
            <button
              onClick={() => void confirm(false)}
              disabled={!canConfirm}
              className={btnPrimary}
            >
              Reopen
            </button>
          )}
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {meta.dirty && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            This document has unsaved edits. Reopening re-reads the file from disk, so the edits
            must be saved or explicitly discarded.
          </p>
        )}

        <div className="flex flex-wrap items-center gap-4">
          <label className="flex items-center gap-1.5">
            <span className="text-zinc-500 dark:text-zinc-400">Delimiter</span>
            <select
              value={DELIMITER_OPTIONS.some((o) => o.value === delimiter) ? delimiter : "custom"}
              onChange={(e) => setOptions({ delimiter: e.target.value })}
              className={selectCls}
            >
              {DELIMITER_OPTIONS.map((o) => (
                <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                  {o.label}
                </option>
              ))}
              {!DELIMITER_OPTIONS.some((o) => o.value === delimiter) && (
                <option value="custom" className="dark:bg-zinc-800">
                  Custom ({delimiter})
                </option>
              )}
            </select>
          </label>

          <label className="flex items-center gap-1.5">
            <span className="text-zinc-500 dark:text-zinc-400">Encoding</span>
            <select
              value={encoding}
              onChange={(e) => setOptions({ encoding: e.target.value })}
              className={selectCls}
            >
              {(ENCODING_OPTIONS.some((o) => o.value === encoding)
                ? ENCODING_OPTIONS
                : [...ENCODING_OPTIONS, { value: encoding, label: encoding }]
              ).map((o) => (
                <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                  {o.label}
                </option>
              ))}
            </select>
          </label>

          <label className="flex cursor-pointer items-center gap-1.5 select-none">
            <input
              type="checkbox"
              checked={hasHeader}
              onChange={(e) => setOptions({ hasHeaderRow: e.target.checked })}
              className="accent-violet-600"
            />
            First row is header
          </label>
        </div>

        {error && (
          <p className="rounded border border-red-200 bg-red-50 px-2 py-1.5 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/50 dark:text-red-300">
            {error}
          </p>
        )}

        {loading && <p className="py-4 text-center text-xs text-zinc-400">Reading file…</p>}

        {preview && !loading && (
          <>
            <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-zinc-500 dark:text-zinc-400">
              <span className="tabular-nums">
                {preview.rowCount.toLocaleString()} rows × {preview.colCount} cols
              </span>
              <span className="uppercase">{preview.lineEnding}</span>
              <span>{preview.hadBom ? "BOM" : "no BOM"}</span>
            </div>

            {(preview.hadDecodeErrors || preview.raggedTotal > 0) && (
              <div className="space-y-1 rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
                {preview.hadDecodeErrors && (
                  <p>
                    Some bytes are not valid {preview.encoding} and would be replaced with “�” — try
                    another encoding.
                  </p>
                )}
                {preview.raggedTotal > 0 && (
                  <p>
                    {preview.raggedTotal.toLocaleString()} record
                    {preview.raggedTotal === 1 ? " has" : "s have"} a field count different from the
                    expected {preview.modalFieldCount}
                    {preview.raggedSamples.length > 0 &&
                      ` (e.g. line ${preview.raggedSamples[0].line} has ${preview.raggedSamples[0].fields})`}
                    .
                  </p>
                )}
              </div>
            )}

            {preview.differences.length > 0 && (
              <div className="rounded border border-violet-200 bg-violet-50/60 px-2 py-1.5 text-xs text-violet-800 dark:border-violet-500/30 dark:bg-violet-500/10 dark:text-violet-300">
                <div className="mb-0.5 font-medium">Changes from the current view</div>
                <ul className="space-y-0.5">
                  {preview.differences.map((d) => (
                    <li key={d.field}>{describeDiff(d)}</li>
                  ))}
                </ul>
              </div>
            )}

            <div className="max-h-64 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
              <table className="w-full border-collapse text-xs">
                <tbody>
                  {preview.records.map((record, r) => {
                    const isHeader = preview.hasHeaderRow && r === 0;
                    return (
                      <tr
                        key={r}
                        className={
                          isHeader
                            ? "bg-zinc-100 font-semibold dark:bg-zinc-800"
                            : "border-t border-zinc-100 dark:border-zinc-800/60"
                        }
                      >
                        <td className="select-none px-1.5 py-0.5 text-right tabular-nums text-zinc-300 dark:text-zinc-600">
                          {isHeader ? "" : preview.hasHeaderRow ? r : r + 1}
                        </td>
                        {record.map((cell, c) => (
                          <td
                            key={c}
                            className="max-w-[10rem] truncate whitespace-nowrap px-1.5 py-0.5"
                            title={cell}
                          >
                            {cell}
                          </td>
                        ))}
                      </tr>
                    );
                  })}
                </tbody>
              </table>
              {preview.rowCount + (preview.hasHeaderRow ? 1 : 0) > preview.records.length && (
                <p className="border-t border-zinc-100 px-2 py-1 text-center text-[11px] text-zinc-400 dark:border-zinc-800">
                  Showing the first {preview.records.length} records
                </p>
              )}
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}

const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary =
  "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40";
const btnDanger =
  "rounded border border-red-300 px-3 py-1.5 text-sm text-red-700 hover:bg-red-50 disabled:opacity-40 dark:border-red-500/40 dark:text-red-300 dark:hover:bg-red-500/10";
