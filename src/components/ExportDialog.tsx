import { useState } from "react";

import { DELIMITER_OPTIONS, ENCODING_OPTIONS } from "../lib/labels";
import { useActiveMeta, useStore } from "../store/useStore";
import type { ExportOptions } from "../types";
import { Modal } from "./Modal";

export function ExportDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const saveActive = useStore((s) => s.saveActive);

  const [opts, setOpts] = useState<ExportOptions>(() => ({
    delimiter: meta?.delimiter ?? ",",
    encoding: ENCODING_OPTIONS.some((o) => o.value === meta?.encoding)
      ? (meta?.encoding ?? "UTF-8")
      : "UTF-8",
    quoteStyle: "minimal",
    lineEnding: meta?.lineEnding ?? "lf",
    bom: meta?.hadBom ?? false,
    includeHeaders: meta?.hasHeaderRow ?? true,
  }));

  if (!meta) return null;
  const patch = (p: Partial<ExportOptions>) => setOpts((o) => ({ ...o, ...p }));

  const save = () => {
    void saveActive(true, opts);
    onClose();
  };

  return (
    <Modal
      title="Export / Save As"
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
            onClick={save}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500"
          >
            Choose file & save
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {meta.filtered && (
          <p className="rounded bg-violet-50 px-2 py-1.5 text-xs text-violet-700 dark:bg-violet-500/10 dark:text-violet-300">
            A filter is active — all {meta.totalRowCount.toLocaleString()} rows will be written. The
            filter only changes the on-screen view, not what is saved.
          </p>
        )}
        <Row label="Delimiter">
          <select
            value={
              DELIMITER_OPTIONS.some((o) => o.value === opts.delimiter) ? opts.delimiter : "other"
            }
            onChange={(e) =>
              patch({ delimiter: e.target.value === "other" ? opts.delimiter : e.target.value })
            }
            className={selectCls}
          >
            {DELIMITER_OPTIONS.map((o) => (
              <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                {o.label}
              </option>
            ))}
            {!DELIMITER_OPTIONS.some((o) => o.value === opts.delimiter) && (
              <option value="other" className="dark:bg-zinc-800">
                Custom ({opts.delimiter})
              </option>
            )}
          </select>
        </Row>

        <Row label="Encoding">
          <select
            value={opts.encoding}
            onChange={(e) => patch({ encoding: e.target.value })}
            className={selectCls}
          >
            {ENCODING_OPTIONS.map((o) => (
              <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                {o.label}
              </option>
            ))}
          </select>
        </Row>

        <Row label="Quoting">
          <Segmented
            value={opts.quoteStyle}
            options={[
              { value: "minimal", label: "Minimal" },
              { value: "always", label: "Always quote" },
            ]}
            onChange={(v) => patch({ quoteStyle: v as ExportOptions["quoteStyle"] })}
          />
        </Row>

        <Row label="Line endings">
          <Segmented
            value={opts.lineEnding}
            options={[
              { value: "lf", label: "LF (\\n)" },
              { value: "crlf", label: "CRLF (\\r\\n)" },
            ]}
            onChange={(v) => patch({ lineEnding: v as ExportOptions["lineEnding"] })}
          />
        </Row>

        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={opts.bom}
            onChange={(e) => patch({ bom: e.target.checked })}
            className="accent-violet-600"
          />
          Write byte-order mark (BOM)
        </label>

        {meta.hasHeaderRow && (
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={opts.includeHeaders}
              onChange={(e) => patch({ includeHeaders: e.target.checked })}
              className="accent-violet-600"
            />
            Include header row
          </label>
        )}
      </div>
    </Modal>
  );
}

const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="text-zinc-500 dark:text-zinc-400">{label}</span>
      {children}
    </div>
  );
}

function Segmented({
  value,
  options,
  onChange,
}: {
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
}) {
  return (
    <div className="flex overflow-hidden rounded border border-zinc-300 text-xs dark:border-zinc-700">
      {options.map((o) => (
        <button
          key={o.value}
          onClick={() => onChange(o.value)}
          className={`px-2.5 py-1 ${value === o.value ? "bg-violet-600 text-white" : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"}`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
