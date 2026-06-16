import { useStore } from "../store/useStore";
import { FilePlus, FolderOpen } from "./Icons";

export function EmptyState() {
  const openDialog = useStore((s) => s.openDialog);
  const openPath = useStore((s) => s.openPath);
  const newDoc = useStore((s) => s.newDoc);
  const recent = useStore((s) => s.recent);

  return (
    <div className="flex h-full flex-col items-center justify-center gap-6 px-6 text-center">
      <div>
        <h1 className="bg-gradient-to-br from-violet-500 to-indigo-500 bg-clip-text text-4xl font-bold tracking-tight text-transparent">
          CEESVEE
        </h1>
        <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">
          A fast, no-nonsense CSV editor.
        </p>
      </div>

      <div className="flex gap-3">
        <button
          onClick={() => void openDialog()}
          className="flex items-center gap-2 rounded-lg bg-violet-600 px-4 py-2 text-sm font-medium text-white shadow-sm hover:bg-violet-500"
        >
          <FolderOpen className="h-4 w-4" />
          Open file
        </button>
        <button
          onClick={() => void newDoc()}
          className="flex items-center gap-2 rounded-lg border border-zinc-300 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
        >
          <FilePlus className="h-4 w-4" />
          New file
        </button>
      </div>

      {recent.length > 0 && (
        <div className="w-full max-w-md text-left">
          <p className="mb-1 px-2 text-xs font-medium uppercase tracking-wide text-zinc-400">
            Recent
          </p>
          <div className="overflow-hidden rounded-lg border border-zinc-200 dark:border-zinc-800">
            {recent.slice(0, 6).map((path) => (
              <button
                key={path}
                onClick={() => void openPath(path)}
                className="block w-full truncate border-b border-zinc-100 px-3 py-2 text-left text-sm text-zinc-600 last:border-0 hover:bg-zinc-50 dark:border-zinc-800/60 dark:text-zinc-300 dark:hover:bg-zinc-900"
                dir="rtl"
                title={path}
              >
                {path}
              </button>
            ))}
          </div>
        </div>
      )}

      <p className="text-xs text-zinc-400 dark:text-zinc-600">
        Tip: drag isn’t needed — use Open, or your recent files above.
      </p>
    </div>
  );
}
