import { useEffect, useMemo, useState } from "react";

import {
  buildImportOptions,
  defaultImportUi,
  defaultSource,
  isValidA1Range,
  sheetKindLabel,
  visibilityLabel,
  type ExcelImportUi,
  type ExcelSource,
} from "../lib/excel";
import { useStore } from "../store/useStore";
import type { ExcelFormulaPolicy, ExcelMergedPolicy, ExcelSheetInfo } from "../types";
import { Modal } from "./Modal";

/**
 * Upper bound on preview columns rendered at once. The import itself is
 * unaffected; this only bounds the dialog's DOM, per the "bounded windows to
 * React only" invariant. A wide sheet shows the first slice with a "+N more".
 */
const MAX_PREVIEW_COLUMNS = 60;

/**
 * Excel `.xlsx` open chooser (F34). Self-driven by the `excelImport` store
 * slice, so opening a `.xlsx` file (or the "Open Excel…" command) shows it
 * automatically. It renders the workbook inspection (sheets with visibility /
 * dimensions / formula + merge counts, named tables, named ranges), lets the
 * user pick a source and import options, previews the projected columns and
 * sample rows through the job registry, and imports into a NEW document — the
 * original workbook is never modified.
 */
export function ExcelOpenDialog() {
  const st = useStore((s) => s.excelImport);
  const derive = useStore((s) => s.derive);
  const deriveError = useStore((s) => s.deriveError);
  const runExcelPreview = useStore((s) => s.runExcelPreview);
  const applyExcelImport = useStore((s) => s.applyExcelImport);
  const cancelExcelPreview = useStore((s) => s.cancelExcelPreview);
  const cancelDerive = useStore((s) => s.cancelDerive);
  const dismiss = useStore((s) => s.dismissExcelImport);

  const workbook = st?.workbook ?? null;

  const [source, setSource] = useState<ExcelSource | null>(null);
  const [ui, setUi] = useState<ExcelImportUi>(() => defaultImportUi());

  // When the inspection lands (and nothing is chosen yet), select a default
  // source so the first preview runs without a click.
  useEffect(() => {
    if (workbook && source === null) {
      setSource(defaultSource(workbook));
      setUi(defaultImportUi());
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workbook]);

  const rangeValid = source?.kind === "sheet" ? isValidA1Range(ui.range) : true;

  const options = useMemo(() => (source ? buildImportOptions(source, ui) : null), [source, ui]);
  const optionsKey = options ? JSON.stringify(options) : null;
  const scannedKey = st?.options ? JSON.stringify(st.options) : null;

  // Re-run the preview whenever the built options diverge from the ones the
  // current preview was scanned under (debounced, and never for an invalid range).
  useEffect(() => {
    if (optionsKey === null || optionsKey === scannedKey || !rangeValid) return;
    const timer = setTimeout(() => void runExcelPreview(JSON.parse(optionsKey)), 300);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [optionsKey, scannedKey, rangeValid]);

  if (!st) return null;

  const preview = st.preview;
  const importing = derive?.kind === "excelImport";
  const scanning = st.previewJobId != null;
  const inspecting = st.workbook === null && st.inspectJobId != null;

  const selectedSheet: ExcelSheetInfo | null =
    source?.kind === "sheet"
      ? (workbook?.sheets.find((s) => s.name === source.name) ?? null)
      : null;

  const patchUi = (p: Partial<ExcelImportUi>) => setUi((u) => ({ ...u, ...p }));

  const shownColumns = preview ? preview.columns.slice(0, MAX_PREVIEW_COLUMNS) : [];
  const hiddenColumns = (preview?.columnCount ?? 0) - shownColumns.length;

  const canImport =
    !!source &&
    rangeValid &&
    !importing &&
    !scanning &&
    preview != null &&
    preview.columnCount > 0 &&
    st.inspectError === null;

  const noCached = preview?.formulasWithoutCachedResults ?? 0;

  return (
    <Modal
      title={`Import Excel — ${st.fileName}`}
      onClose={dismiss}
      size="2xl"
      footer={
        <>
          <button onClick={dismiss} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => options && void applyExcelImport(options)}
            disabled={!canImport}
            title={
              !source
                ? "Choose a sheet, table or named range first"
                : !rangeValid
                  ? "Fix the cell range first"
                  : undefined
            }
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {importing ? "Importing…" : "Import into a new document"}
          </button>
        </>
      }
    >
      <div className="max-h-[72vh] space-y-3 overflow-y-auto text-sm">
        {inspecting && (
          <p className="text-xs text-zinc-500 dark:text-zinc-400">Inspecting the workbook…</p>
        )}
        {st.inspectError && (
          <p className="text-xs text-red-600 dark:text-red-400">{st.inspectError}</p>
        )}

        {workbook && (
          <>
            <div className="grid grid-cols-1 gap-3 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
              {/* ----- Source picker ----- */}
              <div className="space-y-2">
                <p className="text-xs font-medium text-zinc-600 dark:text-zinc-300">Source</p>
                <div className="max-h-72 space-y-2 overflow-y-auto rounded border border-zinc-200 p-2 dark:border-zinc-800">
                  <SourceGroup label="Sheets">
                    {workbook.sheets.map((sheet) => (
                      <SheetRow
                        key={sheet.name}
                        sheet={sheet}
                        selected={source?.kind === "sheet" && source.name === sheet.name}
                        onSelect={() => setSource({ kind: "sheet", name: sheet.name })}
                      />
                    ))}
                  </SourceGroup>

                  {workbook.tables.length > 0 && (
                    <SourceGroup label="Named tables">
                      {workbook.tables.map((table) => (
                        <SourceRadio
                          key={table.name}
                          name="excel-source"
                          checked={source?.kind === "table" && source.name === table.name}
                          onChange={() => setSource({ kind: "table", name: table.name })}
                          title={table.name}
                          subtitle={`${table.sheet} · ${table.columns.length} col${
                            table.columns.length === 1 ? "" : "s"
                          } · ${table.rows.toLocaleString()} row${table.rows === 1 ? "" : "s"}${
                            table.range ? ` · ${table.range}` : ""
                          }`}
                        />
                      ))}
                    </SourceGroup>
                  )}

                  {workbook.namedRanges.length > 0 && (
                    <SourceGroup label="Named ranges">
                      {workbook.namedRanges.map((nr) => {
                        const resolvable = nr.sheet != null && nr.range != null;
                        return (
                          <SourceRadio
                            key={nr.name}
                            name="excel-source"
                            checked={source?.kind === "namedRange" && source.name === nr.name}
                            disabled={!resolvable}
                            onChange={() => setSource({ kind: "namedRange", name: nr.name })}
                            title={nr.name}
                            subtitle={
                              resolvable
                                ? `${nr.sheet}!${nr.range}`
                                : `${nr.formula} — not a single contiguous range`
                            }
                          />
                        );
                      })}
                    </SourceGroup>
                  )}
                </div>
                {workbook.has1904Epoch && (
                  <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                    This workbook uses the 1904 date system — dates are read on that epoch and never
                    shifted.
                  </p>
                )}
              </div>

              {/* ----- Import options ----- */}
              <div className="space-y-2.5">
                <p className="text-xs font-medium text-zinc-600 dark:text-zinc-300">
                  Import options
                </p>

                {source?.kind === "table" && (
                  <p className="rounded bg-zinc-50 px-2 py-1.5 text-[11px] text-zinc-500 dark:bg-zinc-900 dark:text-zinc-400">
                    The table's own column names are the header row.
                  </p>
                )}

                {source?.kind === "sheet" && (
                  <>
                    <Field label="Cell range">
                      <input
                        type="text"
                        value={ui.range}
                        placeholder="whole used range (e.g. B2:F100)"
                        onChange={(e) => patchUi({ range: e.target.value })}
                        className={`${inputCls} w-full ${
                          rangeValid ? "" : "border-red-500 dark:border-red-500"
                        }`}
                      />
                    </Field>
                    {!rangeValid && (
                      <p className="text-[11px] text-red-600 dark:text-red-400">
                        Enter a single cell or an A1 range like <code>B2:F100</code>, or leave it
                        blank for the whole used range.
                      </p>
                    )}
                  </>
                )}

                {(source?.kind === "sheet" || source?.kind === "namedRange") && (
                  <HeaderPicker
                    ui={ui}
                    candidates={selectedSheet?.headerCandidates ?? []}
                    onChange={patchUi}
                  />
                )}

                <Field label="Merged cells">
                  <select
                    value={ui.merged}
                    onChange={(e) => patchUi({ merged: e.target.value as ExcelMergedPolicy })}
                    className={selectCls}
                  >
                    <option value="topLeftOnly">Keep value in the top-left cell only</option>
                    <option value="repeat">Repeat the value across the region</option>
                  </select>
                </Field>

                <Field label="Formulas">
                  <select
                    value={ui.formula}
                    onChange={(e) => patchUi({ formula: e.target.value as ExcelFormulaPolicy })}
                    className={selectCls}
                  >
                    <option value="cachedResult">Use the cached result</option>
                    <option value="formulaText">Keep the formula text</option>
                    <option value="blank">Leave blank</option>
                  </select>
                </Field>

                <div className="flex flex-wrap gap-x-4 gap-y-1.5 pt-0.5 text-xs">
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={ui.trimBlankRows}
                      onChange={(e) => patchUi({ trimBlankRows: e.target.checked })}
                      className="accent-violet-600"
                    />
                    Trim blank rows
                  </label>
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={ui.trimBlankColumns}
                      onChange={(e) => patchUi({ trimBlankColumns: e.target.checked })}
                      className="accent-violet-600"
                    />
                    Trim blank columns
                  </label>
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={ui.forceIndexed}
                      onChange={(e) => patchUi({ forceIndexed: e.target.checked })}
                      className="accent-violet-600"
                    />
                    Open read-only (indexed)
                  </label>
                </div>
              </div>
            </div>

            {/* ----- No-cached-result warning banner ----- */}
            {noCached > 0 && ui.formula === "cachedResult" && (
              <div className="rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-700 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300">
                {noCached.toLocaleString()} formula cell{noCached === 1 ? "" : "s"} in this
                selection have no cached result. CEESVEE does not evaluate formulas, so they import
                blank under the cached-result policy — switch to “Keep the formula text” to keep the
                source.
              </div>
            )}

            {/* ----- Preview ----- */}
            {preview && (
              <div className="space-y-2">
                <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded bg-zinc-50 px-3 py-2 text-xs dark:bg-zinc-900">
                  <span className="font-medium">{preview.source}</span>
                  <span className="text-zinc-500">
                    {preview.hasHeaderRow ? "with header row" : "no header row"}
                  </span>
                  <span className="tabular-nums">
                    {preview.rowCount.toLocaleString()} row{preview.rowCount === 1 ? "" : "s"} ×{" "}
                    {preview.columnCount} column{preview.columnCount === 1 ? "" : "s"}
                  </span>
                </div>

                {/* Columns */}
                <div className="max-h-36 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
                  <table className="w-full border-collapse text-[11px]">
                    <thead className="sticky top-0 bg-white text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
                      <tr>
                        <th className="px-2 py-1 font-medium">Column</th>
                        <th className="px-2 py-1 font-medium">Type</th>
                        <th className="px-2 py-1 text-right font-medium">Non-empty</th>
                        <th className="px-2 py-1 text-right font-medium">Empty</th>
                      </tr>
                    </thead>
                    <tbody>
                      {shownColumns.map((c, i) => (
                        <tr
                          key={`${c.name}-${i}`}
                          className="border-t border-zinc-100 dark:border-zinc-800"
                        >
                          <td className="max-w-64 truncate px-2 py-1 font-mono" title={c.name}>
                            {c.name || <span className="text-zinc-400">(unnamed)</span>}
                          </td>
                          <td className="px-2 py-1 text-zinc-500">{c.inferredType}</td>
                          <td className="px-2 py-1 text-right tabular-nums">
                            {c.nonEmpty.toLocaleString()}
                          </td>
                          <td className="px-2 py-1 text-right tabular-nums text-zinc-400">
                            {c.empty.toLocaleString()}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                {hiddenColumns > 0 && (
                  <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                    + {hiddenColumns.toLocaleString()} more column
                    {hiddenColumns === 1 ? "" : "s"} not shown (all {preview.columnCount} import).
                  </p>
                )}

                {/* Sample rows */}
                {preview.sampleRows.length > 0 && (
                  <div className="max-h-48 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
                    <table className="border-collapse text-[11px]">
                      <thead className="sticky top-0 bg-white text-left text-zinc-400 dark:bg-zinc-900">
                        <tr>
                          {shownColumns.map((c, i) => (
                            <th
                              key={`${c.name}-${i}`}
                              className="whitespace-nowrap border-b border-zinc-200 px-2 py-1 font-mono font-medium dark:border-zinc-800"
                            >
                              {c.name || "(unnamed)"}
                            </th>
                          ))}
                          {hiddenColumns > 0 && (
                            <th className="whitespace-nowrap border-b border-zinc-200 px-2 py-1 font-medium text-zinc-400 dark:border-zinc-800">
                              +{hiddenColumns.toLocaleString()}…
                            </th>
                          )}
                        </tr>
                      </thead>
                      <tbody>
                        {preview.sampleRows.map((row, ri) => (
                          <tr key={ri} className="border-t border-zinc-100 dark:border-zinc-900">
                            {row.slice(0, MAX_PREVIEW_COLUMNS).map((cell, ci) => (
                              <td
                                key={ci}
                                className="max-w-64 truncate px-2 py-1 font-mono"
                                title={cell}
                              >
                                {cell}
                              </td>
                            ))}
                            {hiddenColumns > 0 && <td className="px-2 py-1 text-zinc-400">…</td>}
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </div>
            )}

            {/* Other warnings */}
            {preview && preview.warnings.length > 0 && (
              <ul className="space-y-0.5 text-xs text-amber-600 dark:text-amber-400">
                {preview.warnings.map((w, i) => (
                  <li key={i}>• {w}</li>
                ))}
              </ul>
            )}

            <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
              Importing creates a new CEESVEE document — there is no in-place `.xlsx` save, and the
              original workbook is never modified.
            </p>
          </>
        )}

        {/* Errors */}
        {(st.previewError ?? deriveError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{st.previewError ?? deriveError}</p>
        )}

        {/* Progress */}
        {scanning && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              Scanning…
              {st.previewTotal != null &&
                st.previewTotal > 0 &&
                ` ${Math.min(100, Math.round((st.previewProcessed / st.previewTotal) * 100))}%`}
            </span>
            <button onClick={() => void cancelExcelPreview()} className={cancelBtn}>
              Cancel scan
            </button>
          </div>
        )}
        {importing && derive && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              {derive.message ?? "importing"} — {derive.processed.toLocaleString()}
              {derive.total != null && ` / ${derive.total.toLocaleString()}`}
            </span>
            <button onClick={() => void cancelDerive()} className={cancelBtn}>
              Cancel
            </button>
          </div>
        )}
      </div>
    </Modal>
  );
}

function HeaderPicker({
  ui,
  candidates,
  onChange,
}: {
  ui: ExcelImportUi;
  candidates: number[];
  onChange: (p: Partial<ExcelImportUi>) => void;
}) {
  const mode = ui.header.type;
  const rowIndex = ui.header.type === "row" ? ui.header.index : 0;
  return (
    <Field label="Header row">
      <div className="flex flex-col gap-1">
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <label className="flex items-center gap-1.5">
            <input
              type="radio"
              name="excel-header"
              checked={mode === "firstRow"}
              onChange={() => onChange({ header: { type: "firstRow" } })}
              className="accent-violet-600"
            />
            First row
          </label>
          <label className="flex items-center gap-1.5">
            <input
              type="radio"
              name="excel-header"
              checked={mode === "row"}
              onChange={() => onChange({ header: { type: "row", index: rowIndex } })}
              className="accent-violet-600"
            />
            Row
            <input
              type="number"
              min={1}
              value={rowIndex + 1}
              disabled={mode !== "row"}
              onChange={(e) =>
                onChange({
                  header: { type: "row", index: Math.max(0, Number(e.target.value) - 1) },
                })
              }
              className={`${inputCls} w-16 disabled:opacity-40`}
            />
          </label>
          <label className="flex items-center gap-1.5">
            <input
              type="radio"
              name="excel-header"
              checked={mode === "none"}
              onChange={() => onChange({ header: { type: "none" } })}
              className="accent-violet-600"
            />
            None
          </label>
        </div>
        {candidates.length > 0 && (
          <div className="flex flex-wrap items-center gap-1 text-[11px] text-zinc-500">
            <span>Detected:</span>
            {candidates.map((c) => (
              <button
                key={c}
                onClick={() =>
                  onChange({ header: c === 0 ? { type: "firstRow" } : { type: "row", index: c } })
                }
                className="rounded border border-zinc-300 px-1.5 py-0.5 hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
              >
                Row {c + 1}
              </button>
            ))}
          </div>
        )}
      </div>
    </Field>
  );
}

function SheetRow({
  sheet,
  selected,
  onSelect,
}: {
  sheet: ExcelSheetInfo;
  selected: boolean;
  onSelect: () => void;
}) {
  const isWorksheet = sheet.kind === "worksheet";
  const selectable = isWorksheet && sheet.hasData;
  const meta: string[] = [];
  if (sheet.hasData) meta.push(`${sheet.usedRows.toLocaleString()} × ${sheet.usedCols}`);
  else meta.push("empty");
  if (sheet.formulaCount > 0) meta.push(`${sheet.formulaCount.toLocaleString()} formula`);
  if (sheet.mergedCount > 0) meta.push(`${sheet.mergedCount.toLocaleString()} merged`);
  return (
    <SourceRadio
      name="excel-source"
      checked={selected}
      disabled={!selectable}
      onChange={onSelect}
      title={sheet.name}
      badges={
        <>
          {sheet.visibility !== "visible" && (
            <Badge tone="amber">{visibilityLabel(sheet.visibility)}</Badge>
          )}
          {!isWorksheet && <Badge tone="zinc">{sheetKindLabel(sheet.kind)}</Badge>}
        </>
      }
      subtitle={meta.join(" · ")}
    />
  );
}

function SourceGroup({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-0.5">
      <p className="px-1 text-[10px] font-semibold uppercase tracking-wide text-zinc-400">
        {label}
      </p>
      {children}
    </div>
  );
}

function SourceRadio({
  name,
  checked,
  disabled = false,
  onChange,
  title,
  subtitle,
  badges,
}: {
  name: string;
  checked: boolean;
  disabled?: boolean;
  onChange: () => void;
  title: string;
  subtitle?: string;
  badges?: React.ReactNode;
}) {
  return (
    <label
      className={`flex items-start gap-2 rounded px-1.5 py-1 ${
        disabled
          ? "cursor-not-allowed opacity-45"
          : "cursor-pointer hover:bg-zinc-100 dark:hover:bg-zinc-800/60"
      } ${checked ? "bg-violet-50 dark:bg-violet-950/40" : ""}`}
    >
      <input
        type="radio"
        name={name}
        checked={checked}
        disabled={disabled}
        onChange={onChange}
        className="mt-0.5 accent-violet-600"
      />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5">
          <span className="truncate text-xs font-medium" title={title}>
            {title}
          </span>
          {badges}
        </span>
        {subtitle && (
          <span className="block truncate text-[11px] text-zinc-500 dark:text-zinc-400">
            {subtitle}
          </span>
        )}
      </span>
    </label>
  );
}

function Badge({ children, tone }: { children: React.ReactNode; tone: "amber" | "zinc" }) {
  const cls =
    tone === "amber"
      ? "bg-amber-100 text-amber-700 dark:bg-amber-950/60 dark:text-amber-300"
      : "bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400";
  return (
    <span className={`shrink-0 rounded px-1 py-px text-[9px] font-semibold uppercase ${cls}`}>
      {children}
    </span>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs text-zinc-500 dark:text-zinc-400">{label}</span>
      {children}
    </label>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const cancelBtn =
  "rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10";
