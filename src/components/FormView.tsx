import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  addGroup,
  assignToGroup,
  changedFields,
  clampRecord,
  draftCommitCells,
  emptyLayout,
  fieldChanged,
  fieldValue,
  isDraftDirty,
  layoutSections,
  parseGoto,
  removeGroup,
  saveBlocked,
  stepRecord,
  toggleHidden,
  type RecordDraft,
} from "../lib/recordForm";
import { SEMANTIC_LABELS } from "../lib/semantics";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { DraftValidation, LogicalType, RecordField, RecordLayout, RecordView } from "../types";
import { ChevronDown, ChevronUp, Close, Layers } from "./Icons";
import { Modal } from "./Modal";

const TYPE_LABELS: Record<LogicalType, string> = {
  text: "Text",
  integer: "Integer",
  decimal: "Decimal",
  float: "Float",
  boolean: "Boolean",
  date: "Date",
  datetime: "Datetime",
  uuid: "UUID",
  json: "JSON",
};

/**
 * Record form (F41): edit ONE visible record at a time — schema-aware field
 * labels, dictionary descriptions, semantic badges, multiline editors, a
 * raw/formatted toggle, a null-vs-blank control, per-field validation, a
 * changed indicator, and prev/next/go-to navigation across visible records.
 * A draft commits every changed field as ONE undo step. Indexed documents get
 * a read-only form. Docks on the right like the explorer / changes panels.
 */
export function FormView() {
  const meta = useActiveMeta();
  const open = useStore((s) => s.recordFormOpen);
  const record = useStore((s) => s.record);
  const autoSave = useStore((s) => s.settings?.autoSaveRecordOnNavigate ?? false);
  const setOpen = useStore((s) => s.setRecordFormOpen);
  const setRow = useStore((s) => s.setRecordRow);
  const setDraftField = useStore((s) => s.setRecordDraftField);
  const clearDraft = useStore((s) => s.clearRecordDraft);
  const saveDraft = useStore((s) => s.saveRecordDraft);
  const setLayout = useStore((s) => s.setRecordLayout);
  const jumpToColumn = useStore((s) => s.jumpToRecordColumn);
  const setAutoSave = useStore((s) => s.setAutoSaveRecordOnNavigate);

  const [view, setView] = useState<RecordView | null>(null);
  const [validation, setValidation] = useState<DraftValidation | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [rawMode, setRawMode] = useState(false);
  const [layoutOpen, setLayoutOpen] = useState(false);
  const [gotoInput, setGotoInput] = useState("");
  // The pending navigation target awaiting an unsaved-draft decision.
  const [pendingNav, setPendingNav] = useState<number | null>(null);

  const docId = meta?.id ?? null;
  const revision = meta?.revision;
  const dataVersion = useStore((s) => s.dataVersion);
  const { row, draft, draftRevision, layout } = record;
  const fields = view?.fields ?? [];
  const dirty = isDraftDirty(fields, draft);
  const readOnly = view?.readOnly ?? meta?.backing === "indexedReadOnly";

  // ---- fetch the record whenever the doc, row or revision moves -------------
  useEffect(() => {
    if (!open || docId == null) return;
    // A filter applied while the form is open can shrink the view under the
    // remembered row; re-clamp (which resets any now-orphaned draft) or, when
    // nothing is visible, drop the view rather than fetch an out-of-range row.
    const clamped = clampRecord(row, meta?.rowCount ?? 0);
    if (clamped === null) {
      setView(null);
      return;
    }
    if (clamped !== row) {
      setRow(clamped);
      return;
    }
    let cancelled = false;
    setLoading(true);
    api
      .fetchRecord(docId, row)
      .then((v) => {
        if (cancelled) return;
        setView(v);
        setError(null);
        // A draft started at an earlier revision may have been remapped by a
        // filter/sort/data change under it — discard it rather than risk the
        // edit landing on a different absolute row.
        if (
          draftRevision != null &&
          draftRevision !== v.revision &&
          isDraftDirty(v.fields, draft)
        ) {
          clearDraft();
          setNotice("The document changed — the unsaved draft was discarded.");
        }
      })
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
    // draft/draftRevision intentionally excluded: this refetches on document
    // movement, not on every keystroke (the draft is compared inside).
    // dataVersion catches structural/dictionary refreshes that reload the grid.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, docId, row, revision, dataVersion]);

  // ---- pre-check the draft (debounced) --------------------------------------
  useEffect(() => {
    if (!open || docId == null || readOnly) {
      setValidation(null);
      return;
    }
    const edits = changedFields(fields, draft);
    if (edits.length === 0) {
      setValidation(null);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      api
        .validateRecordDraft(docId, edits)
        .then((v) => !cancelled && setValidation(v))
        .catch(() => !cancelled && setValidation(null));
    }, 150);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, docId, readOnly, draft, view]);

  const visibleLen = view?.visibleLen ?? meta?.rowCount ?? 0;
  const blocked = saveBlocked(validation);

  const commit = useCallback(async (): Promise<boolean> => {
    if (!view || blocked) return false;
    const cells = draftCommitCells(view.displayRow, view.fields, draft);
    if (cells.length === 0) return true;
    return saveDraft(cells);
  }, [view, blocked, draft, saveDraft]);

  // Navigate to a visible record, honouring the unsaved-draft preference.
  const navigate = useCallback(
    async (target: number | null) => {
      if (target === null || target === row) return;
      setNotice(null);
      if (!dirty) {
        setRow(target);
        return;
      }
      if (autoSave && !blocked) {
        if (await commit()) setRow(target);
        else setPendingNav(target); // save failed → let the user decide
        return;
      }
      // Preference off (or a strict-invalid draft can't auto-save) → prompt.
      setPendingNav(target);
    },
    [row, dirty, autoSave, blocked, commit, setRow],
  );

  if (!meta || !open) return null;

  const dense = layout?.density === "compact";
  const sections = layoutSections(fields, layout);
  const noRecords = visibleLen <= 0;

  return (
    <aside className="flex w-[26rem] shrink-0 flex-col border-l border-zinc-200 bg-white text-sm dark:border-zinc-800 dark:bg-zinc-950">
      {/* Header */}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-zinc-200 px-3 dark:border-zinc-800">
        <span className="font-semibold text-zinc-700 dark:text-zinc-200">Record form</span>
        {readOnly && (
          <span className="rounded bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:bg-amber-500/15 dark:text-amber-300">
            read-only
          </span>
        )}
        <div className="flex-1" />
        <button
          title={rawMode ? "Show formatted values" : "Show raw stored values"}
          onClick={() => setRawMode((r) => !r)}
          className={`rounded px-1.5 py-0.5 text-[11px] ${
            rawMode
              ? "bg-violet-600 text-white"
              : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
          }`}
        >
          {rawMode ? "Raw" : "Formatted"}
        </button>
        <button
          title="Field layout: groups, hidden fields, density"
          onClick={() => setLayoutOpen((o) => !o)}
          className={`rounded p-1 ${
            layoutOpen
              ? "bg-violet-600 text-white"
              : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
          }`}
        >
          <Layers className="h-4 w-4" />
        </button>
        <button
          title="Close record form"
          onClick={() => setOpen(false)}
          className="rounded p-1 text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          <Close className="h-4 w-4" />
        </button>
      </div>

      {/* Navigation bar */}
      <div className="flex shrink-0 items-center gap-1.5 border-b border-zinc-100 px-3 py-2 dark:border-zinc-800/60">
        <button
          title="Previous record"
          disabled={stepRecord(row, -1, visibleLen) === null}
          onClick={() => void navigate(stepRecord(row, -1, visibleLen))}
          className="rounded border border-zinc-300 p-1 text-zinc-600 hover:bg-zinc-100 disabled:opacity-30 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          <ChevronUp className="h-4 w-4" />
        </button>
        <button
          title="Next record"
          disabled={stepRecord(row, 1, visibleLen) === null}
          onClick={() => void navigate(stepRecord(row, 1, visibleLen))}
          className="rounded border border-zinc-300 p-1 text-zinc-600 hover:bg-zinc-100 disabled:opacity-30 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          <ChevronDown className="h-4 w-4" />
        </button>
        <span className="px-1 tabular-nums text-xs text-zinc-500">
          {noRecords ? "no records" : `${row + 1} of ${visibleLen.toLocaleString()}`}
        </span>
        <div className="flex-1" />
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const target = parseGoto(gotoInput, visibleLen);
            setGotoInput("");
            void navigate(target);
          }}
          className="flex items-center gap-1"
        >
          <input
            value={gotoInput}
            onChange={(e) => setGotoInput(e.target.value)}
            placeholder="Go to #"
            inputMode="numeric"
            className="w-16 rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-xs tabular-nums outline-none focus:border-violet-500 dark:border-zinc-700"
          />
        </form>
      </div>

      {notice && (
        <p className="shrink-0 border-b border-amber-200 bg-amber-50 px-3 py-1.5 text-xs text-amber-700 dark:border-amber-900/50 dark:bg-amber-950/40 dark:text-amber-300">
          {notice}
        </p>
      )}
      {error && (
        <p className="shrink-0 border-b border-red-200 bg-red-50 px-3 py-1.5 text-xs text-red-700 dark:border-red-900/50 dark:bg-red-950/40 dark:text-red-300">
          {error}
        </p>
      )}

      {layoutOpen && <LayoutEditor fields={fields} layout={layout} onChange={setLayout} />}

      {/* Fields */}
      <div className={`min-h-0 flex-1 overflow-y-auto px-3 ${dense ? "py-2" : "py-3"}`}>
        {loading && fields.length === 0 ? (
          <p className="py-6 text-center text-xs text-zinc-400">Loading record…</p>
        ) : noRecords ? (
          <p className="py-6 text-center text-xs text-zinc-400">
            This view has no records{meta.filtered ? " (the filter matches nothing)" : ""}.
          </p>
        ) : (
          <div className={dense ? "space-y-3" : "space-y-4"}>
            {sections.map((section, i) => (
              <div key={section.group?.id ?? `default-${i}`}>
                {section.group && (
                  <div className="mb-1.5 text-[11px] font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500">
                    {section.group.name}
                  </div>
                )}
                <div className={dense ? "space-y-2" : "space-y-3"}>
                  {section.fields.map((field) => (
                    <FieldRow
                      key={field.col}
                      field={field}
                      draft={draft}
                      rawMode={rawMode}
                      dense={dense}
                      readOnly={readOnly}
                      verdict={validation?.fields.find((f) => f.col === field.col) ?? null}
                      onEdit={(value) => setDraftField(field.col, value)}
                      onCopy={() => void writeText(fieldValue(field, draft)).catch(() => undefined)}
                      onJump={() => jumpToColumn(field.col)}
                    />
                  ))}
                  {section.fields.length === 0 && (
                    <p className="text-xs text-zinc-400">No fields in this group.</p>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Draft footer */}
      {!readOnly && (
        <div className="shrink-0 space-y-2 border-t border-zinc-200 px-3 py-2 dark:border-zinc-800">
          <label className="flex items-center gap-1.5 text-[11px] text-zinc-500 dark:text-zinc-400">
            <input
              type="checkbox"
              checked={autoSave}
              onChange={(e) => void setAutoSave(e.target.checked)}
            />
            Auto-save draft when moving between records
          </label>
          {blocked && (
            <p className="text-xs text-red-600 dark:text-red-400">
              A strict field is invalid — fix it before saving.
            </p>
          )}
          {!blocked && validation && validation.advisoryWarnings > 0 && (
            <p className="text-xs text-amber-600 dark:text-amber-400">
              {validation.advisoryWarnings} advisory issue
              {validation.advisoryWarnings === 1 ? "" : "s"} will be recorded on save.
            </p>
          )}
          <div className="flex items-center gap-2">
            <button
              onClick={() => void commit()}
              disabled={!dirty || blocked}
              className="flex-1 rounded bg-violet-600 px-2 py-1.5 font-medium text-white hover:bg-violet-500 disabled:opacity-40"
            >
              Save draft
            </button>
            <button
              onClick={() => {
                clearDraft();
                setNotice(null);
              }}
              disabled={!dirty}
              className="rounded border border-zinc-300 px-2 py-1.5 text-zinc-600 hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
            >
              Discard
            </button>
          </div>
        </div>
      )}

      {pendingNav !== null && (
        <UnsavedPrompt
          blocked={blocked}
          onSave={async () => {
            const target = pendingNav;
            setPendingNav(null);
            if (await commit()) setRow(target);
          }}
          onDiscard={() => {
            const target = pendingNav;
            setPendingNav(null);
            setRow(target); // setRecordRow resets the draft
          }}
          onCancel={() => setPendingNav(null)}
        />
      )}
    </aside>
  );
}

// ---------------------------------------------------------------------------
// One field row
// ---------------------------------------------------------------------------

function FieldRow({
  field,
  draft,
  rawMode,
  dense,
  readOnly,
  verdict,
  onEdit,
  onCopy,
  onJump,
}: {
  field: RecordField;
  draft: RecordDraft;
  rawMode: boolean;
  dense: boolean;
  readOnly: boolean;
  verdict: DraftValidation["fields"][number] | null;
  onEdit: (value: string) => void;
  onCopy: () => void;
  onJump: () => void;
}) {
  const value = fieldValue(field, draft);
  const changed = fieldChanged(field, draft);
  const label = field.dictionary?.displayName?.trim() || field.header || `Column ${field.col + 1}`;
  const hasNullTokens = (field.nullTokens?.length ?? 0) > 0;
  const multiline = field.logicalType === "json" || value.includes("\n") || value.length > 48;
  // Show the F31 formatted rendering under the raw editor (unless in raw mode,
  // or it is identical to the stored raw — the backend guarantees they only
  // differ for a valid, patterned cell, so they never disagree about content).
  const showFormatted = !rawMode && !changed && field.display !== field.raw;

  return (
    <div className={changed ? "rounded-md bg-violet-50/40 dark:bg-violet-500/[0.06]" : undefined}>
      <div className="flex items-center gap-1.5">
        <span
          className="min-w-0 flex-1 truncate font-medium text-zinc-700 dark:text-zinc-200"
          title={field.header}
        >
          {label}
        </span>
        {changed && (
          <span className="shrink-0 rounded bg-violet-100 px-1 text-[10px] font-medium text-violet-700 dark:bg-violet-500/20 dark:text-violet-300">
            changed
          </span>
        )}
        <button
          title="Copy value"
          onClick={onCopy}
          className="shrink-0 rounded px-1 text-zinc-400 hover:bg-zinc-100 hover:text-violet-600 dark:hover:bg-zinc-800"
        >
          ⧉
        </button>
        <button
          title="Jump to this column in the grid"
          onClick={onJump}
          className="shrink-0 rounded px-1 text-zinc-400 hover:bg-zinc-100 hover:text-violet-600 dark:hover:bg-zinc-800"
        >
          ▦
        </button>
      </div>

      {/* Badges + header (when a friendlier display name replaced it) */}
      <div className="mt-0.5 flex flex-wrap items-center gap-1 text-[10px]">
        {field.dictionary?.displayName && field.dictionary.displayName.trim() !== field.header && (
          <span className="font-mono text-zinc-400" title="Technical header">
            {field.header}
          </span>
        )}
        {field.logicalType && (
          <span className="rounded bg-zinc-100 px-1 py-0.5 font-medium text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">
            {TYPE_LABELS[field.logicalType]}
          </span>
        )}
        {field.semantic && (
          <span className="rounded bg-sky-100 px-1 py-0.5 font-medium text-sky-700 dark:bg-sky-500/15 dark:text-sky-300">
            {SEMANTIC_LABELS[field.semantic]}
            {field.semanticConfidence != null && ` ${Math.round(field.semanticConfidence * 100)}%`}
          </span>
        )}
        {field.validationMode === "strict" && (
          <span className="rounded bg-red-100 px-1 py-0.5 font-medium text-red-700 dark:bg-red-500/15 dark:text-red-300">
            strict
          </span>
        )}
        {field.nullable === false && (
          <span className="rounded bg-zinc-100 px-1 py-0.5 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
            required
          </span>
        )}
        {field.dictionary?.unit && (
          <span className="text-zinc-400">unit: {field.dictionary.unit}</span>
        )}
      </div>

      {field.dictionary?.description && !dense && (
        <p className="mt-0.5 text-[11px] leading-snug text-zinc-500 dark:text-zinc-400">
          {field.dictionary.description}
        </p>
      )}

      {/* Editor */}
      <div className="mt-1">
        {multiline ? (
          <AutoTextarea value={value} readOnly={readOnly} onChange={onEdit} />
        ) : (
          <input
            value={value}
            readOnly={readOnly}
            onChange={(e) => onEdit(e.target.value)}
            className={editorClass(readOnly)}
          />
        )}
      </div>

      {showFormatted && (
        <p className="mt-0.5 truncate text-[11px] text-zinc-400" title={field.display}>
          Formatted: {field.display}
        </p>
      )}

      {/* Null-vs-blank control (only when the schema declares null tokens) */}
      {hasNullTokens && !readOnly && (
        <div className="mt-1 flex items-center gap-1 text-[10px]">
          <span className="text-zinc-400">set:</span>
          <button
            onClick={() => onEdit(field.nullTokens![0])}
            className={`rounded border px-1 py-0.5 ${
              field.class === "nullToken" && !changed
                ? "border-violet-400 text-violet-600 dark:text-violet-300"
                : "border-zinc-300 text-zinc-500 hover:border-violet-400 dark:border-zinc-700"
            }`}
            title={`Null token “${field.nullTokens![0]}”`}
          >
            null ({field.nullTokens![0]})
          </button>
          <button
            onClick={() => onEdit("")}
            className={`rounded border px-1 py-0.5 ${
              field.class === "empty" && !changed
                ? "border-violet-400 text-violet-600 dark:text-violet-300"
                : "border-zinc-300 text-zinc-500 hover:border-violet-400 dark:border-zinc-700"
            }`}
            title="Empty string"
          >
            blank
          </button>
        </div>
      )}

      {/* Validation + issues */}
      <FieldMessages field={field} verdict={verdict} changed={changed} />
    </div>
  );
}

/** All validation messages for a field, from three distinct sources. */
function FieldMessages({
  field,
  verdict,
  changed,
}: {
  field: RecordField;
  verdict: DraftValidation["fields"][number] | null;
  changed: boolean;
}) {
  return (
    <div className="mt-0.5 space-y-0.5 text-[11px]">
      {/* 1. Live verdict on the pending edit. */}
      {verdict && !verdict.valid && (
        <p
          className={
            verdict.blocks ? "text-red-600 dark:text-red-400" : "text-amber-600 dark:text-amber-400"
          }
        >
          {verdict.blocks ? "Cannot save (strict): " : "Advisory: "}
          {verdict.reason ?? "invalid value"}
        </p>
      )}
      {/* 2. The STORED value is invalid under a schema tightened after it landed. */}
      {!changed && !field.valid && field.reason && (
        <p className="text-amber-600 dark:text-amber-400">
          Stored value is invalid: {field.reason}
        </p>
      )}
      {/* 3. Recorded advisory issues for this cell (the deliberate F31 gap). */}
      {!changed &&
        (field.issues ?? []).map((issue, i) => (
          <p key={i} className="text-zinc-400" title={`revision ${issue.revision}`}>
            Recorded issue: {issue.reason}
          </p>
        ))}
    </div>
  );
}

/** A textarea that grows with its content (bounded), for long / multiline text. */
function AutoTextarea({
  value,
  readOnly,
  onChange,
}: {
  value: string;
  readOnly: boolean;
  onChange: (v: string) => void;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 260)}px`;
  }, [value]);
  return (
    <textarea
      ref={ref}
      value={value}
      readOnly={readOnly}
      rows={2}
      onChange={(e) => onChange(e.target.value)}
      className={`${editorClass(readOnly)} resize-none`}
    />
  );
}

function editorClass(readOnly: boolean): string {
  return `w-full rounded border border-zinc-300 bg-transparent px-2 py-1 font-mono text-xs outline-none focus:border-violet-500 dark:border-zinc-700 ${
    readOnly ? "cursor-default text-zinc-500" : ""
  }`;
}

// ---------------------------------------------------------------------------
// Layout editor (groups, hidden fields, density)
// ---------------------------------------------------------------------------

function LayoutEditor({
  fields,
  layout,
  onChange,
}: {
  fields: RecordField[];
  layout: RecordLayout | null;
  onChange: (layout: RecordLayout | null) => void;
}) {
  const base = layout ?? emptyLayout();
  const groupOf = useMemo(() => {
    const map = new Map<string, string>();
    for (const g of base.groups) for (const id of g.columnIds) map.set(id, g.id);
    return map;
  }, [base.groups]);

  const setDensity = (density: RecordLayout["density"]) => onChange({ ...base, density });
  const newGroup = () => {
    const id = `g${Math.random().toString(36).slice(2, 9)}`;
    onChange(addGroup(base, id, `Group ${base.groups.length + 1}`));
  };
  const renameGroup = (id: string, name: string) =>
    onChange({ ...base, groups: base.groups.map((g) => (g.id === id ? { ...g, name } : g)) });

  const isTrivial =
    base.density === "comfortable" && base.hiddenColumnIds.length === 0 && base.groups.length === 0;

  return (
    <div className="shrink-0 space-y-2 border-b border-zinc-200 bg-zinc-50 px-3 py-2 text-xs dark:border-zinc-800 dark:bg-zinc-900/50">
      <div className="flex items-center gap-2">
        <span className="font-semibold text-zinc-600 dark:text-zinc-300">Layout</span>
        <div className="flex overflow-hidden rounded border border-zinc-300 text-[11px] dark:border-zinc-700">
          {(["comfortable", "compact"] as const).map((d) => (
            <button
              key={d}
              onClick={() => setDensity(d)}
              className={`px-2 py-0.5 ${
                base.density === d
                  ? "bg-violet-600 text-white"
                  : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              }`}
            >
              {d}
            </button>
          ))}
        </div>
        <div className="flex-1" />
        <button
          onClick={newGroup}
          className="rounded border border-zinc-300 px-1.5 py-0.5 hover:border-violet-400 dark:border-zinc-700"
        >
          + Group
        </button>
        <button
          onClick={() => onChange(null)}
          disabled={isTrivial}
          title="Reset to automatic (schema order)"
          className="rounded px-1 py-0.5 text-zinc-500 hover:text-violet-600 disabled:opacity-30"
        >
          Reset
        </button>
      </div>

      {base.groups.length > 0 && (
        <div className="space-y-1">
          {base.groups.map((g) => (
            <div key={g.id} className="flex items-center gap-1">
              <input
                value={g.name}
                onChange={(e) => renameGroup(g.id, e.target.value)}
                className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700"
              />
              <button
                onClick={() => onChange(removeGroup(base, g.id))}
                title="Delete group (its fields return to the default section)"
                className="rounded px-1 text-zinc-400 hover:text-red-500"
              >
                ✕
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="max-h-48 space-y-0.5 overflow-y-auto">
        {fields.map((f) => {
          const hidden = base.hiddenColumnIds.includes(f.columnId);
          const label = f.dictionary?.displayName?.trim() || f.header || `Column ${f.col + 1}`;
          return (
            <div key={f.col} className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={!hidden}
                title={hidden ? "Show field" : "Hide field"}
                onChange={() => onChange(toggleHidden(base, f.columnId))}
              />
              <span
                className={`min-w-0 flex-1 truncate ${hidden ? "text-zinc-400 line-through" : "text-zinc-600 dark:text-zinc-300"}`}
                title={f.header}
              >
                {label}
              </span>
              <select
                value={groupOf.get(f.columnId) ?? ""}
                onChange={(e) => onChange(assignToGroup(base, f.columnId, e.target.value || null))}
                className="max-w-[7rem] rounded border border-zinc-300 bg-transparent px-1 py-0.5 text-[11px] outline-none dark:border-zinc-700"
              >
                <option value="" className="dark:bg-zinc-800">
                  (ungrouped)
                </option>
                {base.groups.map((g) => (
                  <option key={g.id} value={g.id} className="dark:bg-zinc-800">
                    {g.name}
                  </option>
                ))}
              </select>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Unsaved-draft navigation prompt
// ---------------------------------------------------------------------------

function UnsavedPrompt({
  blocked,
  onSave,
  onDiscard,
  onCancel,
}: {
  blocked: boolean;
  onSave: () => void;
  onDiscard: () => void;
  onCancel: () => void;
}) {
  return (
    <Modal
      title="Unsaved record changes"
      onClose={onCancel}
      footer={
        <>
          <button
            onClick={onCancel}
            className="rounded border border-zinc-300 px-3 py-1.5 text-sm hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          <button
            onClick={onDiscard}
            className="rounded border border-red-300 px-3 py-1.5 text-sm text-red-600 hover:bg-red-50 dark:border-red-900/60 dark:text-red-400 dark:hover:bg-red-950/40"
          >
            Discard &amp; continue
          </button>
          <button
            onClick={onSave}
            disabled={blocked}
            title={blocked ? "A strict field is invalid — fix or discard" : undefined}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-violet-500 disabled:opacity-40"
          >
            Save &amp; continue
          </button>
        </>
      }
    >
      <p className="text-sm text-zinc-600 dark:text-zinc-300">
        This record has unsaved field edits.
        {blocked
          ? " A strict field is invalid, so the draft can't be saved — discard it or cancel to fix it."
          : " Save them before moving to another record, or discard them?"}
      </p>
    </Modal>
  );
}
