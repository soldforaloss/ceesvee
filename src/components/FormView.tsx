import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { buildRecordIndex, tagColor } from "../lib/annotations";
import {
  addGroup,
  assignToGroup,
  changedFields,
  clampRecord,
  emptyLayout,
  fieldCellNotes,
  fieldChanged,
  fieldValue,
  isDraftDirty,
  layoutSections,
  parseGoto,
  recordViewCurrent,
  recordViewToken,
  removeGroup,
  saveBlocked,
  stepRecord,
  toggleHidden,
  type RecordDraft,
} from "../lib/recordForm";
import { SEMANTIC_LABELS } from "../lib/semantics";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type {
  DraftValidation,
  LogicalType,
  RecordField,
  RecordLayout,
  RecordView,
  RowAnnotationView,
} from "../types";
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

/** The friendliest display label for a field: the data-dictionary display name,
 * else the technical header, else a positional fallback. */
function fieldLabel(field: RecordField): string {
  return field.dictionary?.displayName?.trim() || field.header || `Column ${field.col + 1}`;
}

// Inline annotation glyphs, matching the grid gutter / annotations panel
// vocabulary (star = amber, flag = rose, note = violet).
function StarGlyph({ on }: { on: boolean }) {
  return (
    <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" aria-hidden>
      <path
        d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77 5.82 21l1.18-6.88-5-4.87 6.91-1.01L12 2z"
        fill={on ? "#f59e0b" : "none"}
        stroke="#f59e0b"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function FlagGlyph({ on }: { on: boolean }) {
  return (
    <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" aria-hidden>
      <path
        d="M5 21V4h11l-1.5 4L16 12H5"
        fill={on ? "#f43f5e" : "none"}
        stroke="#f43f5e"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function NoteGlyph({ on }: { on: boolean }) {
  return (
    <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" aria-hidden>
      <path
        d="M4 4h16v11H9l-4 4V4z"
        fill={on ? "#8b5cf6" : "none"}
        stroke={on ? "#8b5cf6" : "currentColor"}
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
    </svg>
  );
}

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
  // F40 annotations wired into the form: the row's star/flag/tag strip and the
  // per-field cell-note indicators reuse F40's commands, dialogs and glyphs.
  const annotationsView = useStore((s) => s.annotationsView);
  const applyRowMarks = useStore((s) => s.applyRowMarks);
  const openTagPicker = useStore((s) => s.openTagPicker);
  const openRowNoteEditor = useStore((s) => s.openRowNoteEditor);
  const openCellNoteEditor = useStore((s) => s.openCellNoteEditor);
  const loadAnnotations = useStore((s) => s.loadAnnotations);

  const [view, setView] = useState<RecordView | null>(null);
  // The (document, row) token the loaded `view` answered to — compared against
  // the live target below to detect the window after a navigation / tab switch
  // where the async refetch has not yet replaced the previous record's fields.
  const [viewToken, setViewToken] = useState<string | null>(null);
  const [validation, setValidation] = useState<DraftValidation | null>(null);
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
  // Schema/dictionary edits move independently of the document revision and
  // dataVersion (setColumnSchema only refreshes schemaInfo; dictionary edits
  // only refresh dictionaryView), so the fetch effect watches them explicitly —
  // otherwise the joined field labels, formats and validity badges go stale.
  const schemaRevision = useStore((s) => s.schemaInfo?.schemaRevision ?? null);
  const dictionaryRevision = useStore((s) => s.dictionaryView?.dictionaryRevision ?? null);
  const { row, draft, draftRevision, layout } = record;
  const fields = view?.fields ?? [];
  const dirty = isDraftDirty(fields, draft);
  const readOnly = view?.readOnly ?? meta?.backing === "indexedReadOnly";

  // The record the form now points at, and whether the loaded `view` still
  // belongs to it. A navigation (or a switch to another document/tab) moves the
  // target before the async `fetch_record` resolves; until it does, the view is
  // stale and the form must NOT render its editable fields or commit against it
  // (that would write the draft onto the previous record — the P1 bug).
  const viewTarget = docId != null ? recordViewToken(docId, row) : null;
  const current = recordViewCurrent(viewToken, viewTarget);

  // The resolved F40 annotation for the record on show (matched entries are
  // keyed by absolute row — the same coordinate `fetch_record` reads at). Drives
  // the header star/flag/tag strip and the per-field cell-note indicators.
  // Annotations are pure metadata, so they stay available on a read-only form.
  const recordIndex = useMemo(() => buildRecordIndex(annotationsView), [annotationsView]);
  const entry = view ? recordIndex.get(view.absRow) : undefined;
  const cellNotes = useMemo(() => fieldCellNotes(entry), [entry]);

  // ---- fetch the record whenever the doc, row or revision moves -------------
  useEffect(() => {
    if (!open || docId == null) return;
    // A filter applied while the form is open can shrink the view under the
    // remembered row; re-clamp (which resets any now-orphaned draft) or, when
    // nothing is visible, drop the view rather than fetch an out-of-range row.
    const clamped = clampRecord(row, meta?.rowCount ?? 0);
    if (clamped === null) {
      setView(null);
      setViewToken(null);
      return;
    }
    if (clamped !== row) {
      setRow(clamped);
      return;
    }
    // The identity this fetch answers to — stamped onto the view on resolve so a
    // superseded-but-uncancelled result can never masquerade as current.
    const token = recordViewToken(docId, row);
    let cancelled = false;
    api
      .fetchRecord(docId, row)
      .then((v) => {
        if (cancelled) return;
        setView(v);
        setViewToken(token);
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
      .catch((e) => !cancelled && setError(String(e)));
    return () => {
      cancelled = true;
    };
    // draft/draftRevision intentionally excluded: this refetches on document
    // movement, not on every keystroke (the draft is compared inside).
    // dataVersion catches structural refreshes that reload the grid;
    // schema/dictionary revisions catch metadata-only edits (declared type,
    // format, dictionary label) that repaint fields without moving the revision.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, docId, row, revision, dataVersion, schemaRevision, dictionaryRevision]);

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

  // Make sure the annotation surface is loaded for the strip/indicators when the
  // form opens (the grid normally loads it, but the form can be the first to
  // need it). Idempotent and cheap; re-resolves against the current view.
  useEffect(() => {
    if (open && docId != null && annotationsView === null) void loadAnnotations();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, docId, annotationsView]);

  const visibleLen = view?.visibleLen ?? meta?.rowCount ?? 0;
  const blocked = saveBlocked(validation);

  const commit = useCallback(async (): Promise<boolean> => {
    // `current` gates the write: if the loaded view is stale (a navigation or
    // tab switch is mid-refetch), refuse rather than commit `view.displayRow` —
    // which is still the PREVIOUS record — under a revision that may coincide.
    if (!view || blocked || !current) return false;
    const edits = changedFields(view.fields, draft);
    if (edits.length === 0) return true;
    // Guard the commit with the revision the fields were READ at (view.revision).
    // If the document moved under the draft (a filter/sort/structural change can
    // remap which absolute row this display index points at), the backend
    // rejects the save and the draft is discarded rather than written onto the
    // wrong row — surface the same notice as the reactive refetch path.
    const result = await saveDraft(view.displayRow, edits, view.revision);
    if (result === "stale") {
      setNotice("The document changed — the unsaved draft was discarded.");
      return false;
    }
    return result === "saved" || result === "noop";
  }, [view, blocked, current, draft, saveDraft]);

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

      {/* Row bookmarks/tags/notes strip (F40): mark the whole record. Gated on
          `current` so a mid-refetch stale view never pairs the previous
          record's marks with the newly-targeted row's actions. */}
      {current && view && (
        <RowAnnotationStrip
          entry={entry}
          onStar={() => void applyRowMarks([row], { star: !(entry?.star ?? false) })}
          onFlag={() => void applyRowMarks([row], { flag: !(entry?.flag ?? false) })}
          onTags={() => openTagPicker([row])}
          onNote={() => openRowNoteEditor(row, `Row ${view.absRow + 1}`, entry?.note?.text ?? "")}
        />
      )}

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
        {noRecords ? (
          <p className="py-6 text-center text-xs text-zinc-400">
            This view has no records{meta.filtered ? " (the filter matches nothing)" : ""}.
          </p>
        ) : !current ? (
          // Stale/absent view: a fetch for the current record is in flight. Show
          // loading rather than the previous record's editable fields, so no
          // edit can be started (or saved) against a row the form has left.
          <p className="py-6 text-center text-xs text-zinc-400">Loading record…</p>
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
                      hasNote={cellNotes.has(field.columnId)}
                      onEdit={(value) => setDraftField(field.col, value)}
                      onCopy={() => void writeText(fieldValue(field, draft)).catch(() => undefined)}
                      onJump={() => jumpToColumn(field.col)}
                      onNote={() =>
                        openCellNoteEditor(
                          row,
                          field.columnId,
                          `Row ${(view?.absRow ?? row) + 1} · ${fieldLabel(field)}`,
                          cellNotes.get(field.columnId) ?? "",
                        )
                      }
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
              disabled={!dirty || blocked || !current}
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
  hasNote,
  onEdit,
  onCopy,
  onJump,
  onNote,
}: {
  field: RecordField;
  draft: RecordDraft;
  rawMode: boolean;
  dense: boolean;
  readOnly: boolean;
  verdict: DraftValidation["fields"][number] | null;
  /** Whether this field carries an F40 cell note (drives the indicator). */
  hasNote: boolean;
  onEdit: (value: string) => void;
  onCopy: () => void;
  onJump: () => void;
  /** Open the reused cell-note editor for this field. */
  onNote: () => void;
}) {
  const value = fieldValue(field, draft);
  const changed = fieldChanged(field, draft);
  const label = fieldLabel(field);
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
        <button
          title={hasNote ? "Edit cell note" : "Add cell note"}
          onClick={onNote}
          className={`shrink-0 rounded p-0.5 ${
            hasNote
              ? "text-violet-600 hover:bg-violet-100 dark:text-violet-300 dark:hover:bg-violet-900/40"
              : "text-zinc-400 hover:bg-zinc-100 hover:text-violet-600 dark:hover:bg-zinc-800"
          }`}
        >
          <NoteGlyph on={hasNote} />
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
// Row bookmarks / tags / notes strip (F40 reuse)
// ---------------------------------------------------------------------------

/**
 * The whole-record annotation strip for the form header: toggle a star or flag,
 * open the tag picker, and open the row-note editor — all reusing F40's store
 * commands and globally-mounted dialogs. Reflects the record's current
 * annotation state; annotations are pure metadata, so it stays live on a
 * read-only form.
 */
function RowAnnotationStrip({
  entry,
  onStar,
  onFlag,
  onTags,
  onNote,
}: {
  entry: RowAnnotationView | undefined;
  onStar: () => void;
  onFlag: () => void;
  onTags: () => void;
  onNote: () => void;
}) {
  const starred = entry?.star ?? false;
  const flagged = entry?.flag ?? false;
  const tags = entry?.tags ?? [];
  const hasNote = entry?.note != null;
  const btn = (active: boolean) =>
    `flex items-center gap-1 rounded border px-1.5 py-0.5 text-[11px] ${
      active
        ? "border-violet-300 bg-violet-50 text-violet-700 dark:border-violet-500/40 dark:bg-violet-500/10 dark:text-violet-300"
        : "border-zinc-300 text-zinc-500 hover:border-violet-400 hover:text-violet-600 dark:border-zinc-700 dark:hover:border-violet-500/50"
    }`;

  return (
    <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-zinc-100 px-3 py-2 dark:border-zinc-800/60">
      <button
        title={starred ? "Remove star" : "Star this record"}
        onClick={onStar}
        className={btn(starred)}
      >
        <StarGlyph on={starred} />
        Star
      </button>
      <button
        title={flagged ? "Remove flag" : "Flag this record"}
        onClick={onFlag}
        className={btn(flagged)}
      >
        <FlagGlyph on={flagged} />
        Flag
      </button>
      <button title="Tags…" onClick={onTags} className={btn(tags.length > 0)}>
        # Tags{tags.length > 0 ? ` (${tags.length})` : ""}
      </button>
      <button
        title={hasNote ? "Edit row note" : "Add row note"}
        onClick={onNote}
        className={btn(hasNote)}
      >
        <NoteGlyph on={hasNote} />
        Note
      </button>
      {tags.length > 0 && (
        <div className="flex w-full flex-wrap gap-1 pt-0.5">
          {tags.map((t) => (
            <span
              key={t}
              className="rounded px-1.5 py-0.5 text-[10px] font-medium text-white"
              style={{ background: tagColor(t) }}
            >
              {t}
            </span>
          ))}
        </div>
      )}
    </div>
  );
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
          const label = fieldLabel(f);
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
