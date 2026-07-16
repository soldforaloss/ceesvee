import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useState } from "react";

import { AppendDialog } from "./components/AppendDialog";
import { ArchiveEntryDialog } from "./components/ArchiveEntryDialog";
import { CellEditorDialog } from "./components/CellEditorDialog";
import { ClusterDialog } from "./components/ClusterDialog";
import { ColumnExplorerPanel } from "./components/ColumnExplorerPanel";
import { CommandPalette } from "./components/CommandPalette";
import { CompareDialog } from "./components/CompareDialog";
import { CopyAsDialog } from "./components/CopyAsDialog";
import { CrossValDialog } from "./components/CrossValDialog";
import { PasteSpecialDialog } from "./components/PasteSpecialDialog";
import { DedupDialog } from "./components/DedupDialog";
import { DiagnosticsPanel } from "./components/DiagnosticsPanel";
import { EmptyState } from "./components/EmptyState";
import { EncodingIssuesDialog } from "./components/EncodingIssuesDialog";
import { ExportDialog } from "./components/ExportDialog";
import { ExternalChangeDialog } from "./components/ExternalChangeDialog";
import { FilterDialog } from "./components/FilterDialog";
import { FindBar } from "./components/FindBar";
import { Grid } from "./components/Grid";
import { GroupByDialog } from "./components/GroupByDialog";
import { Close } from "./components/Icons";
import { JoinDialog } from "./components/JoinDialog";
import { ProfilesDialog } from "./components/ProfilesDialog";
import { ProfileSuggestionBar } from "./components/ProfileSuggestionBar";
import { OpenModeDialog } from "./components/OpenModeDialog";
import { OutlierDialog } from "./components/OutlierDialog";
import { QuitDialog } from "./components/QuitDialog";
import { RecipeDialog } from "./components/RecipeDialog";
import { ReopenDialog } from "./components/ReopenDialog";
import { RepairDialog } from "./components/RepairDialog";
import { ReshapeDialog } from "./components/ReshapeDialog";
import { SemanticDialog } from "./components/SemanticDialog";
import { ShortcutsDialog } from "./components/ShortcutsDialog";
import { SortDialog } from "./components/SortDialog";
import { SourceBar } from "./components/SourceBar";
import { StatusBar } from "./components/StatusBar";
import { SummaryPanel } from "./components/SummaryPanel";
import { Tabs } from "./components/Tabs";
import { Toolbar } from "./components/Toolbar";
import { TransformDialog } from "./components/TransformDialog";
import { registry } from "./lib/commands";
import { registerAppCommands } from "./lib/commandDefs";
import { onJobFinished, onJobProgress } from "./lib/jobs";
import { bindingFromEvent, effectiveBindings } from "./lib/shortcuts";
import * as api from "./lib/tauri";
import { checkForUpdates } from "./lib/updater";
import { useActiveMeta, useStore } from "./store/useStore";

registerAppCommands();

export default function App() {
  const meta = useActiveMeta();
  const dataVersion = useStore((s) => s.dataVersion);
  const theme = useStore((s) => s.theme);
  const error = useStore((s) => s.error);
  const setError = useStore((s) => s.setError);

  const activeModal = useStore((s) => s.activeModal);
  const setModal = useStore((s) => s.setModal);
  const diagnosticsOpen = useStore((s) => s.diagnosticsOpen);
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  const [dragOver, setDragOver] = useState(false);

  // Open a list of file paths sequentially through the store's open flow.
  const openPaths = useCallback(async (paths: string[]) => {
    for (const path of paths) await useStore.getState().openPath(path);
  }, []);

  // Initialise persisted state (recent files, theme) once.
  useEffect(() => {
    useStore.getState().init();
  }, []);

  // Check for a newer release once at launch (no-op in dev).
  useEffect(() => {
    void checkForUpdates();
  }, []);

  // Route background-job progress/completion events into the store.
  useEffect(() => {
    let unProgress: UnlistenFn | undefined;
    let unFinished: UnlistenFn | undefined;
    void onJobProgress((p) => useStore.getState().handleJobProgress(p))
      .then((fn) => {
        unProgress = fn;
      })
      .catch(() => undefined);
    void onJobFinished((f) => void useStore.getState().handleJobFinished(f))
      .then((fn) => {
        unFinished = fn;
      })
      .catch(() => undefined);
    return () => {
      unProgress?.();
      unFinished?.();
    };
  }, []);

  // Intercept the window close: with unsaved edits, quitting must go through
  // Save all / Discard all / Cancel (the QuitDialog destroys the window).
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    void getCurrentWindow()
      .onCloseRequested((event) => {
        const s = useStore.getState();
        if (s.tabs.some((t) => t.dirty)) {
          event.preventDefault();
          s.setQuitPromptOpen(true);
        }
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => undefined);
    return () => unlisten?.();
  }, []);

  // Detect files modified outside CEESVEE whenever the window regains focus.
  useEffect(() => {
    const onFocus = () => void useStore.getState().checkExternalChanges();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);

  // Open files passed via "Open with CEESVEE": at launch (cold start, drained
  // from the backend) and while running (warm start, forwarded by the
  // single-instance plugin / macOS).
  useEffect(() => {
    void api
      .takePendingFiles()
      .then(openPaths)
      .catch(() => undefined);

    let unlisten: UnlistenFn | undefined;
    void listen<string[]>("open-files", (event) => void openPaths(event.payload))
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => undefined);

    return () => unlisten?.();
  }, [openPaths]);

  // Drag a file from the OS onto the window to open it. We must use Tauri's
  // webview drag-drop event: on Windows the OS webview intercepts file drops,
  // so an HTML5 ondrop handler never fires with usable absolute paths.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "enter" || payload.type === "over") {
          setDragOver(true);
        } else if (payload.type === "leave") {
          setDragOver(false);
        } else if (payload.type === "drop") {
          setDragOver(false);
          void openPaths(payload.paths);
        }
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => undefined);

    return () => unlisten?.();
  }, [openPaths]);

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

  // Global keyboard shortcuts, resolved through the command registry (F11).
  // Bindings are recomputed per keypress from live settings, so shortcut
  // edits take effect immediately without a restart. Capture phase so chords
  // the grid would otherwise swallow (F2) reach the registry first; the
  // editable-target guard keeps typing in inputs unaffected.
  useEffect(() => {
    const isEditableTarget = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      return (
        !!target &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)
      );
    };
    const runCommand = (e: KeyboardEvent, commandId: string): boolean => {
      const command = registry.byId(commandId);
      if (!command) return false;
      if (isEditableTarget(e) && !command.allowInEditable) return true; // handled: ignore
      if (command.unavailableReason?.()) return true; // bound but not runnable now
      e.preventDefault();
      e.stopPropagation();
      command.run();
      return true;
    };
    const onKey = (e: KeyboardEvent) => {
      const binding = bindingFromEvent(e);
      if (!binding) return;
      const overrides = useStore.getState().settings?.shortcutOverrides;
      const bindings = effectiveBindings(registry.defaultBindings(), overrides);
      // Primary (rebindable) chords first…
      for (const [commandId, bound] of bindings) {
        if (bound === binding && runCommand(e, commandId)) return;
      }
      // …then fixed aliases (e.g. mod+enter for the cell editor).
      for (const command of registry.staticCommands()) {
        if (command.extraShortcuts?.includes(binding) && runCommand(e, command.id)) return;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  return (
    <div className="flex h-full flex-col bg-white text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <Toolbar />
      <Tabs />
      <SourceBar />
      <ProfileSuggestionBar />
      <FindBar />

      <main className="relative flex min-h-0 flex-1">
        <div className="relative min-w-0 flex-1">
          {meta ? <Grid meta={meta} dataVersion={dataVersion} dark={dark} /> : <EmptyState />}
          {dragOver && (
            <div className="pointer-events-none absolute inset-0 z-40 flex items-center justify-center bg-violet-500/10 backdrop-blur-[1px]">
              <div className="rounded-xl border-2 border-dashed border-violet-400 bg-white/85 px-6 py-4 text-sm font-medium text-violet-700 shadow-lg dark:bg-zinc-900/85 dark:text-violet-300">
                Drop to open
              </div>
            </div>
          )}
        </div>
        {diagnosticsOpen && meta && <DiagnosticsPanel />}
        {meta && <ColumnExplorerPanel />}
      </main>

      <StatusBar />

      {activeModal === "sort" && <SortDialog onClose={() => setModal(null)} />}
      {activeModal === "export" && <ExportDialog onClose={() => setModal(null)} />}
      {activeModal === "summaries" && <SummaryPanel onClose={() => setModal(null)} />}
      {activeModal === "filter" && <FilterDialog onClose={() => setModal(null)} />}
      {activeModal === "profiles" && <ProfilesDialog onClose={() => setModal(null)} />}
      {activeModal === "transform" && <TransformDialog onClose={() => setModal(null)} />}
      {activeModal === "dedup" && (
        <DedupDialog
          onClose={() => setModal(null)}
          onExportDuplicates={() => {
            // The duplicate filter has been applied; export the visible rows.
            setModal("export");
          }}
        />
      )}
      {activeModal === "compare" && <CompareDialog onClose={() => setModal(null)} />}
      {activeModal === "shortcuts" && <ShortcutsDialog onClose={() => setModal(null)} />}
      {activeModal === "copyAs" && <CopyAsDialog onClose={() => setModal(null)} />}
      {activeModal === "cluster" && <ClusterDialog onClose={() => setModal(null)} />}
      {activeModal === "semantic" && <SemanticDialog onClose={() => setModal(null)} />}
      {activeModal === "crossval" && <CrossValDialog onClose={() => setModal(null)} />}
      {activeModal === "repair" && <RepairDialog onClose={() => setModal(null)} />}
      {activeModal === "outlier" && <OutlierDialog onClose={() => setModal(null)} />}
      {activeModal === "append" && <AppendDialog onClose={() => setModal(null)} />}
      {activeModal === "join" && <JoinDialog onClose={() => setModal(null)} />}
      {activeModal === "groupBy" && <GroupByDialog onClose={() => setModal(null)} />}
      {activeModal === "reshape" && <ReshapeDialog onClose={() => setModal(null)} />}
      {activeModal === "recipes" && <RecipeDialog onClose={() => setModal(null)} />}
      {activeModal === "pasteSpecial" && <PasteSpecialDialog onClose={() => setModal(null)} />}
      <CommandPalette />
      <CellEditorDialog />
      <ReopenDialog />
      <ExternalChangeDialog />
      <OpenModeDialog />
      <ArchiveEntryDialog />
      <QuitDialog />
      <EncodingIssuesDialog />

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
