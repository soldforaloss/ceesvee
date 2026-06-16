import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

import { EmptyState } from "./components/EmptyState";
import { ExportDialog } from "./components/ExportDialog";
import { FindBar } from "./components/FindBar";
import { Grid } from "./components/Grid";
import { Close } from "./components/Icons";
import { SortDialog } from "./components/SortDialog";
import { SourceBar } from "./components/SourceBar";
import { StatusBar } from "./components/StatusBar";
import { Tabs } from "./components/Tabs";
import { Toolbar } from "./components/Toolbar";
import * as api from "./lib/tauri";
import { checkForUpdates } from "./lib/updater";
import { useActiveMeta, useStore } from "./store/useStore";

export default function App() {
  const meta = useActiveMeta();
  const dataVersion = useStore((s) => s.dataVersion);
  const theme = useStore((s) => s.theme);
  const error = useStore((s) => s.error);
  const setError = useStore((s) => s.setError);

  const [sortOpen, setSortOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));

  // Initialise persisted state (recent files, theme) once.
  useEffect(() => {
    useStore.getState().init();
  }, []);

  // Check for a newer release once at launch (no-op in dev).
  useEffect(() => {
    void checkForUpdates();
  }, []);

  // Open files passed via "Open with CEESVEE": at launch (cold start, drained
  // from the backend) and while running (warm start, forwarded by the
  // single-instance plugin / macOS).
  useEffect(() => {
    const open = async (paths: string[]) => {
      for (const path of paths) await useStore.getState().openPath(path);
    };
    void api
      .takePendingFiles()
      .then(open)
      .catch(() => undefined);

    let unlisten: UnlistenFn | undefined;
    void listen<string[]>("open-files", (event) => void open(event.payload))
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => undefined);

    return () => unlisten?.();
  }, []);

  // Track effective dark mode for the grid theme.
  useEffect(() => {
    const update = () => setDark(document.documentElement.classList.contains("dark"));
    update();
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", update);
    return () => mq.removeEventListener("change", update);
  }, [theme]);

  // Auto-dismiss errors.
  useEffect(() => {
    if (!error) return;
    const handle = setTimeout(() => setError(null), 6000);
    return () => clearTimeout(handle);
  }, [error, setError]);

  // Global keyboard shortcuts.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const target = e.target as HTMLElement | null;
      const editable =
        !!target &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
      const s = useStore.getState();

      switch (e.key.toLowerCase()) {
        case "o":
          e.preventDefault();
          void s.openDialog();
          break;
        case "n":
          e.preventDefault();
          void s.newDoc();
          break;
        case "s":
          e.preventDefault();
          void s.saveActive(e.shiftKey);
          break;
        case "f":
          e.preventDefault();
          s.setFindOpen(true);
          break;
        case "z":
          if (editable) return;
          e.preventDefault();
          if (e.shiftKey) void s.redo();
          else void s.undo();
          break;
        case "y":
          if (editable) return;
          e.preventDefault();
          void s.redo();
          break;
        default:
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="flex h-full flex-col bg-white text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <Toolbar onSort={() => setSortOpen(true)} onExport={() => setExportOpen(true)} />
      <Tabs />
      <SourceBar />
      <FindBar />

      <main className="relative min-h-0 flex-1">
        {meta ? <Grid meta={meta} dataVersion={dataVersion} dark={dark} /> : <EmptyState />}
      </main>

      <StatusBar />

      {sortOpen && <SortDialog onClose={() => setSortOpen(false)} />}
      {exportOpen && <ExportDialog onClose={() => setExportOpen(false)} />}

      {error && (
        <div className="fixed bottom-10 right-4 z-50 flex max-w-md items-start gap-2 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700 shadow-lg dark:border-red-900/60 dark:bg-red-950/80 dark:text-red-300">
          <span className="flex-1">{error}</span>
          <button
            onClick={() => setError(null)}
            className="shrink-0 rounded p-0.5 hover:bg-red-100 dark:hover:bg-red-900/60"
          >
            <Close className="h-4 w-4" />
          </button>
        </div>
      )}
    </div>
  );
}
