import { save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { useEffect, useState } from "react";

import {
  COMPARE_OP_LABELS,
  RULE_TYPE_LABELS,
  describeRule,
  emptyRule,
  parseCombinations,
  ruleProblem,
} from "../lib/crossval";
import { matchingProfiles } from "../lib/profiles";
import { useActiveMeta, useStore } from "../store/useStore";
import type { CompareOp, CrossRule, WhenCondition } from "../types";
import { Modal } from "./Modal";

/**
 * Cross-column validation (F27): relational rules between columns — a
 * closed, validated set (no expressions). Rules can be pulled from and saved
 * to a matching file profile; scanning is read-only and revision-guarded,
 * and violations support jump-to-row, filter, and JSON export.
 */
export function CrossValDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const crossval = useStore((s) => s.crossval);
  const settings = useStore((s) => s.settings);
  const startScan = useStore((s) => s.startCrossvalScan);
  const cancelScan = useStore((s) => s.cancelCrossvalScan);
  const loadCached = useStore((s) => s.loadCachedCrossvalReport);
  const applyFilter = useStore((s) => s.applyCrossvalFilter);
  const jumpToCell = useStore((s) => s.jumpToCell);
  const saveProfiles = useStore((s) => s.saveProfiles);

  const profile = meta?.path ? matchingProfiles(settings?.profiles ?? [], meta.path)[0] : undefined;

  const [rules, setRules] = useState<CrossRule[]>(() => profile?.crossRules ?? []);
  const [draftType, setDraftType] = useState<CrossRule["type"]>("columnsEqual");
  const [draft, setDraft] = useState<CrossRule | null>(null);
  const [combosText, setCombosText] = useState("");
  const [working, setWorking] = useState(false);

  useEffect(() => {
    void loadCached();
  }, [loadCached]);

  const { report, scanJobId, processed, total, error } = crossval;
  const scanning = scanJobId != null;

  if (!meta) return null;
  const stale = report !== null && report.revision !== meta.revision;
  // Rules reference columns by their RAW backend name (a blank header stays
  // ""); the placeholder is display-only, so resolution never breaks.
  const columnOptions = meta.headers.map((h, i) => ({
    value: h,
    label: h || `Column ${i + 1}`,
  }));
  const clearReport = useStore.getState().clearCrossvalReport;

  const beginDraft = (type: CrossRule["type"]) => {
    setDraftType(type);
    setDraft(emptyRule(type, meta.headers));
    setCombosText("");
  };

  const commitDraft = () => {
    if (!draft) return;
    const finished =
      draft.type === "allowedCombinations"
        ? { ...draft, allowed: parseCombinations(combosText) }
        : draft;
    if (ruleProblem(finished)) return;
    setRules((r) => [...r, finished]);
    setDraft(null);
    // The report pairs results with rules BY INDEX; an edited rule list
    // would misattribute violations, so results are cleared until rescan.
    clearReport();
  };

  const removeRule = (index: number) => {
    setRules((r) => r.filter((_, i) => i !== index));
    clearReport();
  };

  const saveToProfile = () => {
    if (!profile) return;
    const profiles = (settings?.profiles ?? []).map((p) =>
      p.id === profile.id ? { ...p, crossRules: rules } : p,
    );
    void saveProfiles(profiles);
  };

  const runFilter = async (rule: number | null) => {
    setWorking(true);
    const ok = await applyFilter(rule);
    setWorking(false);
    if (ok) onClose();
  };

  const exportReport = async () => {
    if (!report) return;
    const chosen = await saveFileDialog({
      defaultPath: "validation-report.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof chosen === "string") {
      await writeTextFile(chosen, JSON.stringify(report, null, 2));
    }
  };

  const jump = async (row: number, columnName: string) => {
    const col = Math.max(0, meta.headers.indexOf(columnName));
    onClose();
    await jumpToCell(row, col);
  };

  const draftProblem = draft
    ? ruleProblem(
        draft.type === "allowedCombinations"
          ? { ...draft, allowed: parseCombinations(combosText) }
          : draft,
      )
    : null;

  return (
    <Modal
      title="Cross-column validation"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-xs text-zinc-400">
            {profile ? `Rules can persist in profile "${profile.name}"` : ""}
          </span>
          {profile && (
            <button onClick={saveToProfile} disabled={scanning} className={btnGhost}>
              Save rules to profile
            </button>
          )}
          <button onClick={() => void exportReport()} disabled={!report} className={btnGhost}>
            Export report…
          </button>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {/* Current rules */}
        <div className="space-y-1.5">
          {rules.length === 0 && (
            <p className="text-xs text-zinc-400">
              No rules yet — add relational checks between columns below.
            </p>
          )}
          {rules.map((rule, i) => {
            const result = report?.rules.find((r) => r.rule === i);
            return (
              <div
                key={i}
                className="flex items-center gap-2 rounded border border-zinc-200 px-2 py-1.5 text-xs dark:border-zinc-800"
              >
                <span className="rounded bg-zinc-100 px-1.5 py-0.5 text-[11px] text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
                  {RULE_TYPE_LABELS[rule.type]}
                </span>
                <span className="truncate" title={describeRule(rule)}>
                  {describeRule(rule)}
                </span>
                <span className="flex-1" />
                {result && !stale && (
                  <span
                    className={
                      result.violations === 0
                        ? "text-emerald-600 dark:text-emerald-400"
                        : "text-red-600 dark:text-red-400"
                    }
                  >
                    {result.violations === 0
                      ? "passes"
                      : `${result.violations.toLocaleString()} violation${result.violations === 1 ? "" : "s"}`}
                  </span>
                )}
                {result && result.violations > 0 && !stale && (
                  <button onClick={() => void runFilter(i)} disabled={working} className={chipBtn}>
                    Filter
                  </button>
                )}
                <button
                  onClick={() => removeRule(i)}
                  disabled={scanning}
                  className="rounded px-1.5 py-0.5 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
                >
                  Remove
                </button>
              </div>
            );
          })}
        </div>

        {/* Builder */}
        {draft === null ? (
          <div className="flex items-center gap-2 text-xs">
            <select
              value={draftType}
              onChange={(e) => setDraftType(e.target.value as CrossRule["type"])}
              className={selectCls}
            >
              {Object.entries(RULE_TYPE_LABELS).map(([value, label]) => (
                <option key={value} value={value} className="dark:bg-zinc-800">
                  {label}
                </option>
              ))}
            </select>
            <button onClick={() => beginDraft(draftType)} className={chipBtn}>
              Add rule…
            </button>
          </div>
        ) : (
          <div className="space-y-2 rounded border border-violet-300 p-2 text-xs dark:border-violet-800">
            <div className="font-medium">{RULE_TYPE_LABELS[draft.type]}</div>
            <RuleFields
              draft={draft}
              columns={columnOptions}
              onChange={setDraft}
              combosText={combosText}
              onCombosChange={setCombosText}
            />
            {draftProblem && <p className="text-red-600 dark:text-red-400">{draftProblem}</p>}
            <div className="flex gap-2">
              <button
                onClick={commitDraft}
                disabled={draftProblem !== null}
                className="rounded bg-violet-600 px-2 py-1 text-white hover:bg-violet-500 disabled:opacity-40"
              >
                Add rule
              </button>
              <button onClick={() => setDraft(null)} className={btnGhost}>
                Cancel
              </button>
            </div>
          </div>
        )}

        {/* Scan */}
        <div className="flex items-center gap-3">
          <button
            onClick={() => void startScan(rules)}
            disabled={scanning || rules.length === 0}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {scanning ? "Validating…" : "Validate"}
          </button>
          {scanning && (
            <>
              <span className="text-xs text-zinc-500 dark:text-zinc-400">
                {processed.toLocaleString()}
                {total != null && ` / ${total.toLocaleString()}`} rows
              </span>
              <button
                onClick={() => void cancelScan()}
                className="rounded px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Cancel
              </button>
            </>
          )}
          {report && !scanning && (
            <span className="text-xs text-zinc-500 dark:text-zinc-400">
              {report.violatingRows.toLocaleString()} row
              {report.violatingRows === 1 ? "" : "s"} with violations across{" "}
              {report.scannedRows.toLocaleString()} scanned
            </span>
          )}
          {report && report.violatingRows > 0 && !scanning && !stale && (
            <button onClick={() => void runFilter(null)} disabled={working} className={chipBtn}>
              Filter to all violations
            </button>
          )}
        </div>

        {stale && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            The document changed since this validation — run it again before filtering.
          </p>
        )}
        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}

        {/* Violation samples */}
        {report && report.totalViolations > 0 && (
          <div className="max-h-[34vh] space-y-2 overflow-y-auto pr-1">
            {report.rules
              .filter((r) => r.violations > 0)
              .map((r) => (
                <div
                  key={r.rule}
                  className="rounded border border-zinc-200 p-2 text-xs dark:border-zinc-800"
                >
                  <p className="mb-1 font-medium">
                    {r.description} — {r.violations.toLocaleString()} violation
                    {r.violations === 1 ? "" : "s"}
                    {r.violations > r.sample.length && ` (showing first ${r.sample.length})`}
                  </p>
                  <div className="space-y-0.5">
                    {r.sample.slice(0, 8).map((v) => (
                      <div key={`${r.rule}-${v.row}`} className="flex items-center gap-2">
                        <button
                          onClick={() => void jump(v.row, v.values[0]?.[0] ?? "")}
                          title="Jump to row"
                          className="rounded border border-zinc-200 px-1 py-0 font-mono text-[11px] hover:border-violet-400 dark:border-zinc-700"
                        >
                          row {v.row + 1}
                        </button>
                        <span className="truncate font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                          {v.values.map(([name, value]) => `${name}=${value || "∅"}`).join(" · ")}
                        </span>
                        <span className="truncate text-zinc-400">{v.reason}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ))}
          </div>
        )}
      </div>
    </Modal>
  );
}

/** Type-specific editor fields for the rule being built. */
function RuleFields({
  draft,
  columns: columnOptions,
  onChange,
  combosText,
  onCombosChange,
}: {
  draft: CrossRule;
  /** Raw backend column name + display label (blank headers get one). */
  columns: { value: string; label: string }[];
  onChange: (rule: CrossRule) => void;
  combosText: string;
  onCombosChange: (text: string) => void;
}) {
  const colSelect = (value: string, set: (next: string) => void) => (
    <select value={value} onChange={(e) => set(e.target.value)} className={selectCls}>
      {columnOptions.map((c, i) => (
        <option key={i} value={c.value} className="dark:bg-zinc-800">
          {c.label}
        </option>
      ))}
    </select>
  );

  const multiCols = (columns: string[], set: (next: string[]) => void) => (
    <div className="flex flex-wrap gap-x-3 gap-y-1">
      {columnOptions.map((c, i) => (
        <label key={i} className="flex items-center gap-1">
          <input
            type="checkbox"
            checked={columns.includes(c.value)}
            onChange={(e) =>
              set(e.target.checked ? [...columns, c.value] : columns.filter((x) => x !== c.value))
            }
            className="accent-violet-600"
          />
          {c.label}
        </label>
      ))}
    </div>
  );

  switch (draft.type) {
    case "columnsEqual":
      return (
        <div className="flex flex-wrap items-center gap-2">
          {colSelect(draft.left, (left) => onChange({ ...draft, left }))}
          <select
            value={draft.negate ? "differ" : "equal"}
            onChange={(e) => onChange({ ...draft, negate: e.target.value === "differ" })}
            className={selectCls}
          >
            <option value="equal" className="dark:bg-zinc-800">
              must equal
            </option>
            <option value="differ" className="dark:bg-zinc-800">
              must differ from
            </option>
          </select>
          {colSelect(draft.right, (right) => onChange({ ...draft, right }))}
        </div>
      );
    case "numericCompare":
      return (
        <div className="flex flex-wrap items-center gap-2">
          {colSelect(draft.left, (left) => onChange({ ...draft, left }))}
          <select
            value={draft.op}
            onChange={(e) => onChange({ ...draft, op: e.target.value as CompareOp })}
            className={selectCls}
          >
            {Object.entries(COMPARE_OP_LABELS).map(([value, label]) => (
              <option key={value} value={value} className="dark:bg-zinc-800">
                {label}
              </option>
            ))}
          </select>
          {colSelect(draft.right, (right) => onChange({ ...draft, right }))}
        </div>
      );
    case "dateOrder":
      return (
        <div className="flex flex-wrap items-center gap-2">
          {colSelect(draft.earlier, (earlier) => onChange({ ...draft, earlier }))}
          <select
            value={draft.allowEqual ? "onOrBefore" : "before"}
            onChange={(e) => onChange({ ...draft, allowEqual: e.target.value === "onOrBefore" })}
            className={selectCls}
          >
            <option value="before" className="dark:bg-zinc-800">
              must be before
            </option>
            <option value="onOrBefore" className="dark:bg-zinc-800">
              must be on or before
            </option>
          </select>
          {colSelect(draft.later, (later) => onChange({ ...draft, later }))}
        </div>
      );
    case "conditionalRequired": {
      const when = draft.when;
      return (
        <div className="flex flex-wrap items-center gap-2">
          when {colSelect(draft.whenColumn, (whenColumn) => onChange({ ...draft, whenColumn }))}
          <select
            value={when.type}
            onChange={(e) => {
              const t = e.target.value as WhenCondition["type"];
              onChange({
                ...draft,
                when: t === "equals" ? { type: "equals", value: "" } : { type: t },
              });
            }}
            className={selectCls}
          >
            <option value="nonBlank" className="dark:bg-zinc-800">
              is not blank
            </option>
            <option value="blank" className="dark:bg-zinc-800">
              is blank
            </option>
            <option value="equals" className="dark:bg-zinc-800">
              equals
            </option>
          </select>
          {when.type === "equals" && (
            <input
              value={when.value}
              onChange={(e) =>
                onChange({ ...draft, when: { type: "equals", value: e.target.value } })
              }
              placeholder="value"
              className={inputCls}
            />
          )}
          then{" "}
          {colSelect(draft.thenRequired, (thenRequired) => onChange({ ...draft, thenRequired }))} is
          required
        </div>
      );
    }
    case "exactlyOne":
    case "atLeastOne":
    case "atMostOne":
      return multiCols(draft.columns, (columns) => onChange({ ...draft, columns }));
    case "sumEquals":
      return (
        <div className="space-y-1.5">
          <div>parts: {multiCols(draft.parts, (parts) => onChange({ ...draft, parts }))}</div>
          <div className="flex flex-wrap items-center gap-2">
            total {colSelect(draft.total, (total) => onChange({ ...draft, total }))}
            tolerance
            <input
              type="number"
              min={0}
              step="any"
              value={draft.tolerance}
              onChange={(e) => onChange({ ...draft, tolerance: Number(e.target.value) })}
              className="w-20 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
            />
            <select
              value={draft.tolerancePercent ? "percent" : "absolute"}
              onChange={(e) =>
                onChange({ ...draft, tolerancePercent: e.target.value === "percent" })
              }
              className={selectCls}
            >
              <option value="absolute" className="dark:bg-zinc-800">
                absolute
              </option>
              <option value="percent" className="dark:bg-zinc-800">
                % of total
              </option>
            </select>
          </div>
        </div>
      );
    case "allowedCombinations":
      return (
        <div className="space-y-1.5">
          <div>
            columns: {multiCols(draft.columns, (columns) => onChange({ ...draft, columns }))}
          </div>
          <textarea
            value={combosText}
            onChange={(e) => onCombosChange(e.target.value)}
            placeholder={"one combination per line, comma-separated\ne.g.  US, USD"}
            rows={3}
            className="w-full rounded border border-zinc-300 bg-transparent p-1.5 font-mono text-[11px] dark:border-zinc-600"
          />
        </div>
      );
  }
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700";
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
const inputCls =
  "w-32 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-600";
