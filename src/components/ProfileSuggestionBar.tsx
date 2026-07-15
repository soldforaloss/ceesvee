import { useStore } from "../store/useStore";

/**
 * Slim banner shown when a saved file profile matches the active document but
 * its parse settings differ (F08). Applying goes through the previewed reopen
 * flow, so dirty documents keep their Save/Discard/Cancel protection.
 */
export function ProfileSuggestionBar() {
  const suggestion = useStore((s) =>
    s.profileSuggestion && s.profileSuggestion.docId === s.activeId ? s.profileSuggestion : null,
  );
  const applyProfile = useStore((s) => s.applyProfile);
  const dismiss = useStore((s) => s.dismissProfileSuggestion);

  if (!suggestion) return null;

  return (
    <div className="flex h-8 shrink-0 items-center gap-2 border-b border-violet-200 bg-violet-50 px-3 text-xs text-violet-800 dark:border-violet-500/30 dark:bg-violet-500/10 dark:text-violet-200">
      <span>
        File profile <span className="font-semibold">“{suggestion.profile.name}”</span> matches this
        file with different settings.
      </span>
      <button
        onClick={() => applyProfile(suggestion.profile)}
        className="rounded bg-violet-600 px-2 py-0.5 font-medium text-white hover:bg-violet-500"
      >
        Preview & apply
      </button>
      <button
        onClick={dismiss}
        className="rounded px-2 py-0.5 hover:bg-violet-100 dark:hover:bg-violet-500/20"
      >
        Dismiss
      </button>
    </div>
  );
}
