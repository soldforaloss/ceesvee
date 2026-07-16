import { save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { useEffect, useState } from "react";

import {
  buildClusterMapping,
  defaultDecisions,
  rowsAffectedByDecisions,
  type ClusterDecision,
} from "../lib/clustering";
import { useActiveMeta, useStore } from "../store/useStore";
import type { ClusterMethod, ClusterSpec, ExportScope } from "../types";
import { Modal } from "./Modal";

type MethodKey = "fingerprint" | "ngramFingerprint" | "levenshtein" | "jaroWinkler";

/**
 * Fuzzy value clustering (F24): find likely variants of the same value in
 * one column and normalize them in bulk. Deterministic methods; nothing is
 * ever applied automatically — the user accepts clusters explicitly and the
 * whole application is one undo step, revision-guarded.
 */
export function ClusterDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const cluster = useStore((s) => s.cluster);
  const startScan = useStore((s) => s.startClusterScan);
  const cancelScan = useStore((s) => s.cancelClusterScan);
  const clearReport = useStore((s) => s.clearClusterReport);
  const applyClusters = useStore((s) => s.applyClusters);

  const [column, setColumn] = useState(0);
  const [method, setMethod] = useState<MethodKey>("fingerprint");
  const [maxDistance, setMaxDistance] = useState(2);
  const [minSimilarity, setMinSimilarity] = useState(0.92);
  const [ngram, setNgram] = useState(2);
  const [caseFold, setCaseFold] = useState(true);
  const [trimCollapse, setTrimCollapse] = useState(true);
  const [stripPunctuation, setStripPunctuation] = useState(true);
  const [stripDiacritics, setStripDiacritics] = useState(false);
  const [sortWords, setSortWords] = useState(true);
  const [scopeVisible, setScopeVisible] = useState(false);
  const [decisions, setDecisions] = useState<ClusterDecision[]>([]);
  const [working, setWorking] = useState(false);

  const { report, scanJobId, error } = cluster;
  const scanning = scanJobId != null;

  // Fresh decisions whenever a new report lands.
  useEffect(() => {
    setDecisions(report ? defaultDecisions(report.clusters) : []);
  }, [report]);

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";
  const stale = report !== null && report.revision !== meta.revision;

  const buildMethod = (): ClusterMethod => {
    switch (method) {
      case "ngramFingerprint":
        return { type: "ngramFingerprint", n: ngram };
      case "levenshtein":
        return { type: "levenshtein", maxDistance };
      case "jaroWinkler":
        return { type: "jaroWinkler", minSimilarity };
      default:
        return { type: "fingerprint" };
    }
  };

  const scope: ExportScope = scopeVisible ? { type: "visibleRows" } : { type: "all" };

  const runScan = () => {
    const spec: ClusterSpec = {
      column,
      method: buildMethod(),
      normalization: { caseFold, trimCollapse, stripPunctuation, stripDiacritics, sortWords },
      scope,
    };
    void startScan(spec);
  };

  const mapping = report ? buildClusterMapping(report.clusters, decisions) : [];
  const affected = report ? rowsAffectedByDecisions(report.clusters, decisions) : 0;

  const apply = async () => {
    if (!report || mapping.length === 0) return;
    setWorking(true);
    const ok = await applyClusters(report.column, mapping, scope, report.revision);
    setWorking(false);
    if (ok) onClose();
  };

  const exportMapping = async () => {
    if (mapping.length === 0) return;
    const chosen = await saveFileDialog({
      defaultPath: "value-mapping.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof chosen === "string") {
      await writeTextFile(
        chosen,
        JSON.stringify(
          mapping.map(([from, to]) => ({ from, to })),
          null,
          2,
        ),
      );
    }
  };

  const setDecision = (index: number, patch: Partial<ClusterDecision>) =>
    setDecisions((d) => d.map((dec, i) => (i === index ? { ...dec, ...patch } : dec)));

  return (
    <Modal
      title="Cluster values"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
          <button
            onClick={() => void exportMapping()}
            disabled={mapping.length === 0}
            className={btnGhost}
          >
            Export mapping…
          </button>
          <button
            onClick={() => void apply()}
            disabled={working || readOnly || stale || mapping.length === 0}
            title={
              readOnly
                ? "Read-only (indexed) document"
                : stale
                  ? "The document changed — run the scan again"
                  : undefined
            }
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {working ? "Applying…" : `Apply to ${affected.toLocaleString()} rows`}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <label className="flex items-center gap-1.5">
            Column
            <select
              value={column}
              onChange={(e) => {
                setColumn(Number(e.target.value));
                clearReport();
              }}
              className={selectCls}
            >
              {meta.headers.map((h, i) => (
                <option key={i} value={i} className="dark:bg-zinc-800">
                  {h || `Column ${i + 1}`}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-1.5">
            Method
            <select
              value={method}
              onChange={(e) => setMethod(e.target.value as MethodKey)}
              className={selectCls}
            >
              <option value="fingerprint">Fingerprint (key collision)</option>
              <option value="ngramFingerprint">N-gram fingerprint</option>
              <option value="levenshtein">Levenshtein distance</option>
              <option value="jaroWinkler">Jaro-Winkler similarity</option>
            </select>
          </label>
          {method === "ngramFingerprint" && (
            <label className="flex items-center gap-1.5">
              n
              <input
                type="number"
                min={1}
                max={4}
                value={ngram}
                onChange={(e) => setNgram(Number(e.target.value))}
                className={numCls}
              />
            </label>
          )}
          {method === "levenshtein" && (
            <label className="flex items-center gap-1.5">
              Max distance
              <input
                type="number"
                min={1}
                max={5}
                value={maxDistance}
                onChange={(e) => setMaxDistance(Number(e.target.value))}
                className={numCls}
              />
            </label>
          )}
          {method === "jaroWinkler" && (
            <label className="flex items-center gap-1.5">
              Min similarity
              <input
                type="number"
                min={0.5}
                max={1}
                step={0.01}
                value={minSimilarity}
                onChange={(e) => setMinSimilarity(Number(e.target.value))}
                className="w-20 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
              />
            </label>
          )}
        </div>

        <div className="flex flex-wrap gap-x-4 gap-y-1.5 text-xs">
          <Check label="Case-insensitive" value={caseFold} onChange={setCaseFold} />
          <Check label="Trim & collapse spaces" value={trimCollapse} onChange={setTrimCollapse} />
          <Check
            label="Ignore punctuation"
            value={stripPunctuation}
            onChange={setStripPunctuation}
          />
          <Check label="Ignore accents" value={stripDiacritics} onChange={setStripDiacritics} />
          <Check label="Ignore word order" value={sortWords} onChange={setSortWords} />
          {meta.filtered && (
            <Check label="Visible rows only" value={scopeVisible} onChange={setScopeVisible} />
          )}
        </div>

        <div className="flex items-center gap-3">
          <button
            onClick={runScan}
            disabled={scanning}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {scanning ? "Scanning…" : "Find clusters"}
          </button>
          {scanning && (
            <button
              onClick={() => void cancelScan()}
              className="rounded px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
            >
              Cancel
            </button>
          )}
          {report && (
            <span className="text-xs text-zinc-500 dark:text-zinc-400">
              {report.totalClusters.toLocaleString()} cluster
              {report.totalClusters === 1 ? "" : "s"} across{" "}
              {report.distinctValues.toLocaleString()} distinct values
              {report.totalClusters > report.clusters.length &&
                ` (showing top ${report.clusters.length})`}
            </span>
          )}
        </div>

        {stale && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            The document changed since this scan — results cannot be applied. Run it again.
          </p>
        )}
        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}

        {report && report.clusters.length === 0 && (
          <p className="py-4 text-center text-xs text-zinc-400">
            No clusters found with these settings.
          </p>
        )}

        {report && report.clusters.length > 0 && (
          <div className="max-h-[42vh] space-y-2 overflow-y-auto pr-1">
            {report.clusters.map((c, index) => {
              const decision = decisions[index] ?? { accepted: false, canonical: c.suggested };
              return (
                <div
                  key={`${c.matchKey}-${index}`}
                  className={`rounded border p-2 ${
                    decision.accepted
                      ? "border-violet-400 bg-violet-50/50 dark:bg-violet-500/5"
                      : "border-zinc-200 dark:border-zinc-800"
                  }`}
                >
                  <div className="flex items-center gap-2 text-xs">
                    <label className="flex items-center gap-1.5 font-medium">
                      <input
                        type="checkbox"
                        checked={decision.accepted}
                        onChange={(e) => setDecision(index, { accepted: e.target.checked })}
                        className="accent-violet-600"
                      />
                      {c.members.length} values · {c.rowsAffected.toLocaleString()} rows
                    </label>
                    <span className="text-zinc-400">({c.matchKey})</span>
                    <span className="flex-1" />
                    <label className="flex items-center gap-1.5">
                      Replace with
                      <input
                        value={decision.canonical}
                        onChange={(e) => setDecision(index, { canonical: e.target.value })}
                        className="w-48 rounded border border-zinc-300 bg-white px-1.5 py-0.5 dark:border-zinc-600 dark:bg-zinc-950"
                      />
                    </label>
                  </div>
                  <div className="mt-1.5 flex flex-wrap gap-1.5">
                    {c.members.map((m) => (
                      <button
                        key={m.value}
                        onClick={() => setDecision(index, { canonical: m.value })}
                        title="Use as the canonical value"
                        className={`rounded border px-1.5 py-0.5 font-mono text-[11px] ${
                          m.value === decision.canonical
                            ? "border-violet-500 bg-violet-100 dark:bg-violet-500/20"
                            : "border-zinc-200 hover:border-violet-400 dark:border-zinc-700"
                        }`}
                      >
                        {m.value} ×{m.count}
                      </button>
                    ))}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </Modal>
  );
}

function Check({
  label,
  value,
  onChange,
}: {
  label: string;
  value: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-1.5">
      <input
        type="checkbox"
        checked={value}
        onChange={(e) => onChange(e.target.checked)}
        className="accent-violet-600"
      />
      {label}
    </label>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700";
const numCls =
  "w-14 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600";
