import { useStore } from "../store/useStore";
import { formatBytes } from "../lib/save";
import { Modal } from "./Modal";

/**
 * F10: shown when the open-time estimate says a file may exhaust memory if
 * loaded eagerly. Offers indexed read-only mode (bounded memory), a full
 * in-memory load anyway, or cancelling the open. Also renders the progress of
 * a running indexing job started from here.
 */
export function OpenModeDialog() {
  const decision = useStore((s) => s.openDecision);
  const indexing = useStore((s) => s.indexing);
  const dismiss = useStore((s) => s.dismissOpenDecision);
  const openEditable = useStore((s) => s.confirmOpenEditable);
  const openIndexed = useStore((s) => s.confirmOpenIndexed);
  const cancelIndexing = useStore((s) => s.cancelIndexing);

  // Progress phase: an indexed open (or an indexed reload) is running.
  if (indexing?.kind === "openIndexed" || indexing?.kind === "reindex") {
    const pct =
      indexing.total && indexing.total > 0
        ? Math.min(100, Math.round((indexing.processed / indexing.total) * 100))
        : null;
    return (
      <Modal
        title={indexing.kind === "reindex" ? "Reloading (re-indexing)…" : "Building index…"}
        onClose={() => void cancelIndexing()}
        footer={
          <button onClick={() => void cancelIndexing()} className={btnGhost}>
            Cancel
          </button>
        }
      >
        <div className="space-y-3 text-sm">
          <p
            className="truncate text-xs text-zinc-500 dark:text-zinc-400"
            title={indexing.path ?? ""}
          >
            {indexing.path}
          </p>
          <div className="h-2 overflow-hidden rounded bg-zinc-200 dark:bg-zinc-800">
            <div
              className="h-full rounded bg-violet-600 transition-all"
              style={{ width: `${pct ?? 5}%` }}
            />
          </div>
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {formatBytes(indexing.processed)}
            {indexing.total ? ` of ${formatBytes(indexing.total)}` : ""} scanned
            {pct !== null ? ` · ${pct}%` : ""}
          </p>
        </div>
      </Modal>
    );
  }

  if (!decision) return null;
  const { path, estimate } = decision;
  const fileName = path.split(/[\\/]/).pop() ?? path;

  return (
    <Modal
      title="This file is large"
      onClose={dismiss}
      footer={
        <>
          <button onClick={dismiss} className={btnGhost}>
            Cancel
          </button>
          <button onClick={() => void openEditable()} className={btnGhost}>
            Open editable in memory
          </button>
          <button onClick={() => void openIndexed()} className={btnPrimary}>
            Open read-only (low memory)
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <p className="truncate font-medium" title={path}>
          {fileName}
        </p>
        <dl className="grid grid-cols-2 gap-x-4 gap-y-1 rounded border border-zinc-200 px-3 py-2 text-xs dark:border-zinc-800">
          <dt className="text-zinc-500 dark:text-zinc-400">File size</dt>
          <dd>{formatBytes(estimate.fileSize)}</dd>
          <dt className="text-zinc-500 dark:text-zinc-400">Estimated rows</dt>
          <dd>~{estimate.estimatedRows.toLocaleString()}</dd>
          <dt className="text-zinc-500 dark:text-zinc-400">Estimated memory if editable</dt>
          <dd>~{formatBytes(estimate.estimatedMemory)}</dd>
          <dt className="text-zinc-500 dark:text-zinc-400">Encoding</dt>
          <dd>{estimate.encoding}</dd>
        </dl>
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          Read-only mode scans the file once and then streams rows on demand, so memory stays
          bounded no matter the size. Browsing, find, filter, export, diagnostics and profiling all
          work; editing needs “Convert to editable” later or the in-memory mode now.
        </p>
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary = "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500";
