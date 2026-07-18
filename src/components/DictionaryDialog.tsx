import { useEffect, useMemo, useRef, useState } from "react";

import {
  FIELD_KEY_LABELS,
  MATCH_BY_OPTIONS,
  ROLE_LABELS,
  ROLE_OPTIONS,
  SENSITIVITY_LABELS,
  SENSITIVITY_OPTIONS,
  allConflictsResolved,
  applyMatchBy,
  bulkChoices,
  completeness,
  conflictKey,
  buildPerFieldResolution,
  isDocumented,
  isSensitive,
  normalizeField,
  unresolvedCount,
  type ConflictChoices,
} from "../lib/dictionary";
import { LOGICAL_TYPE_LABELS } from "../lib/schema";
import { useActiveMeta, useStore } from "../store/useStore";
import type {
  DictionaryEntryView,
  DictionaryField,
  DictionaryFormat,
  FieldRole,
  MergeMatchBy,
  MergePlan,
  MergeResolution,
  Sensitivity,
} from "../types";
import { Modal } from "./Modal";

/**
 * Data dictionary (F38): a searchable, per-column documentation editor. Each
 * column's technical name and inferred F31 type are prefilled; every
 * DictionaryField is editable (display name, description, role, unit, source,
 * sensitivity, allowed values, example, owner, notes). Documentation is
 * metadata — edits never dirty the source document. From here the user can
 * export the dictionary (JSON / Markdown / CSV), import and MERGE a dictionary
 * (matching by column ID or name, resolving field-level conflicts explicitly),
 * and clean up orphaned entries left when a documented column is deleted.
 */
export function DictionaryDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const view = useStore((s) => s.dictionaryView);
  const focusColumn = useStore((s) => s.dictionaryDialogColumn);
  const loadDictionary = useStore((s) => s.loadDictionary);
  const setField = useStore((s) => s.setDictionaryField);
  const removeField = useStore((s) => s.removeDictionaryField);
  const discardOrphans = useStore((s) => s.discardDictionaryOrphans);
  const exportToFile = useStore((s) => s.exportDictionaryToFile);
  const pickImportFile = useStore((s) => s.pickDictionaryImportFile);
  const previewImport = useStore((s) => s.previewDictionaryImport);
  const applyImport = useStore((s) => s.applyDictionaryImport);

  const entries = useMemo(() => view?.entries ?? [], [view]);
  const orphans = view?.orphans ?? [];

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [draft, setDraft] = useState<DictionaryField | null>(null);
  const [allowedInput, setAllowedInput] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  // Import sub-flow state.
  const [importPath, setImportPath] = useState<string | null>(null);
  const [matchBy, setMatchBy] = useState<MergeMatchBy>("auto");
  const [plan, setPlan] = useState<MergePlan | null>(null);
  const [importBusy, setImportBusy] = useState(false);
  const [conflictOpen, setConflictOpen] = useState(false);
  const [importSummary, setImportSummary] = useState<string | null>(null);
  // Monotonic id of the most recently requested preview. A preview that
  // resolves after a newer request (a changed Match by selector) or after the
  // panel was cancelled is stale and must not populate the displayed plan.
  const previewSeq = useRef(0);

  useEffect(() => {
    void loadDictionary();
  }, [loadDictionary]);

  // Pick an initial column: the one the caller focused, else the first.
  useEffect(() => {
    if (selectedId !== null || entries.length === 0) return;
    const focused =
      focusColumn != null ? entries.find((e) => e.columnIndex === focusColumn) : undefined;
    setSelectedId((focused ?? entries[0]).columnId);
  }, [entries, focusColumn, selectedId]);

  const selectedEntry: DictionaryEntryView | undefined = useMemo(
    () => entries.find((e) => e.columnId === selectedId),
    [entries, selectedId],
  );
  const storedKey = selectedEntry ? JSON.stringify(selectedEntry.field) : "";

  // Seed the editable draft from the stored field whenever the selected column
  // (or its stored documentation) changes. Typing does not persist until Save.
  useEffect(() => {
    if (!selectedEntry) {
      setDraft(null);
      return;
    }
    setDraft({
      ...selectedEntry.field,
      allowedValues: [...(selectedEntry.field.allowedValues ?? [])],
    });
    setAllowedInput("");
    setNotice(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId, storedKey]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter(
      (e) =>
        e.columnName.toLowerCase().includes(q) ||
        (e.field.displayName ?? "").toLowerCase().includes(q) ||
        (e.field.description ?? "").toLowerCase().includes(q),
    );
  }, [entries, search]);

  if (!meta) return null;

  const isDirty =
    draft !== null &&
    selectedEntry !== undefined &&
    JSON.stringify(normalizeField(draft)) !== JSON.stringify(normalizeField(selectedEntry.field));

  const patch = (p: Partial<DictionaryField>) => setDraft((d) => (d ? { ...d, ...p } : d));

  const save = async () => {
    if (!draft) return;
    setWorking(true);
    setNotice(null);
    const ok = await setField(normalizeField(draft));
    setWorking(false);
    if (ok) setNotice("Documentation saved.");
  };

  const clearEntry = async () => {
    if (!selectedId) return;
    setWorking(true);
    setNotice(null);
    const ok = await removeField(selectedId);
    setWorking(false);
    if (ok) setNotice("Documentation cleared.");
  };

  const runExport = async (format: DictionaryFormat) => {
    setNotice(null);
    await exportToFile(format);
  };

  const startImport = async () => {
    const path = await pickImportFile();
    if (!path) return;
    setImportPath(path);
    setImportSummary(null);
    await runPreview(path, matchBy);
  };

  const runPreview = async (path: string, mb: MergeMatchBy) => {
    const seq = ++previewSeq.current;
    setImportBusy(true);
    const p = await previewImport(path, mb);
    // Drop a stale result: a newer preview or a cancel superseded this one, so
    // the plan it produced no longer matches the current Match by selection.
    if (seq !== previewSeq.current) return;
    setImportBusy(false);
    setPlan(p);
  };

  const changeMatchBy = async (mb: MergeMatchBy) => {
    setMatchBy(mb);
    if (importPath) await runPreview(importPath, mb);
  };

  const cancelImport = () => {
    // Invalidate any in-flight preview so its late result cannot repopulate the
    // panel after it has been dismissed.
    previewSeq.current++;
    setImportBusy(false);
    setImportPath(null);
    setPlan(null);
    setConflictOpen(false);
  };

  const finishImport = async (resolution: MergeResolution) => {
    if (!importPath || !plan) return;
    setImportBusy(true);
    // Apply under exactly what the reviewed plan was computed with: its own
    // match mode and revision, NOT the live dialog state. The Match by selector
    // (or a stale preview) can have moved on since this plan was displayed, and
    // merging under a different mode would touch a different set of columns than
    // the conflicts/counts the user reviewed. The revision guard likewise
    // rejects a now-stale apply if documentation was edited after the preview.
    const outcome = await applyImport(
      importPath,
      applyMatchBy(plan),
      resolution,
      plan.dictionaryRevision,
    );
    setImportBusy(false);
    if (!outcome) return; // error surfaced globally; keep the panel open to retry
    setConflictOpen(false);
    setImportPath(null);
    setPlan(null);
    const bits = [
      `${outcome.newEntries} new`,
      `${outcome.fieldsAdded} field${outcome.fieldsAdded === 1 ? "" : "s"} added`,
      `${outcome.conflictsResolved} conflict${outcome.conflictsResolved === 1 ? "" : "s"} resolved`,
    ];
    if (outcome.unmatched.length > 0) bits.push(`${outcome.unmatched.length} unmatched`);
    setImportSummary(`Imported: ${bits.join(", ")}.`);
  };

  // No-conflict imports still need an explicit apply; keepAllExisting is a safe
  // resolution because there is nothing to resolve.
  const applyClean = () => void finishImport({ type: "keepAllExisting" });

  const draftComplete = draft ? completeness(draft) : null;

  return (
    <>
      <Modal
        title="Data dictionary"
        onClose={onClose}
        size="xl"
        footer={
          <>
            <span className="mr-auto text-xs text-zinc-400">
              Documentation is metadata — it never changes cell text or marks the document dirty.
            </span>
            <button onClick={onClose} className={btnGhost}>
              Close
            </button>
          </>
        }
      >
        <div className="space-y-3 text-sm">
          {/* toolbar */}
          <div className="flex flex-wrap items-center gap-2">
            <button onClick={() => void startImport()} disabled={importBusy} className={btnOutline}>
              Import…
            </button>
            <div className="flex items-center gap-1">
              <span className="text-xs text-zinc-400">Export</span>
              <button onClick={() => void runExport("json")} className={btnOutline}>
                JSON
              </button>
              <button onClick={() => void runExport("markdown")} className={btnOutline}>
                Markdown
              </button>
              <button onClick={() => void runExport("csv")} className={btnOutline}>
                CSV
              </button>
            </div>
            <span className="ml-auto text-xs text-zinc-400">
              {documentedCount(entries)} / {entries.length} column
              {entries.length === 1 ? "" : "s"} documented
            </span>
          </div>

          {notice && (
            <p className="rounded bg-emerald-50 px-2 py-1.5 text-xs text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-300">
              {notice}
            </p>
          )}

          {/* import panel */}
          {importPath && (
            <div className="space-y-2 rounded border border-violet-300 p-2.5 text-xs dark:border-violet-800">
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">Import dictionary</span>
                <span className="truncate text-zinc-400" title={importPath}>
                  {fileNameOf(importPath)}
                </span>
                <label className="ml-auto flex items-center gap-1">
                  Match by
                  <select
                    value={matchBy}
                    onChange={(e) => void changeMatchBy(e.target.value as MergeMatchBy)}
                    className={selectCls}
                  >
                    {MATCH_BY_OPTIONS.map((o) => (
                      <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                        {o.label}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
              {importBusy && <p className="text-zinc-500">Analyzing…</p>}
              {plan && !importBusy && (
                <>
                  <div className="flex flex-wrap gap-1.5">
                    <Pill cls="bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300">
                      {plan.matchedColumns} matched
                    </Pill>
                    <Pill cls="bg-sky-100 text-sky-700 dark:bg-sky-500/15 dark:text-sky-300">
                      {plan.newEntries.length} new
                    </Pill>
                    <Pill cls="bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">
                      {plan.cleanAdditions} clean additions
                    </Pill>
                    <Pill
                      cls={
                        plan.conflicts.length > 0
                          ? "bg-amber-100 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300"
                          : "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300"
                      }
                    >
                      {plan.conflicts.length} conflict{plan.conflicts.length === 1 ? "" : "s"}
                    </Pill>
                    {plan.unmatched.length > 0 && (
                      <Pill cls="bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
                        {plan.unmatched.length} unmatched
                      </Pill>
                    )}
                  </div>
                  {plan.unmatched.length > 0 && (
                    <p className="text-[11px] text-zinc-400">
                      No current column for: {plan.unmatched.join(", ")}
                    </p>
                  )}
                  <div className="flex items-center gap-2 pt-0.5">
                    {plan.conflicts.length > 0 ? (
                      <button onClick={() => setConflictOpen(true)} className={btnPrimary}>
                        Resolve {plan.conflicts.length} conflict
                        {plan.conflicts.length === 1 ? "" : "s"}…
                      </button>
                    ) : (
                      <button onClick={applyClean} disabled={importBusy} className={btnPrimary}>
                        Apply import
                      </button>
                    )}
                    <button onClick={cancelImport} className={btnGhost}>
                      Cancel
                    </button>
                  </div>
                </>
              )}
            </div>
          )}

          {importSummary && (
            <p className="rounded bg-emerald-50 px-2 py-1.5 text-xs text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-300">
              {importSummary}
            </p>
          )}

          <div className="flex gap-3">
            {/* ----- column list ----- */}
            <div className="flex w-56 shrink-0 flex-col">
              <input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="Search columns…"
                className={`${inputCls} mb-2`}
              />
              <div className="max-h-[46vh] overflow-y-auto rounded border border-zinc-200 dark:border-zinc-800">
                {filtered.map((e) => {
                  const c = completeness(e.field);
                  return (
                    <button
                      key={e.columnId}
                      onClick={() => setSelectedId(e.columnId)}
                      className={`flex w-full items-center gap-1.5 px-2 py-1.5 text-left text-xs ${
                        e.columnId === selectedId
                          ? "bg-violet-100 dark:bg-violet-500/20"
                          : "hover:bg-zinc-100 dark:hover:bg-zinc-800"
                      }`}
                    >
                      <span
                        className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                          e.documented ? "bg-violet-500" : "bg-zinc-300 dark:bg-zinc-600"
                        }`}
                        title={e.documented ? "Documented" : "Undocumented"}
                      />
                      <span className="min-w-0 flex-1 truncate" title={e.columnName}>
                        {e.field.displayName?.trim() || e.columnName}
                      </span>
                      {c.filled > 0 && (
                        <span className="shrink-0 text-[10px] text-zinc-400">
                          {c.filled}/{c.total}
                        </span>
                      )}
                    </button>
                  );
                })}
                {filtered.length === 0 && (
                  <p className="px-2 py-3 text-center text-xs text-zinc-400">No columns match.</p>
                )}
              </div>
            </div>

            {/* ----- editor ----- */}
            {draft && selectedEntry && (
              <div className="min-w-0 flex-1 space-y-2.5">
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <h3 className="truncate font-medium" title={selectedEntry.columnName}>
                      {selectedEntry.columnName}
                    </h3>
                    <p className="text-[11px] text-zinc-400">
                      ID {selectedEntry.columnId}
                      {selectedEntry.logicalType && (
                        <> · type {LOGICAL_TYPE_LABELS[selectedEntry.logicalType]}</>
                      )}
                    </p>
                  </div>
                  {draftComplete && (
                    <span className="shrink-0 rounded bg-zinc-100 px-1.5 py-0.5 text-[11px] text-zinc-500 dark:bg-zinc-800 dark:text-zinc-300">
                      {draftComplete.filled}/{draftComplete.total} fields
                    </span>
                  )}
                </div>

                <div className="grid grid-cols-2 gap-2.5">
                  <label className={fieldLabel}>
                    Display name
                    <input
                      value={draft.displayName ?? ""}
                      onChange={(e) => patch({ displayName: e.target.value })}
                      placeholder={selectedEntry.columnName}
                      className={inputCls}
                    />
                  </label>
                  <label className={fieldLabel}>
                    Role
                    <select
                      value={draft.role ?? ""}
                      onChange={(e) =>
                        patch({ role: e.target.value ? (e.target.value as FieldRole) : undefined })
                      }
                      className={selectCls}
                    >
                      <option value="" className="dark:bg-zinc-800">
                        — none —
                      </option>
                      {ROLE_OPTIONS.map((r) => (
                        <option key={r} value={r} className="dark:bg-zinc-800">
                          {ROLE_LABELS[r]}
                        </option>
                      ))}
                    </select>
                  </label>
                </div>

                <label className={fieldLabel}>
                  Description
                  <textarea
                    value={draft.description ?? ""}
                    onChange={(e) => patch({ description: e.target.value })}
                    rows={2}
                    placeholder="What does this column mean?"
                    className={`${inputCls} resize-y`}
                  />
                </label>

                <div className="grid grid-cols-2 gap-2.5">
                  <label className={fieldLabel}>
                    Unit
                    <input
                      value={draft.unit ?? ""}
                      onChange={(e) => patch({ unit: e.target.value })}
                      placeholder="USD, ms, kg…"
                      className={inputCls}
                    />
                  </label>
                  <label className={fieldLabel}>
                    Sensitivity
                    <select
                      value={draft.sensitivity ?? ""}
                      onChange={(e) =>
                        patch({
                          sensitivity: e.target.value ? (e.target.value as Sensitivity) : undefined,
                        })
                      }
                      className={selectCls}
                    >
                      <option value="" className="dark:bg-zinc-800">
                        — none —
                      </option>
                      {SENSITIVITY_OPTIONS.map((s) => (
                        <option key={s} value={s} className="dark:bg-zinc-800">
                          {SENSITIVITY_LABELS[s]}
                        </option>
                      ))}
                    </select>
                  </label>
                </div>

                {isSensitive(draft.sensitivity) && (
                  <p className="rounded bg-amber-50 px-2 py-1 text-[11px] text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
                    Confidential and restricted columns are flagged by the personal-data preflight
                    even without a pattern hit.
                  </p>
                )}

                <div className="grid grid-cols-2 gap-2.5">
                  <label className={fieldLabel}>
                    Source
                    <input
                      value={draft.source ?? ""}
                      onChange={(e) => patch({ source: e.target.value })}
                      placeholder="System of record…"
                      className={inputCls}
                    />
                  </label>
                  <label className={fieldLabel}>
                    Owner
                    <input
                      value={draft.owner ?? ""}
                      onChange={(e) => patch({ owner: e.target.value })}
                      placeholder="Steward / team…"
                      className={inputCls}
                    />
                  </label>
                </div>

                <label className={fieldLabel}>
                  Example value
                  <input
                    value={draft.example ?? ""}
                    onChange={(e) => patch({ example: e.target.value })}
                    className={inputCls}
                  />
                </label>

                {/* allowed values chips */}
                <div>
                  <span className={fieldLabelText}>Allowed values</span>
                  <div className="mt-1 flex flex-wrap items-center gap-1">
                    {(draft.allowedValues ?? []).map((val, i) => (
                      <span key={i} className={chip}>
                        {val}
                        <button
                          onClick={() =>
                            patch({
                              allowedValues: (draft.allowedValues ?? []).filter((_, j) => j !== i),
                            })
                          }
                          className="ml-1 text-zinc-400 hover:text-red-500"
                          aria-label="Remove value"
                        >
                          ×
                        </button>
                      </span>
                    ))}
                    <input
                      value={allowedInput}
                      onChange={(e) => setAllowedInput(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          const v = allowedInput.trim();
                          if (v && !(draft.allowedValues ?? []).includes(v)) {
                            patch({ allowedValues: [...(draft.allowedValues ?? []), v] });
                          }
                          setAllowedInput("");
                        }
                      }}
                      placeholder="add value, Enter"
                      className={`${inputCls} w-36`}
                    />
                  </div>
                </div>

                <label className={fieldLabel}>
                  Notes
                  <textarea
                    value={draft.notes ?? ""}
                    onChange={(e) => patch({ notes: e.target.value })}
                    rows={2}
                    className={`${inputCls} resize-y`}
                  />
                </label>

                <div className="flex items-center gap-2 pt-1">
                  <button
                    onClick={() => void save()}
                    disabled={working || !isDirty}
                    className={btnPrimary}
                  >
                    Save
                  </button>
                  {isDocumented(selectedEntry.field) && (
                    <button
                      onClick={() => void clearEntry()}
                      disabled={working}
                      className={btnDanger}
                    >
                      Clear
                    </button>
                  )}
                  {isDirty && <span className="text-xs text-amber-500">Unsaved changes</span>}
                </div>
              </div>
            )}
          </div>

          {/* ----- orphans ----- */}
          {orphans.length > 0 && (
            <div className="rounded border border-amber-300 p-2.5 text-xs dark:border-amber-800/70">
              <div className="flex items-center gap-2">
                <span className="font-medium text-amber-700 dark:text-amber-300">
                  {orphans.length} orphaned entr{orphans.length === 1 ? "y" : "ies"}
                </span>
                <span className="text-zinc-400">
                  documentation whose column was deleted (kept in case it returns)
                </span>
                <button onClick={() => void discardOrphans()} className={`${btnDanger} ml-auto`}>
                  Discard all
                </button>
              </div>
              <ul className="mt-1.5 space-y-1">
                {orphans.map((o) => (
                  <li key={o.columnId} className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 truncate" title={o.columnId}>
                      {o.label}
                    </span>
                    <button
                      onClick={() => void removeField(o.columnId)}
                      className="text-zinc-400 hover:text-red-500"
                    >
                      Remove
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      </Modal>

      {conflictOpen && plan && plan.conflicts.length > 0 && (
        <ConflictDialog
          plan={plan}
          busy={importBusy}
          onCancel={() => setConflictOpen(false)}
          onApply={(resolution) => void finishImport(resolution)}
        />
      )}
    </>
  );
}

// ---------------------------------------------------------------------------
// Conflict resolution dialog
// ---------------------------------------------------------------------------

/**
 * Field-level conflict resolution for a dictionary import (F38). Each reported
 * conflict shows the existing vs incoming value and must be assigned a side;
 * "keep all" / "take all" set every choice at once. The apply button stays
 * disabled until every conflict is resolved — the backend also rejects a gap,
 * so a conflict can never be silently dropped.
 */
function ConflictDialog({
  plan,
  busy,
  onCancel,
  onApply,
}: {
  plan: MergePlan;
  busy: boolean;
  onCancel: () => void;
  onApply: (resolution: MergeResolution) => void;
}) {
  const [choices, setChoices] = useState<ConflictChoices>({});
  const remaining = unresolvedCount(plan.conflicts, choices);
  const resolved = allConflictsResolved(plan.conflicts, choices);

  const apply = () => onApply(buildPerFieldResolution(plan.conflicts, choices));

  return (
    <Modal
      title={`Resolve ${plan.conflicts.length} documentation conflict${
        plan.conflicts.length === 1 ? "" : "s"
      }`}
      onClose={onCancel}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-xs text-zinc-400">
            {remaining > 0 ? `${remaining} still unresolved` : "All conflicts resolved"}
          </span>
          <button onClick={onCancel} className={btnGhost}>
            Cancel
          </button>
          <button onClick={apply} disabled={!resolved || busy} className={btnPrimary}>
            Apply import
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex items-center gap-2 text-xs">
          <span className="text-zinc-400">Set all:</span>
          <button
            onClick={() => setChoices(bulkChoices(plan.conflicts, "keepExisting"))}
            className={btnOutline}
          >
            Keep existing
          </button>
          <button
            onClick={() => setChoices(bulkChoices(plan.conflicts, "takeIncoming"))}
            className={btnOutline}
          >
            Take incoming
          </button>
        </div>

        <div className="max-h-[52vh] space-y-2 overflow-y-auto pr-1">
          {plan.conflicts.map((c) => {
            const key = conflictKey(c.columnId, c.field);
            const choice = choices[key];
            const pick = (side: "keepExisting" | "takeIncoming") =>
              setChoices((prev) => ({ ...prev, [key]: side }));
            return (
              <div
                key={key}
                className="rounded border border-zinc-200 p-2 text-xs dark:border-zinc-800"
              >
                <div className="mb-1.5 flex items-center gap-1.5">
                  <span className="font-medium">{c.columnName}</span>
                  <span className="text-zinc-400">· {FIELD_KEY_LABELS[c.field]}</span>
                  {!choice && (
                    <span className="ml-auto rounded bg-amber-100 px-1.5 py-0.5 text-[10px] text-amber-800 dark:bg-amber-500/15 dark:text-amber-300">
                      Choose
                    </span>
                  )}
                </div>
                <div className="grid grid-cols-2 gap-2">
                  <ChoiceCard
                    label="Keep existing"
                    value={c.existing}
                    selected={choice === "keepExisting"}
                    onClick={() => pick("keepExisting")}
                  />
                  <ChoiceCard
                    label="Take incoming"
                    value={c.incoming}
                    selected={choice === "takeIncoming"}
                    onClick={() => pick("takeIncoming")}
                  />
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </Modal>
  );
}

function ChoiceCard({
  label,
  value,
  selected,
  onClick,
}: {
  label: string;
  value: string;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`rounded border px-2 py-1.5 text-left ${
        selected
          ? "border-violet-500 bg-violet-50/60 dark:bg-violet-500/10"
          : "border-zinc-200 hover:border-violet-400 dark:border-zinc-800"
      }`}
    >
      <div className="mb-0.5 flex items-center gap-1 text-[10px] uppercase tracking-wide text-zinc-400">
        <span
          className={`h-2 w-2 rounded-full border ${
            selected ? "border-violet-500 bg-violet-500" : "border-zinc-400"
          }`}
        />
        {label}
      </div>
      <div className="break-words text-zinc-700 dark:text-zinc-200">
        {value || <span className="text-zinc-400">(blank)</span>}
      </div>
    </button>
  );
}

// ---------------------------------------------------------------------------

function Pill({ children, cls }: { children: React.ReactNode; cls: string }) {
  return <span className={`rounded px-1.5 py-0.5 text-[11px] ${cls}`}>{children}</span>;
}

function documentedCount(entries: DictionaryEntryView[]): number {
  return entries.filter((e) => e.documented).length;
}

function fileNameOf(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary =
  "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40";
const btnOutline =
  "rounded border border-zinc-200 px-2.5 py-1 text-xs hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
const btnDanger =
  "rounded border border-red-200 px-2 py-1 text-xs text-red-600 hover:bg-red-50 disabled:opacity-40 dark:border-red-900/60 dark:text-red-400 dark:hover:bg-red-500/10";
const chip =
  "inline-flex items-center rounded bg-zinc-100 px-1.5 py-0.5 text-[11px] text-zinc-700 dark:bg-zinc-800 dark:text-zinc-200";
const inputCls =
  "w-full rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const fieldLabelText = "text-xs font-medium text-zinc-500 dark:text-zinc-400";
const fieldLabel = `flex flex-col gap-1 ${fieldLabelText}`;
