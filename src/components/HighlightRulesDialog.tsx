import { useEffect, useMemo, useState } from "react";

import {
  CONDITION_GROUPS,
  CONDITION_LABELS,
  EMPHASIS_LABELS,
  TEXT_STYLE_LABELS,
  TONE_LABELS,
  TONE_ORDER,
  conditionAnalysisBacked,
  conditionColumnId,
  conditionReserved,
  conditionSupportsColumn,
  defaultCondition,
  describeCondition,
  describeTarget,
  highlightBackground,
  newHighlightRule,
  orderRulesByPriority,
  validateHighlightRule,
  withDecoration,
  type ConditionKind,
} from "../lib/highlight";
import { useActiveMeta, useStore } from "../store/useStore";
import type {
  ExportScope,
  HighlightCondition,
  HighlightEmphasis,
  HighlightReportFormat,
  HighlightRule,
  HighlightTextStyle,
} from "../types";
import { Modal } from "./Modal";

const EMPHASES: HighlightEmphasis[] = ["subtle", "normal", "strong"];
const TEXT_STYLES: HighlightTextStyle[] = ["normal", "bold", "italic"];

const isDark = () => document.documentElement.classList.contains("dark");

/**
 * Conditional highlighting (F42): a two-pane editor for the active document's
 * view-only decoration rules. The left pane lists rules in winning (priority)
 * order with an enable toggle, up/down reordering and a live match count; the
 * right pane edits the selected rule's condition, target and semantic
 * decoration with inline validation. Rules never touch data — editing one is
 * never an undoable operation and never dirties the document. Save a named
 * view (F12) or profile (F08) to persist them for a file.
 */
export function HighlightRulesDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const rules = useStore((s) => s.highlight.rules);
  const counts = useStore((s) => s.highlight.counts);
  const reportError = useStore((s) => s.highlight.reportError);
  const upsertRule = useStore((s) => s.upsertHighlightRule);
  const deleteRule = useStore((s) => s.deleteHighlightRule);
  const setRules = useStore((s) => s.setHighlightRules);
  const clearRules = useStore((s) => s.clearHighlightRules);
  const exportReport = useStore((s) => s.exportHighlightReport);

  const [selectedId, setSelectedId] = useState<string | null>(rules[0]?.id ?? null);
  const [draft, setDraft] = useState<HighlightRule | null>(
    () => rules.find((r) => r.id === (rules[0]?.id ?? null)) ?? null,
  );
  const [reportFormat, setReportFormat] = useState<HighlightReportFormat>("csv");
  const [reportVisibleOnly, setReportVisibleOnly] = useState(false);
  const dark = isDark();

  const columns = useMemo(
    () =>
      meta
        ? meta.columnIds.map((id, i) => ({ id, name: meta.headers[i] || `Column ${i + 1}` }))
        : [],
    [meta],
  );
  const nameFor = useMemo(() => {
    const map = new Map(columns.map((c) => [c.id, c.name]));
    return (id: string) => map.get(id) ?? id;
  }, [columns]);

  const ordered = useMemo(() => orderRulesByPriority(rules), [rules]);
  const draftError = draft ? validateHighlightRule(draft) : null;

  // Persist a valid, changed draft after a short debounce → live grid preview.
  // Invalid drafts show their error inline and are never stored.
  useEffect(() => {
    if (!draft || validateHighlightRule(draft)) return;
    const stored = useStore.getState().highlight.rules.find((r) => r.id === draft.id);
    if (stored && JSON.stringify(stored) === JSON.stringify(draft)) return;
    const t = setTimeout(() => void useStore.getState().upsertHighlightRule(draft), 350);
    return () => clearTimeout(t);
  }, [draft]);

  if (!meta) return null;

  const select = (id: string) => {
    setSelectedId(id);
    setDraft(rules.find((r) => r.id === id) ?? null);
  };

  const addRule = () => {
    const rule = newHighlightRule(rules);
    setSelectedId(rule.id);
    setDraft(rule);
    void upsertRule(rule);
  };

  const duplicate = (rule: HighlightRule) => {
    const copy = { ...newHighlightRule(rules), name: rule.name ? `${rule.name} copy` : "" };
    copy.condition = { ...rule.condition };
    copy.target =
      rule.target.type === "columns"
        ? { type: "columns", columnIds: [...rule.target.columnIds] }
        : { ...rule.target };
    copy.decoration = { ...rule.decoration };
    setSelectedId(copy.id);
    setDraft(copy);
    void upsertRule(copy);
  };

  const remove = (id: string) => {
    void deleteRule(id);
    if (selectedId === id) {
      const next = ordered.find((r) => r.id !== id);
      setSelectedId(next?.id ?? null);
      setDraft(next ? (rules.find((r) => r.id === next.id) ?? null) : null);
    }
  };

  const patchDraft = (patch: Partial<HighlightRule>) =>
    setDraft((d) => (d ? { ...d, ...patch } : d));

  // Reorder by swapping the two rules' priorities. Uses the in-progress draft
  // for the selected rule so an unsaved edit is preserved, and keeps the draft
  // in sync so the debounced persist doesn't undo the move.
  const reorder = (ruleId: string, direction: "up" | "down") => {
    const effective = rules.map((r) => (draft && r.id === draft.id ? draft : r));
    const ord = orderRulesByPriority(effective);
    const at = ord.findIndex((r) => r.id === ruleId);
    const swapWith = direction === "up" ? at - 1 : at + 1;
    if (at < 0 || swapWith < 0 || swapWith >= ord.length) return;
    const a = ord[at];
    const b = ord[swapWith];
    let pa = b.priority;
    let pb = a.priority;
    if (pa === pb) {
      if (direction === "up") pa = pb + 1;
      else pb = pa + 1;
    }
    const next = effective.map((r) =>
      r.id === a.id ? { ...r, priority: pa } : r.id === b.id ? { ...r, priority: pb } : r,
    );
    if (draft) {
      const updated = next.find((r) => r.id === draft.id);
      if (updated) setDraft(updated);
    }
    void setRules(next);
  };

  const doExport = () => {
    const scope: ExportScope = reportVisibleOnly ? { type: "visibleRows" } : { type: "all" };
    void exportReport(reportFormat, scope);
  };

  return (
    <Modal
      title="Conditional highlighting"
      onClose={onClose}
      size="2xl"
      footer={
        <>
          <div className="mr-auto flex items-center gap-2 text-xs text-zinc-500 dark:text-zinc-400">
            <span>Match report:</span>
            <select
              value={reportFormat}
              onChange={(e) => setReportFormat(e.target.value as HighlightReportFormat)}
              className="rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-xs dark:border-zinc-700"
            >
              <option value="csv">CSV</option>
              <option value="json">JSON</option>
            </select>
            <label className="flex items-center gap-1">
              <input
                type="checkbox"
                checked={reportVisibleOnly}
                onChange={(e) => setReportVisibleOnly(e.target.checked)}
              />
              Visible rows only
            </label>
            <button
              onClick={doExport}
              disabled={rules.length === 0}
              className="rounded border border-zinc-300 px-2 py-1 text-xs hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-800"
            >
              Export…
            </button>
          </div>
          <button
            onClick={onClose}
            className="rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            Close
          </button>
        </>
      }
    >
      {reportError && (
        <div className="mb-2 rounded border border-red-300 bg-red-50 px-3 py-1.5 text-xs text-red-700 dark:border-red-500/40 dark:bg-red-500/10 dark:text-red-300">
          {reportError}
        </div>
      )}
      <div className="flex gap-4">
        {/* ----- rule list ----- */}
        <div className="flex w-64 shrink-0 flex-col">
          <div className="mb-2 flex items-center gap-2">
            <button
              onClick={addRule}
              className="rounded bg-violet-600 px-2.5 py-1 text-xs font-medium text-white hover:bg-violet-500"
            >
              Add rule
            </button>
            {rules.length > 0 && (
              <button
                onClick={() => void clearRules()}
                className="rounded px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Clear all
              </button>
            )}
          </div>
          {ordered.length === 0 ? (
            <p className="rounded border border-dashed border-zinc-300 px-3 py-6 text-center text-xs text-zinc-500 dark:border-zinc-700 dark:text-zinc-400">
              No highlight rules yet. Add one to flag cells or rows — decoration only, your data is
              never changed.
            </p>
          ) : (
            <ul className="max-h-96 space-y-1 overflow-y-auto pr-1">
              {ordered.map((rule, idx) => (
                <li key={rule.id}>
                  <div
                    className={`rounded border px-2 py-1.5 ${
                      rule.id === selectedId
                        ? "border-violet-400 bg-violet-50/60 dark:border-violet-500/50 dark:bg-violet-500/10"
                        : "border-zinc-200 dark:border-zinc-700/60"
                    }`}
                  >
                    <div className="flex items-center gap-1.5">
                      <input
                        type="checkbox"
                        checked={rule.enabled}
                        title={rule.enabled ? "Enabled" : "Disabled"}
                        onChange={(e) => {
                          // Toggling the rule being edited goes through the
                          // draft so an unsaved edit in the right pane is not
                          // discarded; other rules update directly.
                          if (rule.id === selectedId && draft) {
                            patchDraft({ enabled: e.target.checked });
                          } else {
                            void upsertRule({ ...rule, enabled: e.target.checked });
                          }
                        }}
                      />
                      <span
                        className="h-3 w-3 shrink-0 rounded-sm border border-black/10 dark:border-white/10"
                        style={{
                          backgroundColor: highlightBackground(
                            rule.decoration.tone,
                            rule.decoration.emphasis,
                            dark,
                          ),
                        }}
                      />
                      <button
                        onClick={() => select(rule.id)}
                        className={`min-w-0 flex-1 truncate text-left text-sm ${
                          rule.enabled ? "" : "text-zinc-400 line-through dark:text-zinc-500"
                        }`}
                      >
                        {rule.decoration.icon ? `${rule.decoration.icon} ` : ""}
                        {rule.name || "(unnamed rule)"}
                      </button>
                      <div className="flex shrink-0 flex-col">
                        <button
                          disabled={idx === 0}
                          onClick={() => reorder(rule.id, "up")}
                          className="px-1 text-[10px] leading-none text-zinc-500 hover:text-zinc-800 disabled:opacity-30 dark:hover:text-zinc-200"
                          title="Higher priority"
                        >
                          ▲
                        </button>
                        <button
                          disabled={idx === ordered.length - 1}
                          onClick={() => reorder(rule.id, "down")}
                          className="px-1 text-[10px] leading-none text-zinc-500 hover:text-zinc-800 disabled:opacity-30 dark:hover:text-zinc-200"
                          title="Lower priority"
                        >
                          ▼
                        </button>
                      </div>
                    </div>
                    <div className="mt-0.5 flex items-center justify-between gap-2 pl-6 text-[11px] text-zinc-500 dark:text-zinc-400">
                      <span className="truncate">{describeCondition(rule.condition, nameFor)}</span>
                      <span className="shrink-0 tabular-nums">
                        {counts ? (counts[rule.id] ?? 0) : "…"}
                      </span>
                    </div>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>

        {/* ----- editor ----- */}
        <div className="min-w-0 flex-1 border-l border-zinc-200 pl-4 dark:border-zinc-700/60">
          {draft ? (
            <RuleEditor
              draft={draft}
              columns={columns}
              dark={dark}
              error={draftError}
              matchCount={counts?.[draft.id]}
              onPatch={patchDraft}
              onPatchDecoration={(p) =>
                patchDraft({ decoration: withDecoration(draft.decoration, p) })
              }
              onDelete={() => remove(draft.id)}
              onDuplicate={() => duplicate(draft)}
            />
          ) : (
            <p className="py-10 text-center text-sm text-zinc-500 dark:text-zinc-400">
              Select a rule to edit, or add a new one.
            </p>
          )}
        </div>
      </div>
    </Modal>
  );
}

// ----- rule editor -----------------------------------------------------------

interface Column {
  id: string;
  name: string;
}

function RuleEditor({
  draft,
  columns,
  dark,
  error,
  matchCount,
  onPatch,
  onPatchDecoration,
  onDelete,
  onDuplicate,
}: {
  draft: HighlightRule;
  columns: Column[];
  dark: boolean;
  error: string | null;
  matchCount: number | undefined;
  onPatch: (patch: Partial<HighlightRule>) => void;
  onPatchDecoration: (patch: Partial<HighlightRule["decoration"]>) => void;
  onDelete: () => void;
  onDuplicate: () => void;
}) {
  const kind = draft.condition.type;
  const reserved = conditionReserved(kind);

  const changeKind = (next: ConditionKind) => {
    // Preserve the column scope when both kinds carry one.
    const prevCol = conditionColumnId(draft.condition);
    const cond = defaultCondition(next);
    if (prevCol && conditionSupportsColumn(next) && "columnId" in cond) cond.columnId = prevCol;
    onPatch({ condition: cond });
  };

  const setCondition = (condition: HighlightCondition) => onPatch({ condition });
  // The column scope is only shown for column-scoped kinds, so the cast is
  // safe: every such condition variant carries `columnId`.
  const setColumn = (columnId: string | null) =>
    setCondition({ ...draft.condition, columnId } as HighlightCondition);

  const label = "block text-xs font-medium text-zinc-600 dark:text-zinc-300";
  const input =
    "w-full rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <input
          value={draft.name}
          placeholder="Rule name"
          onChange={(e) => onPatch({ name: e.target.value })}
          className="flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm font-medium outline-none focus:border-violet-500 dark:border-zinc-700"
        />
        <button
          onClick={onDuplicate}
          className="rounded px-2 py-1 text-xs text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          Duplicate
        </button>
        <button
          onClick={onDelete}
          className="rounded px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
        >
          Delete
        </button>
      </div>

      {/* condition type */}
      <div>
        <label className={label}>Condition</label>
        <select
          value={kind}
          onChange={(e) => changeKind(e.target.value as ConditionKind)}
          className={`${input} mt-1`}
        >
          {CONDITION_GROUPS.map((group) => (
            <optgroup key={group.label} label={group.label}>
              {group.kinds.map((k) => (
                <option key={k} value={k}>
                  {CONDITION_LABELS[k]}
                </option>
              ))}
            </optgroup>
          ))}
        </select>
      </div>

      {reserved && (
        <p className="rounded border border-amber-300 bg-amber-50 px-3 py-1.5 text-xs text-amber-800 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-200">
          Row annotations (bookmarks / flags / tags) aren’t available yet — this rule is saved but
          matches nothing until that feature ships.
        </p>
      )}
      {conditionAnalysisBacked(kind) && (
        <p className="rounded border border-sky-300 bg-sky-50 px-3 py-1.5 text-xs text-sky-800 dark:border-sky-500/40 dark:bg-sky-500/10 dark:text-sky-200">
          Reads the document’s last matching scan — run it (diagnostics / cross-column / outliers)
          to populate matches; empty until then.
        </p>
      )}

      {/* column scope */}
      {conditionSupportsColumn(kind) && (
        <div>
          <label className={label}>Column</label>
          <select
            value={conditionColumnId(draft.condition) ?? ""}
            onChange={(e) => setColumn(e.target.value || null)}
            className={`${input} mt-1`}
          >
            <option value="">Any column</option>
            {columns.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </div>
      )}

      <ConditionFields
        condition={draft.condition}
        onChange={setCondition}
        inputClass={input}
        labelClass={label}
      />

      {/* target */}
      <div>
        <label className={label}>Apply to</label>
        <div className="mt-1 flex gap-1">
          {(["cell", "row", "columns"] as const).map((t) => (
            <button
              key={t}
              onClick={() =>
                onPatch({
                  target: t === "columns" ? { type: "columns", columnIds: [] } : { type: t },
                })
              }
              className={`rounded px-2.5 py-1 text-xs ${
                draft.target.type === t
                  ? "bg-violet-600 text-white"
                  : "border border-zinc-300 text-zinc-600 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
              }`}
            >
              {t === "cell" ? "Matched cell" : t === "row" ? "Whole row" : "Columns"}
            </button>
          ))}
        </div>
        {draft.target.type === "columns" && (
          <div className="mt-2 max-h-28 space-y-0.5 overflow-y-auto rounded border border-zinc-200 p-1.5 dark:border-zinc-700/60">
            {columns.map((c) => {
              const checked =
                draft.target.type === "columns" && draft.target.columnIds.includes(c.id);
              return (
                <label key={c.id} className="flex items-center gap-1.5 text-xs">
                  <input
                    type="checkbox"
                    checked={checked}
                    onChange={(e) => {
                      if (draft.target.type !== "columns") return;
                      const ids = e.target.checked
                        ? [...draft.target.columnIds, c.id]
                        : draft.target.columnIds.filter((x) => x !== c.id);
                      onPatch({ target: { type: "columns", columnIds: ids } });
                    }}
                  />
                  {c.name}
                </label>
              );
            })}
          </div>
        )}
      </div>

      {/* decoration */}
      <div className="rounded border border-zinc-200 p-2.5 dark:border-zinc-700/60">
        <div className="mb-1.5 flex items-center justify-between">
          <span className={label}>Decoration</span>
          <DecorationPreview decoration={draft.decoration} dark={dark} />
        </div>
        <div className="grid grid-cols-2 gap-2">
          <div>
            <label className="text-[11px] text-zinc-500 dark:text-zinc-400">Tone</label>
            <div className="mt-1 flex flex-wrap gap-1">
              {TONE_ORDER.map((tone) => (
                <button
                  key={tone}
                  title={TONE_LABELS[tone]}
                  onClick={() => onPatchDecoration({ tone })}
                  className={`h-6 w-6 rounded ${
                    draft.decoration.tone === tone
                      ? "ring-2 ring-violet-500 ring-offset-1 dark:ring-offset-zinc-900"
                      : ""
                  }`}
                  style={{ backgroundColor: highlightBackground(tone, "strong", dark) }}
                />
              ))}
            </div>
          </div>
          <div>
            <label className="text-[11px] text-zinc-500 dark:text-zinc-400">Emphasis</label>
            <select
              value={draft.decoration.emphasis}
              onChange={(e) => onPatchDecoration({ emphasis: e.target.value as HighlightEmphasis })}
              className={`${input} mt-1`}
            >
              {EMPHASES.map((e) => (
                <option key={e} value={e}>
                  {EMPHASIS_LABELS[e]}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="text-[11px] text-zinc-500 dark:text-zinc-400">Text style</label>
            <select
              value={draft.decoration.textStyle}
              onChange={(e) =>
                onPatchDecoration({ textStyle: e.target.value as HighlightTextStyle })
              }
              className={`${input} mt-1`}
            >
              {TEXT_STYLES.map((t) => (
                <option key={t} value={t}>
                  {TEXT_STYLE_LABELS[t]}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="text-[11px] text-zinc-500 dark:text-zinc-400">Icon (optional)</label>
            <input
              value={draft.decoration.icon ?? ""}
              placeholder="e.g. ⚑"
              maxLength={2}
              onChange={(e) => onPatchDecoration({ icon: e.target.value || null })}
              className={`${input} mt-1`}
            />
          </div>
        </div>
      </div>

      {/* priority + status */}
      <div className="flex items-center gap-3">
        <label className="flex items-center gap-1.5 text-xs text-zinc-600 dark:text-zinc-300">
          Priority
          <input
            type="number"
            value={draft.priority}
            onChange={(e) => onPatch({ priority: Number(e.target.value) || 0 })}
            className="w-16 rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-sm dark:border-zinc-700"
          />
        </label>
        <span className="text-xs text-zinc-500 dark:text-zinc-400">
          {describeTarget(draft.target)} · {matchCount ?? "…"} matches
        </span>
      </div>

      {error && (
        <p className="rounded border border-red-300 bg-red-50 px-3 py-1.5 text-xs text-red-700 dark:border-red-500/40 dark:bg-red-500/10 dark:text-red-300">
          {error}
        </p>
      )}
    </div>
  );
}

/** The condition-specific inputs for the selected condition kind. Each case
 *  narrows the discriminated union, so it emits a complete replacement
 *  condition (avoiding the `Partial<union>` common-keys-only pitfall). */
function ConditionFields({
  condition,
  onChange,
  inputClass,
  labelClass,
}: {
  condition: HighlightCondition;
  onChange: (condition: HighlightCondition) => void;
  inputClass: string;
  labelClass: string;
}) {
  const checkbox = (
    key: string,
    label: string,
    checked: boolean | undefined,
    onToggle: (v: boolean) => void,
  ) => (
    <label key={key} className="flex items-center gap-1.5 text-xs text-zinc-600 dark:text-zinc-300">
      <input type="checkbox" checked={!!checked} onChange={(e) => onToggle(e.target.checked)} />
      {label}
    </label>
  );
  const num = (raw: string): number | null => (raw === "" ? null : Number(raw));

  switch (condition.type) {
    case "equals":
    case "notEquals":
    case "contains":
      return (
        <div className="space-y-2">
          <div>
            <label className={labelClass}>Value</label>
            <input
              value={condition.value}
              onChange={(e) => onChange({ ...condition, value: e.target.value })}
              className={`${inputClass} mt-1`}
            />
          </div>
          {checkbox("cs", "Case sensitive", condition.caseSensitive, (v) =>
            onChange({ ...condition, caseSensitive: v }),
          )}
        </div>
      );
    case "regex":
      return (
        <div className="space-y-2">
          <div>
            <label className={labelClass}>Pattern</label>
            <input
              value={condition.pattern}
              placeholder="e.g. ^\d{4}-\d{2}-\d{2}$"
              onChange={(e) => onChange({ ...condition, pattern: e.target.value })}
              className={`${inputClass} mt-1 font-mono`}
            />
          </div>
          {checkbox("cs", "Case sensitive", condition.caseSensitive, (v) =>
            onChange({ ...condition, caseSensitive: v }),
          )}
        </div>
      );
    case "numericRange":
      return (
        <div className="space-y-2">
          <div className="grid grid-cols-2 gap-2">
            <div>
              <label className={labelClass}>Min</label>
              <input
                type="number"
                value={condition.min ?? ""}
                onChange={(e) => onChange({ ...condition, min: num(e.target.value) })}
                className={`${inputClass} mt-1`}
              />
            </div>
            <div>
              <label className={labelClass}>Max</label>
              <input
                type="number"
                value={condition.max ?? ""}
                onChange={(e) => onChange({ ...condition, max: num(e.target.value) })}
                className={`${inputClass} mt-1`}
              />
            </div>
          </div>
          {checkbox("inc", "Inclusive bounds", condition.inclusive, (v) =>
            onChange({ ...condition, inclusive: v }),
          )}
        </div>
      );
    case "dateRange":
      return (
        <div className="grid grid-cols-2 gap-2">
          <div>
            <label className={labelClass}>Earliest</label>
            <input
              value={condition.min ?? ""}
              placeholder="2024-01-01"
              onChange={(e) => onChange({ ...condition, min: e.target.value || null })}
              className={`${inputClass} mt-1`}
            />
          </div>
          <div>
            <label className={labelClass}>Latest</label>
            <input
              value={condition.max ?? ""}
              placeholder="2024-12-31"
              onChange={(e) => onChange({ ...condition, max: e.target.value || null })}
              className={`${inputClass} mt-1`}
            />
          </div>
        </div>
      );
    case "duplicate":
      return (
        <div className="space-y-1.5">
          {checkbox("trim", "Trim whitespace", condition.trim, (v) =>
            onChange({ ...condition, trim: v }),
          )}
          {checkbox("ci", "Case insensitive", condition.caseInsensitive, (v) =>
            onChange({ ...condition, caseInsensitive: v }),
          )}
          {checkbox("cw", "Collapse inner whitespace", condition.collapseWhitespace, (v) =>
            onChange({ ...condition, collapseWhitespace: v }),
          )}
        </div>
      );
    case "diagnostic":
      return (
        <div>
          <label className={labelClass}>Issue id (optional)</label>
          <input
            value={condition.issueId ?? ""}
            placeholder="Any row-filterable issue"
            onChange={(e) => onChange({ ...condition, issueId: e.target.value || null })}
            className={`${inputClass} mt-1`}
          />
        </div>
      );
    case "crossColumn":
      return (
        <div>
          <label className={labelClass}>Rule number (optional, 1-based)</label>
          <input
            type="number"
            min={1}
            value={condition.ruleIndex != null ? condition.ruleIndex + 1 : ""}
            placeholder="Any rule"
            onChange={(e) => {
              const n = num(e.target.value);
              onChange({ ...condition, ruleIndex: n == null ? null : n - 1 });
            }}
            className={`${inputClass} mt-1`}
          />
        </div>
      );
    case "flagged":
      return (
        <div>
          <label className={labelClass}>Flag label (optional)</label>
          <input
            value={condition.label ?? ""}
            onChange={(e) => onChange({ ...condition, label: e.target.value || null })}
            className={`${inputClass} mt-1`}
          />
        </div>
      );
    case "tagged":
      return (
        <div>
          <label className={labelClass}>Tag</label>
          <input
            value={condition.tag}
            onChange={(e) => onChange({ ...condition, tag: e.target.value })}
            className={`${inputClass} mt-1`}
          />
        </div>
      );
    default:
      // blank / invalid / changedSinceSave / outlier / bookmarked carry no extra
      // fields beyond the (already-rendered) column scope.
      return null;
  }
}

/** A small "Aa" chip showing exactly how a decoration will read on the grid. */
function DecorationPreview({
  decoration,
  dark,
}: {
  decoration: HighlightRule["decoration"];
  dark: boolean;
}) {
  const weight =
    decoration.textStyle === "bold" ? 600 : decoration.textStyle === "italic" ? 400 : 400;
  return (
    <span
      className="inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs text-zinc-800 dark:text-zinc-100"
      style={{
        backgroundColor: highlightBackground(decoration.tone, decoration.emphasis, dark),
        fontWeight: weight,
        fontStyle: decoration.textStyle === "italic" ? "italic" : "normal",
      }}
    >
      {decoration.icon ? `${decoration.icon} ` : ""}Aa
    </span>
  );
}
