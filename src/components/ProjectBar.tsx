import { useState } from "react";

import { useProjectDirty, useStore } from "../store/useStore";
import { Close, Dot, Layers, Save } from "./Icons";

/**
 * Thin strip shown while a project workspace is open (F37): the project name, a
 * dirty dot, quick Save/Close actions, and a banner listing any sources whose
 * saved views were gated on the last open (warn, never break).
 */
export function ProjectBar() {
  const project = useStore((s) => s.project);
  const dirty = useProjectDirty();
  const warnings = useStore((s) => s.projectWarnings);
  const projectSave = useStore((s) => s.projectSave);
  const requestClose = useStore((s) => s.requestCloseProject);
  const dismissWarnings = useStore((s) => s.dismissProjectWarnings);
  const [showWarnings, setShowWarnings] = useState(false);

  if (!project) return null;

  return (
    <div className="flex h-8 shrink-0 items-center gap-2 border-b border-violet-200 bg-violet-50 px-3 text-xs text-violet-900 dark:border-violet-500/25 dark:bg-violet-500/10 dark:text-violet-200">
      <Layers className="h-3.5 w-3.5 shrink-0 text-violet-500 dark:text-violet-400" />
      <span className="truncate font-medium" title={project.path ?? "Unsaved project"}>
        {project.name}
      </span>
      {dirty && (
        <span title="Unsaved project changes" className="flex shrink-0">
          <Dot className="h-2 w-2 text-violet-500" />
        </span>
      )}
      {!project.path && <span className="text-violet-400 dark:text-violet-500/70">(unsaved)</span>}

      {warnings.length > 0 && (
        <div className="relative">
          <button
            onClick={() => setShowWarnings((o) => !o)}
            className="rounded bg-amber-100 px-1.5 py-0.5 text-[11px] font-medium text-amber-800 hover:bg-amber-200 dark:bg-amber-500/15 dark:text-amber-300 dark:hover:bg-amber-500/25"
            title="Some saved views were not reapplied"
          >
            {warnings.length} view {warnings.length === 1 ? "warning" : "warnings"}
          </button>
          {showWarnings && (
            <div
              className="absolute left-0 top-7 z-40 w-96 rounded-lg border border-zinc-200 bg-white p-2 text-[11px] text-zinc-700 shadow-xl dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200"
              onMouseLeave={() => setShowWarnings(false)}
            >
              <div className="mb-1 flex items-center justify-between">
                <span className="font-semibold">Saved views not reapplied</span>
                <button
                  onClick={() => {
                    setShowWarnings(false);
                    dismissWarnings();
                  }}
                  className="rounded p-0.5 hover:bg-zinc-100 dark:hover:bg-zinc-700"
                  title="Dismiss"
                >
                  <Close className="h-3 w-3" />
                </button>
              </div>
              <ul className="max-h-56 space-y-1.5 overflow-y-auto">
                {warnings.map((w) => (
                  <li key={w.sourceId}>
                    <div className="font-medium">{w.name}</div>
                    <ul className="ml-3 list-disc text-zinc-500 dark:text-zinc-400">
                      {w.warnings.map((msg, i) => (
                        <li key={i}>{msg}</li>
                      ))}
                    </ul>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      <div className="ml-auto flex items-center gap-1">
        <button
          onClick={() => void projectSave(false)}
          className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-violet-100 disabled:opacity-40 dark:hover:bg-violet-500/20"
          disabled={!dirty && !!project.path}
          title={project.path ? "Save project (config only, no data)" : "Save project…"}
        >
          <Save className="h-3.5 w-3.5" />
          Save
        </button>
        <button
          onClick={requestClose}
          className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-violet-100 dark:hover:bg-violet-500/20"
          title="Close project (documents stay open)"
        >
          <Close className="h-3.5 w-3.5" />
          Close
        </button>
      </div>
    </div>
  );
}
