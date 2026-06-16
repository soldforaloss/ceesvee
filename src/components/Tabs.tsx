import { ask } from "@tauri-apps/plugin-dialog";

import { useStore } from "../store/useStore";
import type { DocumentMeta } from "../types";
import { Close, Dot } from "./Icons";

export function Tabs() {
  const tabs = useStore((s) => s.tabs);
  const activeId = useStore((s) => s.activeId);
  const setActive = useStore((s) => s.setActive);
  const closeTab = useStore((s) => s.closeTab);

  if (tabs.length === 0) return null;

  const handleClose = async (e: React.MouseEvent, tab: DocumentMeta) => {
    e.stopPropagation();
    if (tab.dirty) {
      const ok = await ask(`Discard unsaved changes to “${tab.fileName}”?`, {
        title: "Unsaved changes",
        kind: "warning",
      });
      if (!ok) return;
    }
    void closeTab(tab.id);
  };

  return (
    <div className="flex h-9 shrink-0 items-stretch overflow-x-auto border-b border-zinc-200 bg-zinc-100 dark:border-zinc-800 dark:bg-zinc-900">
      {tabs.map((tab) => {
        const active = tab.id === activeId;
        return (
          <div
            key={tab.id}
            onClick={() => setActive(tab.id)}
            className={`group flex max-w-[220px] cursor-pointer items-center gap-1.5 border-r border-zinc-200 px-3 text-sm dark:border-zinc-800 ${
              active
                ? "bg-white text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100"
                : "text-zinc-500 hover:bg-zinc-200/60 dark:text-zinc-400 dark:hover:bg-zinc-800/60"
            }`}
          >
            {tab.dirty && <Dot className="h-2 w-2 shrink-0 text-violet-500" />}
            <span className="truncate" title={tab.path ?? tab.fileName}>
              {tab.fileName}
            </span>
            <button
              onClick={(e) => void handleClose(e, tab)}
              className="ml-0.5 shrink-0 rounded p-0.5 text-zinc-400 opacity-0 hover:bg-zinc-300 hover:text-zinc-700 group-hover:opacity-100 dark:hover:bg-zinc-700 dark:hover:text-zinc-200"
              title="Close"
            >
              <Close className="h-3 w-3" />
            </button>
          </div>
        );
      })}
    </div>
  );
}
