import {
  buildResolutions,
  canApply,
  defaultChoice,
  hasBlockingSources,
  statusDisplay,
  type SourceAction,
  type SourceChoice,
} from "../lib/project";
import { useStore } from "../store/useStore";
import type { SourcePreviewEntry } from "../types";
import { Modal } from "./Modal";

/**
 * Project open flow (F37): a per-source status list where each referenced file
 * can be opened from its saved path, relinked to a replacement, left out
 * ("open available only"), or removed from the project. Missing sources never
 * block opening the rest; cancelling the dialog cancels the whole open and
 * leaves any current project untouched. Nothing here runs recipes or exports.
 */
export function ProjectOpenDialog() {
  const preview = useStore((s) => s.projectOpen);
  const choices = useStore((s) => s.projectOpenChoices);
  const setChoice = useStore((s) => s.setProjectChoice);
  const locate = useStore((s) => s.projectLocateSource);
  const cancel = useStore((s) => s.cancelProjectOpen);
  const apply = useStore((s) => s.applyProjectOpen);
  const openAvailableOnly = useStore((s) => s.projectOpenAvailableOnly);

  if (!preview) return null;

  const ready = canApply(preview.sources, choices);
  const blocking = hasBlockingSources(preview.sources);
  const projectName = preview.path.split(/[\\/]/).pop() ?? preview.path;
  // What each source will actually do once applied (for the summary line).
  const resolutions = buildResolutions(preview.sources, choices);
  const willOpen = resolutions.filter((r) => r.action === "open" || r.action === "locate").length;
  const willSkip = resolutions.filter((r) => r.action === "skip").length;
  const willRemove = resolutions.filter((r) => r.action === "remove").length;

  return (
    <Modal
      title={`Open project — ${projectName}`}
      onClose={cancel}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-[11px] text-zinc-500 dark:text-zinc-400">
            {willOpen} open · {willSkip} left out · {willRemove} removed
          </span>
          <button onClick={cancel} className={btnGhost}>
            Cancel
          </button>
          {blocking && (
            <button onClick={() => void openAvailableOnly()} className={btnGhost}>
              Open available only
            </button>
          )}
          <button
            onClick={() => void apply()}
            disabled={!ready}
            className={ready ? btnPrimary : btnDisabled}
          >
            Open project
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          Format {preview.formatVersion} · saved by CEESVEE {preview.appVersion}. Opening restores
          the documents, tab order and layout. Saved views reapply only when a file still matches
          what the project captured — nothing is run automatically.
        </p>

        <div className="max-h-[52vh] space-y-2 overflow-y-auto pr-1">
          {preview.sources.length === 0 && (
            <p className="py-6 text-center text-xs text-zinc-400">
              This project references no documents.
            </p>
          )}
          {preview.sources.map((entry) => (
            <SourceRow
              key={entry.sourceId}
              entry={entry}
              choice={choices[entry.sourceId] ?? defaultChoice(entry)}
              onAction={(action) => {
                if (action === "locate") void locate(entry.sourceId);
                else setChoice(entry.sourceId, { action });
              }}
            />
          ))}
        </div>
      </div>
    </Modal>
  );
}

function SourceRow({
  entry,
  choice,
  onAction,
}: {
  entry: SourcePreviewEntry;
  choice: SourceChoice;
  onAction: (action: SourceAction) => void;
}) {
  const status = statusDisplay(entry.status);
  const tone =
    status.tone === "ok"
      ? "bg-emerald-100 text-emerald-800 dark:bg-emerald-500/15 dark:text-emerald-300"
      : status.tone === "warn"
        ? "bg-amber-100 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300"
        : "bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300";

  const located = choice.action === "locate" ? choice.locatePath : null;

  return (
    <div className="rounded border border-zinc-200 p-2 dark:border-zinc-800">
      <div className="flex items-center gap-2">
        <span className={`rounded px-1.5 py-0.5 text-[11px] font-medium ${tone}`}>
          {status.label}
        </span>
        <span className="truncate font-medium" title={entry.resolvedPath}>
          {entry.displayName ?? entry.resolvedPath}
        </span>
      </div>
      <p className="mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">{status.hint}</p>
      {located && (
        <p
          className="mt-1 truncate text-[11px] text-violet-600 dark:text-violet-300"
          title={located}
        >
          → relinking to {located}
        </p>
      )}

      <div className="mt-1.5 flex flex-wrap gap-1.5">
        <ActionChip
          label="Open"
          active={choice.action === "open"}
          disabled={entry.status === "missing" || entry.status === "movedCandidate"}
          onClick={() => onAction("open")}
        />
        <ActionChip
          label={entry.status === "movedCandidate" && located ? "Relink" : "Locate…"}
          active={choice.action === "locate"}
          onClick={() => onAction("locate")}
        />
        <ActionChip
          label="Leave out"
          active={choice.action === "skip"}
          onClick={() => onAction("skip")}
        />
        <ActionChip
          label="Remove"
          active={choice.action === "remove"}
          danger
          onClick={() => onAction("remove")}
        />
      </div>
    </div>
  );
}

function ActionChip({
  label,
  active,
  disabled,
  danger,
  onClick,
}: {
  label: string;
  active: boolean;
  disabled?: boolean;
  danger?: boolean;
  onClick: () => void;
}) {
  const base = "rounded border px-2 py-0.5 text-[11px] disabled:cursor-default disabled:opacity-30";
  const style = active
    ? danger
      ? "border-red-400 bg-red-50 text-red-700 dark:border-red-500/40 dark:bg-red-500/10 dark:text-red-300"
      : "border-violet-400 bg-violet-50 text-violet-700 dark:border-violet-500/40 dark:bg-violet-500/15 dark:text-violet-200"
    : "border-zinc-200 text-zinc-600 hover:border-violet-400 dark:border-zinc-700 dark:text-zinc-300";
  return (
    <button onClick={onClick} disabled={disabled} className={`${base} ${style}`}>
      {label}
    </button>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary = "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500";
const btnDisabled = "rounded bg-zinc-300 px-3 py-1.5 text-sm text-white dark:bg-zinc-700";
