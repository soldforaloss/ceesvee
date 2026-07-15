import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEffect, useState } from "react";

import { formatNumber } from "../lib/format";
import { useActiveMeta, useStore } from "../store/useStore";
import type { ColumnKind } from "../types";
import { Close } from "./Icons";

const KIND_BADGE: Record<ColumnKind, string> = {
  number: "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300",
  date: "bg-sky-100 text-sky-700 dark:bg-sky-500/15 dark:text-sky-300",
  bool: "bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300",
  text: "bg-zinc-100 text-zinc-600 dark:bg-zinc-700/40 dark:text-zinc-300",
};

/**
 * Interactive column explorer (F05): per-column profiling with type
 * distribution, blanks, distinct counts, bounded top values (approximate
 * results labelled), numeric quartiles, date extremes and text lengths —
 * with one-click filtering that never destroys the existing filter tree.
 */
export function ColumnExplorerPanel() {
  const meta = useActiveMeta();
  const explorer = useStore((s) => s.explorer);
  const column = useStore((s) => Math.min(s.activeExplorerColumn ?? 0, (meta?.colCount ?? 1) - 1));
  const setOpen = useStore((s) => s.setExplorerOpen);
  const setColumn = useStore((s) => s.setExplorerColumn);
  const setScope = useStore((s) => s.setExplorerScope);
  const refresh = useStore((s) => s.refreshExplorerProfile);
  const cancel = useStore((s) => s.cancelExplorerProfile);
  const applyValueFilter = useStore((s) => s.applyValueFilter);
  const applyRangeFilter = useStore((s) => s.applyRangeFilter);

  const [rangeMin, setRangeMin] = useState("");
  const [rangeMax, setRangeMax] = useState("");

  // Profile whenever the panel is visible and the document/column/scope (or
  // the data itself) changes; the backend cache absorbs redundant requests.
  useEffect(() => {
    if (explorer.open && meta) void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [explorer.open, explorer.scope, column, meta?.id, meta?.revision]);

  if (!meta || !explorer.open) return null;
  const { profile, jobId } = explorer;
  const loading = jobId != null;
  const percent =
    loading && explorer.total
      ? Math.min(100, Math.round((explorer.processed / explorer.total) * 100))
      : null;
  const showRange =
    profile !== null && (profile.inferredKind === "number" || profile.inferredKind === "date");

  return (
    <aside className="flex w-96 shrink-0 flex-col border-l border-zinc-200 bg-white text-sm dark:border-zinc-800 dark:bg-zinc-950">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-zinc-200 px-3 dark:border-zinc-800">
        <span className="font-semibold text-zinc-700 dark:text-zinc-200">Column explorer</span>
        <div className="flex-1" />
        <button
          title="Close explorer"
          onClick={() => setOpen(false)}
          className="rounded p-1 text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          <Close className="h-4 w-4" />
        </button>
      </div>

      <div className="flex shrink-0 items-center gap-2 border-b border-zinc-100 px-3 py-2 dark:border-zinc-800/60">
        <select
          value={column}
          onChange={(e) => setColumn(Number(e.target.value))}
          className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-xs outline-none focus:border-violet-500 dark:border-zinc-700"
        >
          {meta.headers.map((h, i) => (
            <option key={i} value={i} className="dark:bg-zinc-800">
              {h.trim() || `Column ${i + 1}`}
            </option>
          ))}
        </select>
        <div className="flex overflow-hidden rounded border border-zinc-300 text-[11px] dark:border-zinc-700">
          {(
            [
              ["all", "All rows"],
              ["visibleRows", "Visible"],
            ] as const
          ).map(([value, label]) => (
            <button
              key={value}
              onClick={() => setScope(value)}
              className={`px-2 py-1 ${
                explorer.scope === value
                  ? "bg-violet-600 text-white"
                  : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              }`}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        {loading && (
          <div className="mb-3 rounded border border-zinc-200 px-3 py-2 dark:border-zinc-800">
            <div className="mb-1.5 flex items-center justify-between text-xs">
              <span className="text-zinc-500">
                Profiling{percent !== null ? ` — ${percent}%` : "…"}
              </span>
              <button
                onClick={() => void cancel()}
                className="rounded px-1.5 py-0.5 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Cancel
              </button>
            </div>
            <div className="h-1.5 overflow-hidden rounded bg-zinc-100 dark:bg-zinc-800">
              <div
                className="h-full rounded bg-violet-500 transition-[width]"
                style={{ width: `${percent ?? 5}%` }}
              />
            </div>
          </div>
        )}

        {explorer.error && !loading && (
          <p className="mb-3 rounded border border-red-200 bg-red-50 px-2 py-1.5 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/50 dark:text-red-300">
            {explorer.error}
          </p>
        )}

        {profile && (
          <div className="space-y-3">
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <span
                className={`rounded px-1.5 py-0.5 font-medium ${KIND_BADGE[profile.inferredKind]}`}
              >
                {profile.inferredKind}
              </span>
              <span className="tabular-nums text-zinc-500">
                {profile.rowCount.toLocaleString()} rows
                {profile.scope === "visibleRows" && " (visible)"}
              </span>
            </div>

            <StatGrid
              stats={[
                [
                  "Blank",
                  `${profile.blankCount.toLocaleString()} (${
                    profile.rowCount ? Math.round((profile.blankCount / profile.rowCount) * 100) : 0
                  }%)`,
                ],
                [
                  "Distinct",
                  `${profile.distinctIsApproximate ? "≈ " : ""}${profile.distinctCount.toLocaleString()}`,
                ],
                ["Numbers", profile.typeCounts.number.toLocaleString()],
                ["Dates", profile.typeCounts.date.toLocaleString()],
                ["Booleans", profile.typeCounts.bool.toLocaleString()],
                ["Text", profile.typeCounts.text.toLocaleString()],
              ]}
            />

            {profile.numeric && (
              <Section title="Numbers">
                <StatGrid
                  stats={[
                    ["Min", formatNumber(profile.numeric.min)],
                    ["Max", formatNumber(profile.numeric.max)],
                    ["Mean", formatNumber(profile.numeric.mean)],
                    ["Median", formatNumber(profile.numeric.median)],
                    ["Q1", formatNumber(profile.numeric.q1)],
                    ["Q3", formatNumber(profile.numeric.q3)],
                  ]}
                />
              </Section>
            )}

            {(profile.earliestDate || profile.latestDate) && (
              <Section title="Dates">
                <StatGrid
                  stats={[
                    ["Earliest", profile.earliestDate ?? "—"],
                    ["Latest", profile.latestDate ?? "—"],
                  ]}
                />
              </Section>
            )}

            {profile.text && (
              <Section title="Text length">
                <StatGrid
                  stats={[
                    ["Shortest", profile.text.minLen.toLocaleString()],
                    ["Longest", profile.text.maxLen.toLocaleString()],
                    ["Average", formatNumber(profile.text.avgLen)],
                  ]}
                />
              </Section>
            )}

            {showRange && (
              <Section title="Range filter">
                <div className="flex items-center gap-1.5 text-xs">
                  <input
                    value={rangeMin}
                    onChange={(e) => setRangeMin(e.target.value)}
                    placeholder="min"
                    className={rangeInput}
                  />
                  <span className="text-zinc-400">to</span>
                  <input
                    value={rangeMax}
                    onChange={(e) => setRangeMax(e.target.value)}
                    placeholder="max"
                    className={rangeInput}
                  />
                  <button
                    onClick={() => void applyRangeFilter(rangeMin || null, rangeMax || null)}
                    disabled={!rangeMin && !rangeMax}
                    className="rounded bg-violet-600 px-2 py-1 font-medium text-white hover:bg-violet-500 disabled:opacity-40"
                  >
                    AND filter
                  </button>
                </div>
              </Section>
            )}

            <Section title={`Top values${profile.topIsApproximate ? " (≈ approximate)" : ""}`}>
              {profile.topValues.length === 0 ? (
                <p className="text-xs text-zinc-400">No non-blank values.</p>
              ) : (
                <ul className="space-y-0.5">
                  {profile.topValues.map((v) => (
                    <ValueRow
                      key={v.value}
                      value={v.value}
                      count={v.count}
                      maxCount={profile.topValues[0]?.count ?? 1}
                      approximate={profile.topIsApproximate}
                      onOnly={() => void applyValueFilter(v.value, "only")}
                      onExclude={() => void applyValueFilter(v.value, "exclude")}
                      onAnd={() => void applyValueFilter(v.value, "and")}
                      onCopy={() => void writeText(v.value).catch(() => undefined)}
                    />
                  ))}
                </ul>
              )}
            </Section>
          </div>
        )}
      </div>
    </aside>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500">
        {title}
      </div>
      {children}
    </div>
  );
}

function StatGrid({ stats }: { stats: [string, string][] }) {
  return (
    <dl className="grid grid-cols-2 gap-x-4 gap-y-0.5 text-xs">
      {stats.map(([label, value]) => (
        <div key={label} className="flex items-baseline justify-between gap-2">
          <dt className="text-zinc-400 dark:text-zinc-500">{label}</dt>
          <dd className="truncate text-right tabular-nums text-zinc-700 dark:text-zinc-200">
            {value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function ValueRow({
  value,
  count,
  maxCount,
  approximate,
  onOnly,
  onExclude,
  onAnd,
  onCopy,
}: {
  value: string;
  count: number;
  maxCount: number;
  approximate: boolean;
  onOnly: () => void;
  onExclude: () => void;
  onAnd: () => void;
  onCopy: () => void;
}) {
  return (
    <li className="group relative rounded px-1 py-0.5 text-xs hover:bg-zinc-50 dark:hover:bg-zinc-900">
      <div
        className="absolute inset-y-0 left-0 rounded bg-violet-100/70 dark:bg-violet-500/10"
        style={{ width: `${Math.max(2, (count / maxCount) * 100)}%` }}
      />
      <div className="relative flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate" title={value}>
          {value === "" ? <em className="text-zinc-400">(empty)</em> : value}
        </span>
        <span className="shrink-0 tabular-nums text-zinc-400">
          {approximate ? "≥" : ""}
          {count.toLocaleString()}
        </span>
        <span className="hidden shrink-0 items-center gap-0.5 group-hover:flex">
          <ValueAction title="Filter to this value" onClick={onOnly} label="=" />
          <ValueAction title="Exclude this value" onClick={onExclude} label="≠" />
          <ValueAction title="AND into the current filter" onClick={onAnd} label="+" />
          <ValueAction title="Copy value" onClick={onCopy} label="⧉" />
        </span>
      </div>
    </li>
  );
}

function ValueAction({
  title,
  label,
  onClick,
}: {
  title: string;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      className="rounded border border-zinc-300 px-1 leading-4 text-zinc-500 hover:bg-violet-50 hover:text-violet-700 dark:border-zinc-700 dark:hover:bg-violet-500/10 dark:hover:text-violet-300"
    >
      {label}
    </button>
  );
}

const rangeInput =
  "w-20 rounded border border-zinc-300 bg-transparent px-1.5 py-1 tabular-nums outline-none focus:border-violet-500 dark:border-zinc-700";
