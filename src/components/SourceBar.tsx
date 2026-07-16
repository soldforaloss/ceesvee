import { useState } from "react";

import { DELIMITER_OPTIONS, ENCODING_OPTIONS } from "../lib/labels";
import { useActiveMeta, useStore } from "../store/useStore";

const CUSTOM = "__custom__";

export function SourceBar() {
  const meta = useActiveMeta();
  const openReopenDialog = useStore((s) => s.openReopenDialog);
  const setHeaderMode = useStore((s) => s.setHeaderMode);
  const convertToEditable = useStore((s) => s.convertActiveToEditable);
  const indexing = useStore((s) => s.indexing);

  const standard = DELIMITER_OPTIONS.some((o) => o.value === meta?.delimiter);
  const [customMode, setCustomMode] = useState(false);
  const [customValue, setCustomValue] = useState("");
  // After a refused convert (memory estimate), offer "convert anyway".
  const [offerForce, setOfferForce] = useState(false);

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";
  // Reparse materialises the whole file, so indexed documents change their
  // source settings by re-opening instead.
  const canReparse = meta.path !== null && !readOnly;
  const converting = indexing?.kind === "convertEditable" && indexing.docId === meta.id;

  const onConvert = async (force: boolean) => {
    await convertToEditable(force);
    const failed = useStore.getState().error;
    setOfferForce(!force && !!failed && failed.includes("convert anyway"));
  };

  // Source-setting changes re-read the file, so they never apply directly:
  // they open the "Reopen with settings" preview (F02), which handles dirty
  // documents explicitly.
  const onDelimiterSelect = (value: string) => {
    if (value === CUSTOM) {
      setCustomMode(true);
      setCustomValue(meta.delimiter);
      return;
    }
    setCustomMode(false);
    openReopenDialog({ delimiter: value });
  };

  const applyCustom = () => {
    if (customValue) openReopenDialog({ delimiter: customValue });
    setCustomMode(false);
  };

  const onHeaderToggle = (checked: boolean) => {
    if (canReparse) {
      openReopenDialog({ hasHeaderRow: checked });
    } else {
      // Unsaved documents have no file to re-read; toggle in memory.
      void setHeaderMode(checked);
    }
  };

  return (
    <div className="flex h-9 shrink-0 items-center gap-4 border-b border-zinc-200 bg-white px-3 text-xs text-zinc-600 dark:border-zinc-800 dark:bg-zinc-950 dark:text-zinc-300">
      <Field label="Delimiter">
        {customMode ? (
          <span className="flex items-center gap-1">
            <input
              autoFocus
              maxLength={1}
              value={customValue}
              onChange={(e) => setCustomValue(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && applyCustom()}
              className="w-10 rounded border border-zinc-300 bg-transparent px-1 py-0.5 text-center outline-none focus:border-violet-500 dark:border-zinc-600"
            />
            <button
              onClick={applyCustom}
              className="rounded bg-violet-600 px-1.5 py-0.5 text-white hover:bg-violet-500"
            >
              Preview…
            </button>
          </span>
        ) : (
          <Select
            value={standard ? meta.delimiter : CUSTOM}
            disabled={!canReparse}
            onChange={onDelimiterSelect}
            options={[
              ...DELIMITER_OPTIONS,
              { value: CUSTOM, label: standard ? "Custom…" : `Custom (${meta.delimiter})` },
            ]}
          />
        )}
      </Field>

      <Field label="Encoding">
        <Select
          value={meta.encoding}
          disabled={!canReparse}
          onChange={(value) => openReopenDialog({ encoding: value })}
          options={
            ENCODING_OPTIONS.some((o) => o.value === meta.encoding)
              ? ENCODING_OPTIONS
              : [...ENCODING_OPTIONS, { value: meta.encoding, label: meta.encoding }]
          }
        />
      </Field>

      <label
        className={`flex items-center gap-1.5 select-none ${readOnly ? "opacity-40" : "cursor-pointer"}`}
      >
        <input
          type="checkbox"
          checked={meta.hasHeaderRow}
          disabled={readOnly}
          onChange={(e) => onHeaderToggle(e.target.checked)}
          className="accent-violet-600"
        />
        First row is header
      </label>

      {readOnly && (
        <span className="flex items-center gap-2">
          <span
            title="This document is served from a streaming record index instead of memory, so it can be huge — browsing, find, filter, export, diagnostics and profiling all work, but cells cannot be edited."
            className="rounded-full bg-amber-100 px-2 py-0.5 font-medium text-amber-800 dark:bg-amber-500/15 dark:text-amber-300"
          >
            Read-only (indexed)
          </span>
          {converting ? (
            <span className="text-zinc-400">
              Converting…{" "}
              {indexing.total ? `${Math.round((indexing.processed / indexing.total) * 100)}%` : ""}
            </span>
          ) : (
            <button
              onClick={() => void onConvert(offerForce)}
              className="rounded border border-zinc-300 px-1.5 py-0.5 hover:border-violet-500 hover:text-violet-600 dark:border-zinc-600 dark:hover:text-violet-300"
              title={
                offerForce
                  ? "The estimate says this may exhaust memory — convert anyway?"
                  : "Load the whole file into memory to enable editing"
              }
            >
              {offerForce ? "Convert anyway" : "Convert to editable"}
            </button>
          )}
        </span>
      )}

      {(meta.path ?? meta.archive) && (
        <span
          className="ml-auto truncate text-zinc-400 dark:text-zinc-600"
          dir="rtl"
          title={
            meta.path ??
            `${meta.archive!.archivePath}${meta.archive!.entryName ? ` → ${meta.archive!.entryName}` : ""}`
          }
        >
          {meta.path ??
            `${meta.archive!.archivePath}${meta.archive!.entryName ? ` → ${meta.archive!.entryName}` : ""}`}
        </span>
      )}
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className="text-zinc-400 dark:text-zinc-500">{label}</span>
      {children}
    </span>
  );
}

function Select({
  value,
  options,
  onChange,
  disabled,
}: {
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
  disabled?: boolean;
}) {
  return (
    <select
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value)}
      className="rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 disabled:opacity-40 dark:border-zinc-700"
    >
      {options.map((o) => (
        <option key={o.value} value={o.value} className="dark:bg-zinc-800">
          {o.label}
        </option>
      ))}
    </select>
  );
}
