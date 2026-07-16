import { readText } from "@tauri-apps/plugin-clipboard-manager";
import { useEffect, useRef, useState } from "react";

import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { PastePreview, PasteSpecialOptions } from "../types";
import { Modal } from "./Modal";

/**
 * Paste Special (F14): structured clipboard paste with an always-on preview.
 * Nothing mutates until Apply; the preview shows dimensions, growth, header
 * changes, warnings, and the first ten resulting rows. Applying is one undo
 * step, guarded by the document revision the preview was computed against.
 */
export function PasteSpecialDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const selectionRect = useStore((s) => s.selectionRect);
  const reloadActive = useStore((s) => s.refreshActiveDoc);

  const [text, setText] = useState<string | null>(null);
  const [mode, setMode] = useState<PasteSpecialOptions["mode"]>("overwrite");
  const [transpose, setTranspose] = useState(false);
  const [skipBlanks, setSkipBlanks] = useState(false);
  const [trim, setTrim] = useState(false);
  const [repeatToFill, setRepeatToFill] = useState(false);
  const [firstRowHeaders, setFirstRowHeaders] = useState(false);
  const [preview, setPreview] = useState<PastePreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  const previewRequest = useRef(0);
  // What the CURRENT preview was computed for: Apply stays disabled until a
  // fresh preview matches the current options AND the apply is guarded by
  // the revision the preview saw, so nothing ever mutates unpreviewed.
  const [previewedKey, setPreviewedKey] = useState<string | null>(null);
  const [previewRevision, setPreviewRevision] = useState<number | null>(null);

  const anchorRow = selectionRect?.y ?? 0;
  const anchorCol = selectionRect?.x ?? 0;
  const selectionRows = selectionRect?.height ?? 0;
  const selectionCols = selectionRect?.width ?? 0;

  const options: PasteSpecialOptions = {
    mode,
    transpose,
    skipBlanks,
    trim,
    repeatToFill,
    firstRowHeaders,
  };
  const optionsKey = JSON.stringify([
    mode,
    transpose,
    skipBlanks,
    trim,
    repeatToFill,
    firstRowHeaders,
    anchorRow,
    anchorCol,
    selectionRows,
    selectionCols,
  ]);

  // Read the clipboard once when the dialog opens.
  useEffect(() => {
    readText()
      .then((clip) => setText(clip ?? ""))
      .catch((e) => setError(String(e)));
  }, []);

  // Recompute the preview whenever the text or any option changes; stale
  // responses are dropped by request id.
  useEffect(() => {
    if (!meta || text === null) return;
    if (text === "") {
      setPreview(null);
      setError("The clipboard is empty.");
      return;
    }
    const request = ++previewRequest.current;
    const requestKey = optionsKey;
    const requestRevision = meta.revision;
    const handle = setTimeout(() => {
      api
        .previewPasteSpecial(
          meta.id,
          text,
          options,
          anchorRow,
          anchorCol,
          selectionRows,
          selectionCols,
        )
        .then((p) => {
          if (previewRequest.current === request) {
            setPreview(p);
            setPreviewedKey(requestKey);
            setPreviewRevision(requestRevision);
            setError(null);
          }
        })
        .catch((e) => {
          if (previewRequest.current === request) {
            setPreview(null);
            setPreviewedKey(null);
            setError(String(e));
          }
        });
    }, 150);
    return () => clearTimeout(handle);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    meta?.id,
    meta?.revision,
    text,
    mode,
    transpose,
    skipBlanks,
    trim,
    repeatToFill,
    firstRowHeaders,
  ]);

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";

  // Fresh preview matching the current options — the only state Apply accepts.
  const previewCurrent = preview !== null && previewedKey === optionsKey;

  const apply = async () => {
    if (text === null || readOnly || !previewCurrent || previewRevision === null) return;
    setWorking(true);
    try {
      // Guarded by the revision the PREVIEW was computed against, so an edit
      // made while the dialog is open is rejected instead of applied blind.
      await api.applyPasteSpecial(
        meta.id,
        text,
        options,
        anchorRow,
        anchorCol,
        selectionRows,
        selectionCols,
        previewRevision,
      );
      await reloadActive();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const check = (
    label: string,
    value: boolean,
    onChange: (next: boolean) => void,
    disabled = false,
  ) => (
    <label
      className={`flex items-center gap-1.5 text-xs ${disabled ? "opacity-40" : "cursor-pointer"}`}
    >
      <input
        type="checkbox"
        checked={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="accent-violet-600"
      />
      {label}
    </label>
  );

  return (
    <Modal
      title="Paste Special"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => void apply()}
            disabled={working || !previewCurrent || readOnly}
            title={readOnly ? "Read-only (indexed) document" : undefined}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {working ? "Applying…" : previewCurrent || !preview ? "Apply" : "Previewing…"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
          <label className="flex items-center gap-1.5 text-xs">
            <input
              type="radio"
              checked={mode === "overwrite"}
              onChange={() => setMode("overwrite")}
              className="accent-violet-600"
            />
            Overwrite from anchor
          </label>
          <label className="flex items-center gap-1.5 text-xs">
            <input
              type="radio"
              checked={mode === "insertRows"}
              onChange={() => setMode("insertRows")}
              className="accent-violet-600"
            />
            Insert as new rows
          </label>
          <span className="text-xs text-zinc-400">
            Anchor: row {anchorRow + 1}, column {anchorCol + 1}
          </span>
        </div>
        <div className="flex flex-wrap gap-x-4 gap-y-1.5">
          {check("Transpose", transpose, setTranspose)}
          {check("Skip blank source cells", skipBlanks, setSkipBlanks, mode !== "overwrite")}
          {check("Trim incoming cells", trim, setTrim)}
          {check(
            "Repeat pattern over selection",
            repeatToFill,
            setRepeatToFill,
            selectionRows <= 1 && selectionCols <= 1,
          )}
          {check("First row is headers", firstRowHeaders, setFirstRowHeaders)}
        </div>

        {preview && (
          <div className="space-y-2 rounded border border-zinc-200 p-2 dark:border-zinc-800">
            <p className="text-xs text-zinc-500 dark:text-zinc-400">
              {preview.rows.toLocaleString()} × {preview.cols} into row {preview.targetRow + 1},
              column {preview.targetCol + 1}
              {preview.addedRows > 0 && ` · adds ${preview.addedRows.toLocaleString()} rows`}
              {preview.addedCols > 0 && ` · adds ${preview.addedCols} columns`}
            </p>
            {preview.headerChanges.length > 0 && (
              <p className="text-xs text-violet-600 dark:text-violet-300">
                Headers: {preview.headerChanges.join(", ")}
              </p>
            )}
            {preview.warnings.map((w) => (
              <p key={w} className="text-xs text-amber-600 dark:text-amber-400">
                {w}
              </p>
            ))}
            <div className="max-h-48 overflow-auto">
              <table className="w-full border-collapse text-xs">
                <tbody>
                  {preview.sample.map((row, r) => (
                    <tr key={r} className="border-t border-zinc-100 dark:border-zinc-800">
                      {row.map((cell, c) => (
                        <td key={c} className="max-w-40 truncate px-2 py-0.5 font-mono">
                          {cell}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        )}
        {text !== null && !preview && !error && (
          <p className="py-3 text-center text-xs text-zinc-400">Building preview…</p>
        )}
        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
