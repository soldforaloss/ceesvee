import { useState } from "react";

import { JOIN_CONFIRM_THRESHOLD, joinNeedsConfirmation, joinRunLabel } from "../lib/joins";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { JoinPreview, JoinSpec, JoinType } from "../types";
import { Modal } from "./Modal";

const JOIN_LABELS: Record<JoinType, string> = {
  inner: "Inner (matching rows only)",
  left: "Left outer (keep all left rows)",
  right: "Right outer (keep all right rows)",
  full: "Full outer (keep everything)",
  leftAnti: "Left anti (left rows WITHOUT a match)",
  rightAnti: "Right anti (right rows WITHOUT a match)",
};

/**
 * Relational joins (F21): join the active document with another open tab on
 * ordered composite keys into a NEW document — both sources stay untouched.
 * The preview reports match/unmatched/duplicate-key counts and the
 * projected output size; expansion past the threshold needs an explicit
 * second click.
 */
export function JoinDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const tabs = useStore((s) => s.tabs);
  const derive = useStore((s) => s.derive);
  const deriveError = useStore((s) => s.deriveError);
  const trackDerive = useStore((s) => s.trackDerive);
  const cancelDerive = useStore((s) => s.cancelDerive);

  const others = tabs.filter((t) => t.id !== meta?.id);
  const [rightId, setRightId] = useState<number | null>(others[0]?.id ?? null);
  const [joinType, setJoinType] = useState<JoinType>("left");
  const [leftKeys, setLeftKeys] = useState<number[]>([0]);
  const [rightKeys, setRightKeys] = useState<number[]>([0]);
  const [rightColumns, setRightColumns] = useState<number[]>([]);
  const [lookup, setLookup] = useState(false);
  const [trim, setTrim] = useState(true);
  const [caseInsensitive, setCaseInsensitive] = useState(false);
  const [blankEqual, setBlankEqual] = useState(false);
  const [numericEqual, setNumericEqual] = useState(false);
  const [dateEqual, setDateEqual] = useState(false);
  const [suffix, setSuffix] = useState(" (right)");
  const [preview, setPreview] = useState<JoinPreview | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const running = derive?.kind === "join";
  const right = tabs.find((t) => t.id === rightId);

  if (!meta) return null;
  if (others.length === 0) {
    return (
      <Modal title="Join" onClose={onClose} size="md">
        <p className="py-4 text-sm text-zinc-500 dark:text-zinc-400">
          Joining needs a second open document — open the other file first.
        </p>
      </Modal>
    );
  }

  const leftHeaders = meta.headers.map((h, i) => h || `Column ${i + 1}`);
  const rightHeaders = (right?.headers ?? []).map((h, i) => h || `Column ${i + 1}`);

  const buildSpec = (cap: number | null): JoinSpec => ({
    join: joinType,
    leftKeys,
    rightKeys,
    rightColumns,
    lookup,
    collisionSuffix: suffix,
    normalization: { trim, caseInsensitive, blankEqual, numericEqual, dateEqual },
    maxOutputRows: cap,
  });

  const invalidate = () => {
    setPreview(null);
    setConfirmed(false);
  };

  const runPreview = async () => {
    if (!right) return;
    setError(null);
    try {
      setPreview(
        await api.previewJoin(meta.id, right.id, buildSpec(null), meta.revision, right.revision),
      );
    } catch (e) {
      setError(String(e));
      setPreview(null);
    }
  };

  const overThreshold = joinNeedsConfirmation(preview);

  const run = async () => {
    if (!right || !preview) return;
    setError(null);
    try {
      const cap = confirmed || !overThreshold ? null : JOIN_CONFIRM_THRESHOLD;
      const started = await api.startJoin(
        meta.id,
        right.id,
        buildSpec(cap),
        meta.revision,
        right.revision,
      );
      trackDerive(started.jobId, started.docId, "join");
    } catch (e) {
      setError(String(e));
    }
  };

  const keyPicker = (
    label: string,
    headers: string[],
    keys: number[],
    setKeys: (next: number[]) => void,
  ) => (
    <div className="text-xs">
      <p className="mb-1 text-zinc-500 dark:text-zinc-400">{label} (in order):</p>
      <div className="flex flex-wrap items-center gap-1.5">
        {keys.map((k, i) => (
          <span key={i} className="flex items-center gap-1">
            <select
              value={k}
              onChange={(e) => {
                setKeys(keys.map((x, j) => (j === i ? Number(e.target.value) : x)));
                invalidate();
              }}
              className={selectCls}
            >
              {headers.map((h, ci) => (
                <option key={ci} value={ci} className="dark:bg-zinc-800">
                  {h}
                </option>
              ))}
            </select>
            {keys.length > 1 && (
              <button
                onClick={() => {
                  setKeys(keys.filter((_, j) => j !== i));
                  invalidate();
                }}
                className="text-red-600 hover:underline dark:text-red-400"
              >
                ×
              </button>
            )}
          </span>
        ))}
        <button
          onClick={() => {
            setKeys([...keys, 0]);
            invalidate();
          }}
          className={chipBtn}
        >
          + key
        </button>
      </div>
    </div>
  );

  return (
    <Modal
      title="Join documents"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
          <button
            onClick={() => void runPreview()}
            disabled={!right || running || leftKeys.length === 0}
            className={btnGhost}
          >
            Preview
          </button>
          <button
            onClick={() => {
              if (overThreshold && !confirmed) {
                setConfirmed(true);
                return;
              }
              void run();
            }}
            disabled={!right || running || !preview || (preview !== null && preview.lookupConflict)}
            title={
              preview?.lookupConflict
                ? "Lookup mode needs unique right-side keys"
                : !preview
                  ? "Preview first"
                  : undefined
            }
            className={`rounded px-3 py-1.5 text-sm text-white disabled:opacity-40 ${
              overThreshold && !confirmed
                ? "bg-amber-600 hover:bg-amber-500"
                : "bg-violet-600 hover:bg-violet-500"
            }`}
          >
            {joinRunLabel(preview, confirmed, running)}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <span className="font-medium">{meta.fileName}</span>
          <select
            value={joinType}
            onChange={(e) => {
              setJoinType(e.target.value as JoinType);
              invalidate();
            }}
            className={selectCls}
          >
            {Object.entries(JOIN_LABELS).map(([value, label]) => (
              <option key={value} value={value} className="dark:bg-zinc-800">
                {label}
              </option>
            ))}
          </select>
          <select
            value={rightId ?? ""}
            onChange={(e) => {
              setRightId(Number(e.target.value));
              setRightKeys([0]);
              setRightColumns([]);
              invalidate();
            }}
            className={selectCls}
          >
            {others.map((t) => (
              <option key={t.id} value={t.id} className="dark:bg-zinc-800">
                {t.fileName}
              </option>
            ))}
          </select>
          <label className="flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={lookup}
              onChange={(e) => {
                setLookup(e.target.checked);
                invalidate();
              }}
              className="accent-violet-600"
            />
            Lookup mode (unique right keys)
          </label>
        </div>

        {keyPicker("Left keys", leftHeaders, leftKeys, setLeftKeys)}
        {keyPicker("Right keys", rightHeaders, rightKeys, setRightKeys)}

        <div className="text-xs">
          <p className="mb-1 text-zinc-500 dark:text-zinc-400">Right columns to include:</p>
          <div className="flex max-h-20 flex-wrap gap-x-3 gap-y-1 overflow-y-auto">
            {rightHeaders.map((h, i) => (
              <label key={i} className="flex items-center gap-1">
                <input
                  type="checkbox"
                  checked={rightColumns.includes(i)}
                  onChange={(e) => {
                    setRightColumns(
                      e.target.checked
                        ? [...rightColumns, i].sort((a, b) => a - b)
                        : rightColumns.filter((c) => c !== i),
                    );
                    invalidate();
                  }}
                  className="accent-violet-600"
                />
                {h}
              </label>
            ))}
          </div>
          <label className="mt-1.5 flex items-center gap-1.5">
            Collision suffix
            <input
              value={suffix}
              onChange={(e) => {
                setSuffix(e.target.value);
                invalidate();
              }}
              className="w-28 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 dark:border-zinc-600"
            />
          </label>
        </div>

        <div className="flex flex-wrap gap-x-4 gap-y-1.5 text-xs">
          <Check
            label="Trim keys"
            value={trim}
            onChange={(v) => {
              setTrim(v);
              invalidate();
            }}
          />
          <Check
            label="Case-insensitive"
            value={caseInsensitive}
            onChange={(v) => {
              setCaseInsensitive(v);
              invalidate();
            }}
          />
          <Check
            label="Blank keys match blanks"
            value={blankEqual}
            onChange={(v) => {
              setBlankEqual(v);
              invalidate();
            }}
          />
          <Check
            label="Numeric equivalence"
            value={numericEqual}
            onChange={(v) => {
              setNumericEqual(v);
              invalidate();
            }}
          />
          <Check
            label="Date equivalence"
            value={dateEqual}
            onChange={(v) => {
              setDateEqual(v);
              invalidate();
            }}
          />
        </div>

        {(error ?? deriveError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? deriveError}</p>
        )}

        {running && derive && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              {derive.message ?? "joining"} — {derive.processed.toLocaleString()}
              {derive.total != null && ` / ${derive.total.toLocaleString()}`}
            </span>
            <button
              onClick={() => void cancelDerive()}
              className="rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
            >
              Cancel
            </button>
          </div>
        )}

        {preview && (
          <div className="space-y-1 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {preview.projectedRows.toLocaleString()} projected rows ·{" "}
              {preview.matchedPairs.toLocaleString()} matched pairs
              {preview.expands && " · one-to-many expansion"}
            </p>
            <p className="text-zinc-500 dark:text-zinc-400">
              left: {preview.leftRows.toLocaleString()} rows,{" "}
              {preview.leftUnmatched.toLocaleString()} unmatched,{" "}
              {preview.leftDuplicateKeys.toLocaleString()} duplicate keys · right:{" "}
              {preview.rightRows.toLocaleString()} rows, {preview.rightUnmatched.toLocaleString()}{" "}
              unmatched, {preview.rightDuplicateKeys.toLocaleString()} duplicate keys
            </p>
            {preview.lookupConflict && (
              <p className="text-red-600 dark:text-red-400">
                Lookup mode needs unique right-side keys — this key set has duplicates.
              </p>
            )}
            <p className="truncate font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
              {preview.outputColumns.join(" · ")}
            </p>
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
