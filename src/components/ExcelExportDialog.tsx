import { useMemo, useState } from "react";

import {
  checkExportLimits,
  columnWidthsLabel,
  dedupeSheetNames,
  defaultExportOptions,
  gridWidthsPx,
  outputColumnsForScope,
  sanitizeSheetName,
  sizingForScope,
  suggestExcelFileName,
  validateSheetNames,
  type SheetSizing,
} from "../lib/excel";
import { scopeChoices } from "../lib/export";
import { useActiveMeta, useStore } from "../store/useStore";
import type {
  ExcelColumnWidths,
  ExcelExportOptions,
  ExcelSheetExport,
  ExportScope,
} from "../types";
import { Modal } from "./Modal";

type Mode = "active" | "tabs";
const COLUMN_WIDTHS: ExcelColumnWidths[] = ["default", "autofit", "grid"];

/**
 * Excel `.xlsx` export (F34). Two modes: one sheet from the active document
 * (with the usual scope choices, and optional grid column widths), or one sheet
 * per selected open tab into a single workbook. Header styling, a frozen header
 * row, an autofilter, typed emission and column widths are all optional. Excel's
 * row/column limits and sheet-name rules are checked BEFORE the write is offered
 * (the backend re-checks and rejects too). Exports never touch a save point.
 */
export function ExcelExportDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const tabs = useStore((s) => s.tabs);
  const activeId = useStore((s) => s.activeId);
  const exportExcel = useStore((s) => s.exportExcel);
  const columnWidths = useStore((s) => s.columnWidths);
  const uiStates = useStore((s) => s.uiStates);

  const filtered = useStore((s) => s.tabs.find((t) => t.id === s.activeId)?.filtered ?? false);
  const selectedRows = useStore((s) => s.selectedRows);
  const selectedCols = useStore((s) => s.selectedCols);
  const selectionRect = useStore((s) => s.selectionPhysicalRect)();
  const viewSorted = meta?.viewSorted ?? false;

  const [mode, setMode] = useState<Mode>("active");
  const [opts, setOpts] = useState<ExcelExportOptions>(() => defaultExportOptions());

  // Active-document mode: scope + sheet name.
  const choices = useMemo(
    () => scopeChoices(filtered, selectionRect, selectedRows, selectedCols, viewSorted),
    [filtered, selectionRect, selectedRows, selectedCols, viewSorted],
  );
  const [scopeIdx, setScopeIdx] = useState(0);
  const [activeName, setActiveName] = useState(() =>
    meta ? sanitizeSheetName(meta.fileName) : "Sheet",
  );

  // Multi-tab mode: which tabs are included, and each tab's (editable) sheet name.
  const [selected, setSelected] = useState<Set<number>>(
    () => new Set(activeId != null ? [activeId] : []),
  );
  const [tabNames, setTabNames] = useState<Record<number, string>>(() => {
    const deduped = dedupeSheetNames(tabs.map((t) => sanitizeSheetName(t.fileName)));
    const out: Record<number, string> = {};
    tabs.forEach((t, i) => (out[t.id] = deduped[i]));
    return out;
  });

  const patch = (p: Partial<ExcelExportOptions>) => setOpts((o) => ({ ...o, ...p }));

  const widthsForTab = (tabId: number): Record<number, number> =>
    tabId === activeId ? columnWidths : (uiStates[tabId]?.columnWidths ?? {});

  // Build the wire sheet set and the projected sizing for the limit pre-check.
  const { sheets, sizings } = useMemo(() => {
    if (mode === "active") {
      if (!meta) return { sheets: [] as ExcelSheetExport[], sizings: [] as SheetSizing[] };
      const scope: ExportScope = (choices[scopeIdx] ?? choices[0]).scope;
      const outCols = outputColumnsForScope(scope, meta.colCount);
      const sheet: ExcelSheetExport = {
        docId: meta.id,
        name: activeName,
        scope,
        expectedRevision: meta.revision,
        gridWidthsPx:
          opts.columnWidths === "grid" ? gridWidthsPx(columnWidths, outCols) : undefined,
      };
      const sizing = sizingForScope(activeName, scope, {
        totalRows: meta.totalRowCount,
        visibleRows: meta.rowCount,
        columns: meta.colCount,
        hasHeader: meta.hasHeaderRow,
      });
      return { sheets: [sheet], sizings: [sizing] };
    }
    // Multi-tab: one full sheet per selected tab, in tab order.
    const included = tabs.filter((t) => selected.has(t.id));
    const allScope: ExportScope = { type: "all" };
    const sheetList: ExcelSheetExport[] = included.map((t) => {
      const outCols = outputColumnsForScope(allScope, t.colCount);
      return {
        docId: t.id,
        name: tabNames[t.id] ?? sanitizeSheetName(t.fileName),
        scope: allScope,
        expectedRevision: t.revision,
        gridWidthsPx:
          opts.columnWidths === "grid" ? gridWidthsPx(widthsForTab(t.id), outCols) : undefined,
      };
    });
    const sizingList: SheetSizing[] = included.map((t) => ({
      name: tabNames[t.id] ?? sanitizeSheetName(t.fileName),
      dataRows: t.totalRowCount,
      columns: t.colCount,
      hasHeader: t.hasHeaderRow,
    }));
    return { sheets: sheetList, sizings: sizingList };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    mode,
    meta,
    choices,
    scopeIdx,
    activeName,
    tabs,
    selected,
    tabNames,
    opts.columnWidths,
    columnWidths,
    uiStates,
    activeId,
  ]);

  const nameIssues = useMemo(() => validateSheetNames(sheets.map((s) => s.name)), [sheets]);
  const limitViolations = useMemo(() => checkExportLimits(sizings), [sizings]);

  if (!meta) return null;

  const blocked = sheets.length === 0 || nameIssues.length > 0 || limitViolations.length > 0;

  const doExport = () => {
    if (blocked) return;
    const suggested = suggestExcelFileName(
      mode === "active"
        ? meta.fileName
        : (tabs.find((t) => selected.has(t.id))?.fileName ?? meta.fileName),
    );
    void exportExcel(sheets, opts, suggested);
    onClose();
  };

  const toggleTab = (id: number) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  return (
    <Modal
      title="Export to Excel"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={doExport}
            disabled={blocked}
            title={nameIssues[0]?.message ?? limitViolations[0]?.message}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            Choose file & export
          </button>
        </>
      }
    >
      <div className="max-h-[70vh] space-y-3 overflow-y-auto text-sm">
        {/* Mode */}
        <div className="flex overflow-hidden rounded border border-zinc-300 text-xs dark:border-zinc-700">
          <ModeButton active={mode === "active"} onClick={() => setMode("active")}>
            This document (one sheet)
          </ModeButton>
          <ModeButton
            active={mode === "tabs"}
            onClick={() => setMode("tabs")}
            disabled={tabs.length < 2}
          >
            Open tabs (one sheet each)
          </ModeButton>
        </div>

        {mode === "active" ? (
          <>
            <Row label="Rows to export">
              <select
                value={scopeIdx}
                onChange={(e) => setScopeIdx(Number(e.target.value))}
                className={selectCls}
              >
                {choices.map((c, i) => (
                  <option key={c.label} value={i} className="dark:bg-zinc-800">
                    {c.label}
                  </option>
                ))}
              </select>
            </Row>
            <Row label="Sheet name">
              <input
                type="text"
                value={activeName}
                onChange={(e) => setActiveName(e.target.value)}
                className={`${inputCls} w-56`}
              />
            </Row>
          </>
        ) : (
          <div className="space-y-1">
            <p className="text-xs font-medium text-zinc-600 dark:text-zinc-300">
              Tabs to include (a sheet per tab)
            </p>
            <div className="max-h-52 space-y-1 overflow-y-auto rounded border border-zinc-200 p-2 dark:border-zinc-800">
              {tabs.map((t) => (
                <div key={t.id} className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={selected.has(t.id)}
                    onChange={() => toggleTab(t.id)}
                    className="accent-violet-600"
                  />
                  <span className="w-40 shrink-0 truncate text-xs text-zinc-500" title={t.fileName}>
                    {t.fileName}
                  </span>
                  <input
                    type="text"
                    value={tabNames[t.id] ?? ""}
                    disabled={!selected.has(t.id)}
                    onChange={(e) => setTabNames((n) => ({ ...n, [t.id]: e.target.value }))}
                    className={`${inputCls} flex-1 disabled:opacity-40`}
                    placeholder="sheet name"
                  />
                  <span className="w-24 shrink-0 text-right text-[11px] tabular-nums text-zinc-400">
                    {t.totalRowCount.toLocaleString()} × {t.colCount}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}

        <hr className="border-zinc-100 dark:border-zinc-800" />

        {/* Options */}
        <div className="flex flex-wrap gap-x-5 gap-y-2">
          <Check checked={opts.headerStyle} onChange={(v) => patch({ headerStyle: v })}>
            Style the header (bold + fill)
          </Check>
          <Check checked={opts.freezeHeader} onChange={(v) => patch({ freezeHeader: v })}>
            Freeze the header row
          </Check>
          <Check checked={opts.autofilter} onChange={(v) => patch({ autofilter: v })}>
            Add an autofilter
          </Check>
          <Check checked={opts.typed} onChange={(v) => patch({ typed: v })}>
            Typed numbers, dates & booleans (from the schema)
          </Check>
          <Check
            checked={opts.backup === "single"}
            onChange={(v) => patch({ backup: v ? "single" : "none" })}
          >
            Keep .bak of a replaced file
          </Check>
        </div>

        <Row label="Column widths">
          <select
            value={opts.columnWidths}
            onChange={(e) => patch({ columnWidths: e.target.value as ExcelColumnWidths })}
            className={selectCls}
          >
            {COLUMN_WIDTHS.map((w) => (
              <option key={w} value={w} className="dark:bg-zinc-800">
                {columnWidthsLabel(w)}
              </option>
            ))}
          </select>
        </Row>

        {/* Sheet-name issues */}
        {nameIssues.length > 0 && (
          <ul className="space-y-0.5 text-xs text-red-600 dark:text-red-400">
            {nameIssues.map((issue, i) => (
              <li key={i}>• {issue.message}</li>
            ))}
          </ul>
        )}

        {/* Limit pre-check */}
        {limitViolations.length > 0 && (
          <ul className="space-y-0.5 text-xs text-red-600 dark:text-red-400">
            {limitViolations.map((v, i) => (
              <li key={i}>• {v.message}</li>
            ))}
          </ul>
        )}

        <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
          Values only — formulas are never written. Cells invalid under their declared schema are
          exported as text. Excel's 1,048,576-row × 16,384-column limits are enforced before
          writing.
        </p>
      </div>
    </Modal>
  );
}

function ModeButton({
  active,
  onClick,
  disabled = false,
  children,
}: {
  active: boolean;
  onClick: () => void;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`flex-1 px-2.5 py-1.5 ${
        active
          ? "bg-violet-600 text-white"
          : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
      } disabled:cursor-not-allowed disabled:opacity-40`}
    >
      {children}
    </button>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="text-zinc-500 dark:text-zinc-400">{label}</span>
      {children}
    </div>
  );
}

function Check({
  checked,
  onChange,
  children,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  children: React.ReactNode;
}) {
  return (
    <label className="flex items-center gap-2">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="accent-violet-600"
      />
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
