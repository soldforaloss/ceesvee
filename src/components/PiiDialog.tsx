import { useEffect, useState } from "react";

import { nonPiiColumns, redactionNeedsSecret } from "../lib/pii";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { PiiDetector, RedactionAction, RedactionPreview } from "../types";
import { Modal } from "./Modal";

type ActionKind = RedactionAction["type"];

const ACTION_LABELS: Record<ActionKind, string> = {
  fixedReplacement: "Replace with fixed text",
  keepLast: "Keep last N characters",
  fullMask: "Mask completely",
  pseudonymize: "Pseudonymize (HMAC)",
  removeColumn: "Remove the whole column",
  removeRows: "Remove matching rows",
};

/**
 * PII detection and redaction (F28): deterministic detectors only (this
 * does NOT claim to find names, addresses, or all PII). Samples and
 * previews are always MASKED; the pseudonymization secret is used per run
 * and never stored; every redaction is previewed, applies as one undo
 * step, targets one explicitly selected finding, and is audited locally
 * without values. Nothing leaves this device.
 */
export function PiiDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const pii = useStore((s) => s.pii);
  const startScan = useStore((s) => s.startPiiScan);
  const cancelScan = useStore((s) => s.cancelPiiScan);
  const loadCached = useStore((s) => s.loadCachedPiiReport);
  const clearReport = useStore((s) => s.clearPiiReport);
  const refresh = useStore((s) => s.refreshActiveDoc);
  const setSelection = useStore((s) => s.setSelection);
  const setModal = useStore((s) => s.setModal);

  const [useEmail, setUseEmail] = useState(true);
  const [usePhone, setUsePhone] = useState(true);
  const [useIp, setUseIp] = useState(true);
  const [useSsn, setUseSsn] = useState(true);
  const [useCard, setUseCard] = useState(true);
  const [customName, setCustomName] = useState("");
  const [customPattern, setCustomPattern] = useState("");
  const [actionKind, setActionKind] = useState<ActionKind>("fullMask");
  const [replacement, setReplacement] = useState("[REDACTED]");
  const [keepN, setKeepN] = useState(4);
  const [secret, setSecret] = useState("");
  const [salt, setSalt] = useState("");
  const [target, setTarget] = useState<{ detector: number; column: number } | null>(null);
  const [preview, setPreview] = useState<RedactionPreview | null>(null);
  const [working, setWorking] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    void loadCached();
  }, [loadCached]);

  const { report, spec, scanJobId, processed, total, error } = pii;
  const scanning = scanJobId != null;

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";
  const stale = report !== null && report.revision !== meta.revision;
  const headers = meta.headers.map((h, i) => h || `Column ${i + 1}`);

  const buildDetectors = (): PiiDetector[] => {
    const detectors: PiiDetector[] = [];
    if (useEmail) detectors.push({ type: "email" });
    if (usePhone) detectors.push({ type: "phoneNumber" });
    if (useIp) detectors.push({ type: "ipAddress" });
    if (useSsn) detectors.push({ type: "ssn" });
    if (useCard) detectors.push({ type: "paymentCard" });
    if (customPattern.trim() !== "") {
      detectors.push({
        type: "custom",
        name: customName.trim() || "custom",
        pattern: customPattern,
      });
    }
    return detectors;
  };

  const runScan = () => {
    setTarget(null);
    setPreview(null);
    setActionError(null);
    void startScan({ detectors: buildDetectors(), scope: { type: "all" } });
  };

  const buildAction = (): RedactionAction => {
    switch (actionKind) {
      case "fixedReplacement":
        return { type: actionKind, replacement };
      case "keepLast":
        return { type: actionKind, n: keepN };
      case "pseudonymize":
        return { type: actionKind, secret, salt: salt.trim() === "" ? null : salt.trim() };
      default:
        return { type: actionKind } as RedactionAction;
    }
  };

  const runPreview = async () => {
    if (!spec || !report || !target) return;
    setActionError(null);
    try {
      setPreview(
        await api.previewRedaction(
          meta.id,
          spec,
          target.detector,
          target.column,
          buildAction(),
          report.revision,
        ),
      );
    } catch (e) {
      setActionError(String(e));
    }
  };

  const confirmApply = async () => {
    if (!spec || !report || !target || !preview) return;
    setWorking(true);
    setActionError(null);
    try {
      const action = buildAction();
      // Reuse the previewed salt so the applied pseudonyms match it.
      const finalAction: RedactionAction =
        action.type === "pseudonymize" && preview.salt ? { ...action, salt: preview.salt } : action;
      await api.applyRedaction(
        meta.id,
        spec,
        target.detector,
        target.column,
        finalAction,
        report.revision,
      );
      await refresh();
      clearReport();
      setPreview(null);
      setTarget(null);
    } catch (e) {
      setActionError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const exportNonPii = () => {
    if (!report) return;
    const safe = nonPiiColumns(report.findings, headers.length);
    if (!safe) {
      setActionError("every column has findings — nothing safe to export");
      return;
    }
    // Hand the safe column selection to the ordinary export dialog.
    setSelection(null, [], safe);
    setModal("export");
  };

  const structural = actionKind === "removeColumn" || actionKind === "removeRows";

  return (
    <Modal
      title="Find personal data"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-xs text-zinc-400">
            Deterministic detectors only — treat misses AND matches critically. Nothing leaves this
            device.
          </span>
          <button onClick={exportNonPii} disabled={!report || stale} className={btnGhost}>
            Export non-PII columns…
          </button>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap gap-x-4 gap-y-1.5 text-xs">
          <Check label="Emails" value={useEmail} onChange={setUseEmail} />
          <Check label="Phone numbers" value={usePhone} onChange={setUsePhone} />
          <Check label="IP addresses" value={useIp} onChange={setUseIp} />
          <Check label="SSN patterns" value={useSsn} onChange={setUseSsn} />
          <Check label="Payment cards (Luhn)" value={useCard} onChange={setUseCard} />
        </div>
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <label className="flex items-center gap-1.5">
            Custom pattern
            <input
              value={customName}
              onChange={(e) => setCustomName(e.target.value)}
              placeholder="name"
              className="w-24 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 dark:border-zinc-600"
            />
            <input
              value={customPattern}
              onChange={(e) => setCustomPattern(e.target.value)}
              placeholder="regex (full match)"
              className="w-56 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 font-mono dark:border-zinc-600"
            />
          </label>
        </div>

        <div className="flex items-center gap-3">
          <button
            onClick={runScan}
            disabled={scanning}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {scanning ? "Scanning…" : "Scan for personal data"}
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
              {report.totalMatches.toLocaleString()} match
              {report.totalMatches === 1 ? "" : "es"} across{" "}
              {report.findings.length.toLocaleString()} finding
              {report.findings.length === 1 ? "" : "s"}
            </span>
          )}
        </div>

        {stale && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            The document changed since this scan — redactions are disabled. Scan again.
          </p>
        )}
        {(error ?? actionError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? actionError}</p>
        )}

        {report && report.findings.length > 0 && (
          <div className="max-h-[24vh] space-y-1 overflow-y-auto pr-1 text-xs">
            {report.findings.map((f, i) => {
              const selected = target?.detector === f.detector && target?.column === f.column;
              return (
                <button
                  key={i}
                  onClick={() => {
                    setTarget({ detector: f.detector, column: f.column });
                    setPreview(null);
                  }}
                  disabled={stale}
                  className={`flex w-full items-center gap-2 rounded border px-2 py-1 text-left ${
                    selected
                      ? "border-violet-500 bg-violet-50/60 dark:bg-violet-500/10"
                      : "border-zinc-200 hover:border-violet-400 dark:border-zinc-800"
                  }`}
                >
                  <span className="font-medium">{f.detectorLabel}</span>
                  <span className="text-zinc-400">
                    in {headers[f.column] ?? `Column ${f.column + 1}`}
                  </span>
                  <span className="text-zinc-400">
                    · {f.count.toLocaleString()} · {f.validation}
                  </span>
                  <span className="flex-1" />
                  <span className="truncate font-mono text-[11px] text-zinc-500">
                    {f.samples.join("  ")}
                  </span>
                </button>
              );
            })}
          </div>
        )}
        {report && report.findings.length === 0 && (
          <p className="py-2 text-center text-xs text-zinc-400">
            No matches with these detectors — absence of findings is not proof of absence.
          </p>
        )}

        {/* Redaction */}
        {target && report && !stale && (
          <div className="space-y-2 rounded border border-violet-300 p-2 text-xs dark:border-violet-800">
            <div className="flex flex-wrap items-center gap-2">
              <select
                value={actionKind}
                onChange={(e) => {
                  setActionKind(e.target.value as ActionKind);
                  setPreview(null);
                }}
                className={selectCls}
              >
                {Object.entries(ACTION_LABELS).map(([value, label]) => (
                  <option key={value} value={value} className="dark:bg-zinc-800">
                    {label}
                  </option>
                ))}
              </select>
              {actionKind === "fixedReplacement" && (
                <input
                  value={replacement}
                  onChange={(e) => {
                    setReplacement(e.target.value);
                    setPreview(null);
                  }}
                  className={inputCls}
                />
              )}
              {actionKind === "keepLast" && (
                <input
                  type="number"
                  min={1}
                  max={32}
                  value={keepN}
                  onChange={(e) => {
                    setKeepN(Number(e.target.value));
                    setPreview(null);
                  }}
                  className="w-16 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
                />
              )}
              {actionKind === "pseudonymize" && (
                <>
                  <input
                    type="password"
                    value={secret}
                    onChange={(e) => {
                      setSecret(e.target.value);
                      setPreview(null);
                    }}
                    placeholder="secret (never stored)"
                    className={inputCls}
                  />
                  <input
                    value={salt}
                    onChange={(e) => {
                      setSalt(e.target.value);
                      setPreview(null);
                    }}
                    placeholder="salt (blank = generate)"
                    className={inputCls}
                  />
                </>
              )}
              <button
                onClick={() => void runPreview()}
                disabled={readOnly || redactionNeedsSecret(actionKind, secret)}
                title={readOnly ? "Read-only (indexed) document" : undefined}
                className={chipBtn}
              >
                Preview…
              </button>
            </div>

            {preview && (
              <div className="space-y-1">
                <p className="font-medium">
                  {preview.columnRemoved
                    ? "The whole column would be removed."
                    : preview.rowsRemoved > 0
                      ? `${preview.rowsRemoved.toLocaleString()} rows would be removed.`
                      : `${preview.cellsAffected.toLocaleString()} cells would change.`}
                  {preview.salt && (
                    <span className="ml-2 font-normal text-zinc-400">
                      salt: <span className="font-mono">{preview.salt}</span> (save it to keep
                      pseudonyms consistent)
                    </span>
                  )}
                </p>
                {preview.examples.length > 0 && (
                  <ul className="space-y-0.5 font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                    {preview.examples.slice(0, 5).map(([before, after], i) => (
                      <li key={i} className="truncate">
                        {before} → {after}
                      </li>
                    ))}
                  </ul>
                )}
                <button
                  onClick={() => void confirmApply()}
                  disabled={
                    working ||
                    (preview.cellsAffected === 0 &&
                      preview.rowsRemoved === 0 &&
                      !preview.columnRemoved)
                  }
                  className={`rounded px-2 py-1 text-white disabled:opacity-40 ${
                    structural ? "bg-red-600 hover:bg-red-500" : "bg-violet-600 hover:bg-violet-500"
                  }`}
                >
                  {working ? "Applying…" : "Apply (one undo step)"}
                </button>
              </div>
            )}
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
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
const inputCls =
  "w-44 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-600";
