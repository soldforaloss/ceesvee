import { useStore } from "../store/useStore";

/**
 * Recoverable named-view warning (F12): shown when an applied view references
 * columns that no longer exist. The view itself is never modified.
 */
export function ViewWarningBar() {
  const viewWarning = useStore((s) => s.viewWarning);
  const dismissViewWarning = useStore((s) => s.dismissViewWarning);
  const setModal = useStore((s) => s.setModal);

  if (!viewWarning) return null;

  return (
    <div className="flex items-center gap-3 border-b border-amber-300 bg-amber-50 px-3 py-1.5 text-xs text-amber-800 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-200">
      <span className="min-w-0 flex-1 truncate" title={viewWarning}>
        {viewWarning}
      </span>
      <button onClick={() => setModal("views")} className="shrink-0 underline">
        Manage views
      </button>
      <button onClick={dismissViewWarning} className="shrink-0 underline">
        Dismiss
      </button>
    </div>
  );
}
