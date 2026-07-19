// F37 project workspaces: pure, side-effect-free helpers shared by the store,
// the project bar, and the open dialog. Everything here is deterministic so it
// can be unit-tested without a backend — status mapping, per-source resolution
// building, source/tab section capture, and project dirty-state derivation.

import type {
  DocumentMeta,
  FileFingerprint,
  NamedView,
  PlanEntry,
  ProjectLayoutSection,
  ProjectSource,
  ProjectTabsSection,
  ResolutionEntry,
  SourcePreviewEntry,
  SourceStatus,
} from "../types";

// ----- panel layout + snapshot ------------------------------------------------

/** The front-end-owned panel layout a project persists (config only). */
export interface PanelLayout {
  diagnostics: boolean;
  explorer: boolean;
  changes: boolean;
}

/**
 * The slice of UI state a project tracks, used only to DERIVE dirty state
 * (the authoritative persisted state is the backend's). Documents are keyed by
 * path (a project references files, never unsaved buffers).
 */
export interface ProjectSnapshot {
  /** Open document paths in tab order. */
  order: string[];
  /** Active document path, or null. */
  active: string | null;
  layout: PanelLayout;
  /**
   * Each open document's active named-view id, keyed by normalized path. Part
   * of the snapshot so applying/switching a view marks the project dirty (the
   * `views` section is captured on save).
   */
  activeViews: Record<string, string | null>;
}

/**
 * Whether the host filesystem compares paths case-INSENSITIVELY. Windows and
 * the default macOS volume do; Linux — which the app also ships for — does not,
 * where `/data/A.csv` and `/data/a.csv` are genuinely different files. Detected
 * once from the webview's platform (see `detectCaseSensitivePaths`), overridable
 * via `setCaseSensitivePaths` for an explicit startup probe or from tests.
 */
let caseSensitivePaths = detectCaseSensitivePaths();

function detectCaseSensitivePaths(): boolean {
  // WebKitGTK / WebView2 / WKWebView all name the OS in the UA string; only
  // Linux is case-sensitive. Anything without a recognizable UA (the Node test
  // runner) defaults to case-insensitive, preserving the historical behavior.
  const ua = typeof navigator !== "undefined" ? (navigator.userAgent ?? "") : "";
  return /linux/i.test(ua);
}

/**
 * Override the detected filesystem case sensitivity. `true` compares paths
 * exactly (case-sensitive filesystems, e.g. Linux); `false` folds case
 * (Windows / macOS). Exposed for an authoritative startup probe and for tests.
 */
export function setCaseSensitivePaths(sensitive: boolean): void {
  caseSensitivePaths = sensitive;
}

/**
 * Normalize a filesystem path for identity comparison: `\` and `/` are treated
 * alike and trailing separators dropped. Case is folded ONLY on case-insensitive
 * filesystems — lowercasing everywhere would collapse distinct files on Linux
 * (`A.csv` vs `a.csv`), corrupting source ids, tab order and per-file view state.
 * Used to match open documents against saved sources.
 */
export function pathKey(path: string): string {
  const normalized = path.replace(/[\\/]+/g, "/").replace(/\/+$/, "");
  return caseSensitivePaths ? normalized : normalized.toLowerCase();
}

/**
 * Capture the current project-relevant UI state (pure). `activeViewByTab` maps
 * a tab id to the named-view id active for that document (including the active
 * tab); omit it (or leave entries unset) when no view is active.
 */
export function projectSnapshot(
  tabs: Pick<DocumentMeta, "id" | "path">[],
  activeId: number | null,
  panels: PanelLayout,
  activeViewByTab: Record<number, string | null> = {},
): ProjectSnapshot {
  const order = tabs.map((t) => t.path).filter((p): p is string => p != null);
  const active = tabs.find((t) => t.id === activeId)?.path ?? null;
  const activeViews: Record<string, string | null> = {};
  for (const t of tabs) {
    if (t.path) activeViews[pathKey(t.path)] = activeViewByTab[t.id] ?? null;
  }
  return { order, active, layout: { ...panels }, activeViews };
}

export function panelLayoutsEqual(a: PanelLayout, b: PanelLayout): boolean {
  return a.diagnostics === b.diagnostics && a.explorer === b.explorer && a.changes === b.changes;
}

/** Whether two path→activeViewId maps agree (keys are already normalized). */
function activeViewsEqual(
  a: Record<string, string | null>,
  b: Record<string, string | null>,
): boolean {
  const keys = Object.keys(a);
  if (keys.length !== Object.keys(b).length) return false;
  return keys.every((k) => a[k] === b[k]);
}

export function projectSnapshotsEqual(a: ProjectSnapshot, b: ProjectSnapshot): boolean {
  const sameActive =
    a.active === b.active ||
    (a.active != null && b.active != null && pathKey(a.active) === pathKey(b.active));
  return (
    sameActive &&
    a.order.length === b.order.length &&
    a.order.every((p, i) => pathKey(p) === pathKey(b.order[i])) &&
    panelLayoutsEqual(a.layout, b.layout) &&
    activeViewsEqual(a.activeViews, b.activeViews)
  );
}

/**
 * Whether the open project has unsaved changes: true when the live snapshot has
 * drifted from the one captured at the last save/open. With no baseline (no
 * project open) nothing is dirty.
 */
export function deriveProjectDirty(
  baseline: ProjectSnapshot | null,
  current: ProjectSnapshot,
): boolean {
  if (!baseline) return false;
  return !projectSnapshotsEqual(baseline, current);
}

/**
 * Whether the open project has unsaved changes, combining BOTH sources of truth.
 * The backend owns its own dirty flag: applying a relink or removal while opening
 * a project (`project_open_apply` with a `Locate`/`Remove` resolution) mutates
 * the persisted content, so `plan.meta.dirty` is already true before the user
 * touches anything. The snapshot comparison cannot see that — the baseline is
 * captured immediately after opening — so it must be OR-ed with the backend flag,
 * otherwise a relink/removal is treated as clean and silently lost on close/quit.
 */
export function projectDirty(
  backendDirty: boolean,
  baseline: ProjectSnapshot | null,
  current: ProjectSnapshot,
): boolean {
  return backendDirty || deriveProjectDirty(baseline, current);
}

// ----- open-dialog status mapping ---------------------------------------------

export interface StatusDisplay {
  label: string;
  tone: "ok" | "warn" | "error";
  /** One-line explanation for the dialog row. */
  hint: string;
}

/** Human-readable label, tone and hint for a source's open status. */
export function statusDisplay(status: SourceStatus): StatusDisplay {
  switch (status) {
    case "ok":
      return {
        label: "Ready",
        tone: "ok",
        hint: "File found and unchanged since the project was saved.",
      };
    case "missing":
      return {
        label: "Missing",
        tone: "error",
        hint: "The file is not at its saved location. Locate a replacement, or leave it out.",
      };
    case "movedCandidate":
      return {
        label: "Moved?",
        tone: "warn",
        hint: "The file moved, but a matching one was found nearby — relink it or locate another.",
      };
    case "changedFingerprint":
      return {
        label: "Changed",
        tone: "warn",
        hint: "The file changed since the project was saved; saved views won't be reapplied automatically.",
      };
    case "schemaIncompatible":
      return {
        label: "Columns changed",
        tone: "warn",
        hint: "The file's columns no longer match; saved views and schemas won't be reapplied.",
      };
  }
}

/** Whether the file exists at its stored path (so opening in place is valid). */
export function canOpenInPlace(status: SourceStatus): boolean {
  return status === "ok" || status === "changedFingerprint" || status === "schemaIncompatible";
}

// ----- per-source resolution --------------------------------------------------

export type SourceAction = "open" | "locate" | "skip" | "remove";

/** The user's per-source choice in the open dialog. */
export interface SourceChoice {
  action: SourceAction;
  /** Absolute path chosen for a `locate` action. */
  locatePath?: string;
}

/**
 * The choice a source starts with: open when the file is present, relink to a
 * found candidate when one moved, otherwise leave the source out (missing files
 * never block opening the rest).
 */
export function defaultChoice(entry: SourcePreviewEntry): SourceChoice {
  if (entry.status === "movedCandidate" && entry.movedCandidate) {
    return { action: "locate", locatePath: entry.movedCandidate };
  }
  if (canOpenInPlace(entry.status)) return { action: "open" };
  return { action: "skip" };
}

/** Whether a chosen action can actually be applied (no missing pieces). */
export function choiceValid(entry: SourcePreviewEntry, choice: SourceChoice): boolean {
  switch (choice.action) {
    case "open":
      return canOpenInPlace(entry.status);
    case "locate":
      return !!choice.locatePath;
    case "skip":
    case "remove":
      return true;
  }
}

/** Translate one choice into the backend resolution DTO. */
export function resolutionFor(entry: SourcePreviewEntry, choice: SourceChoice): ResolutionEntry {
  switch (choice.action) {
    case "locate":
      return { sourceId: entry.sourceId, action: "locate", path: choice.locatePath ?? "" };
    case "skip":
      return { sourceId: entry.sourceId, action: "skip" };
    case "remove":
      return { sourceId: entry.sourceId, action: "remove" };
    case "open":
      return { sourceId: entry.sourceId, action: "open" };
  }
}

/** Build the full resolution list, filling unset entries with their defaults. */
export function buildResolutions(
  entries: SourcePreviewEntry[],
  choices: Record<string, SourceChoice>,
): ResolutionEntry[] {
  return entries.map((e) => resolutionFor(e, choices[e.sourceId] ?? defaultChoice(e)));
}

/** Whether every source's current choice is actionable (enables "Open"). */
export function canApply(
  entries: SourcePreviewEntry[],
  choices: Record<string, SourceChoice>,
): boolean {
  return entries.every((e) => choiceValid(e, choices[e.sourceId] ?? defaultChoice(e)));
}

/** "Open available only": open every present file, leave the rest out. */
export function availableOnlyChoices(entries: SourcePreviewEntry[]): Record<string, SourceChoice> {
  const out: Record<string, SourceChoice> = {};
  for (const e of entries) {
    out[e.sourceId] = canOpenInPlace(e.status) ? { action: "open" } : { action: "skip" };
  }
  return out;
}

/** Whether any source is missing/moved and needs a decision before opening. */
export function hasBlockingSources(entries: SourcePreviewEntry[]): boolean {
  return entries.some((e) => e.status === "missing" || e.status === "movedCandidate");
}

// ----- view-gating warnings ---------------------------------------------------

export interface GatingWarning {
  sourceId: string;
  name: string;
  warnings: string[];
}

/** Collect the sources whose saved views won't reapply, with their reasons. */
export function gatingWarnings(entries: PlanEntry[]): GatingWarning[] {
  return entries
    .filter((e) => !e.reapplyViews && e.viewWarnings.length > 0)
    .map((e) => ({
      sourceId: e.sourceId,
      name: e.displayName ?? e.path,
      warnings: e.viewWarnings,
    }));
}

// ----- capturing sections for save -------------------------------------------

/** Mint a fresh, collision-free `src-N` id given the ids already in use. */
export function mintSourceId(used: Set<string>): string {
  let n = used.size + 1;
  let id = `src-${n}`;
  while (used.has(id)) {
    n += 1;
    id = `src-${n}`;
  }
  used.add(id);
  return id;
}

/** Capture one open document as a source entry (config + fingerprint only). */
function sourceFromTab(
  id: string,
  tab: DocumentMeta,
  fingerprint: FileFingerprint | null,
  prior?: ProjectSource,
): ProjectSource {
  return {
    id,
    path: tab.path as string,
    displayName: tab.fileName,
    fingerprint: fingerprint ?? prior?.fingerprint ?? null,
    open: {
      delimiter: tab.delimiter || null,
      encoding: tab.encoding || null,
      hasHeaderRow: tab.hasHeaderRow,
    },
    columns: tab.columnIds.map((cid, i) => ({ id: cid, name: tab.headers[i] ?? "" })),
  };
}

/**
 * Build the `sources` section by MERGING the open documents into the project's
 * existing sources. An existing source whose file is currently open is
 * refreshed (fingerprint, parse settings, column snapshot); one that is NOT
 * open is preserved untouched, so a source that was "left out" on open stays
 * referenced for next time. Open documents with no matching source are
 * appended with a fresh id. Only file-backed documents are referenceable;
 * unsaved buffers are skipped. Never captures cell data.
 */
export function buildSources(
  tabs: DocumentMeta[],
  existing: ProjectSource[],
  fingerprints: Record<number, FileFingerprint | null>,
): ProjectSource[] {
  const openByPath = new Map<string, DocumentMeta>();
  for (const tab of tabs) {
    if (tab.path) openByPath.set(pathKey(tab.path), tab);
  }
  const used = new Set(existing.map((s) => s.id));
  const seen = new Set<string>();
  const out: ProjectSource[] = [];

  // Preserve existing sources in place; refresh the ones that are open now.
  for (const prior of existing) {
    const key = pathKey(prior.path);
    const tab = openByPath.get(key);
    if (tab) {
      out.push(sourceFromTab(prior.id, tab, fingerprints[tab.id] ?? null, prior));
      seen.add(key);
    } else {
      out.push(prior);
    }
  }
  // Append open documents that the project didn't reference yet.
  for (const tab of tabs) {
    if (!tab.path) continue;
    const key = pathKey(tab.path);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(sourceFromTab(mintSourceId(used), tab, fingerprints[tab.id] ?? null));
  }
  return out;
}

/**
 * Build the `tabs` section (open order + active) from the open documents,
 * expressed in the stable source ids of the given `sources`.
 */
export function buildTabsSection(
  sources: ProjectSource[],
  tabs: DocumentMeta[],
  activeId: number | null,
): ProjectTabsSection {
  const idByPath = new Map(sources.map((s) => [pathKey(s.path), s.id]));
  const open: string[] = [];
  for (const tab of tabs) {
    if (!tab.path) continue;
    const id = idByPath.get(pathKey(tab.path));
    if (id) open.push(id);
  }
  const activeTab = tabs.find((t) => t.id === activeId);
  const active = activeTab?.path ? (idByPath.get(pathKey(activeTab.path)) ?? null) : null;
  return { open, active };
}

/** Build the `layout` section from the panel flags (config only). */
export function buildLayoutSection(panels: PanelLayout): ProjectLayoutSection {
  return {
    panels: {
      diagnostics: panels.diagnostics,
      explorer: panels.explorer,
      changes: panels.changes,
    },
  };
}

/** Read panel flags back from a stored layout section (missing → false). */
export function panelsFromLayout(layout: ProjectLayoutSection | null | undefined): PanelLayout {
  const p = layout?.panels;
  return {
    diagnostics: !!p?.diagnostics,
    explorer: !!p?.explorer,
    changes: !!p?.changes,
  };
}

/** One source's saved named views, mirroring the backend `views` section. */
export interface SourceViewsSection {
  sourceId: string;
  views: NamedView[];
  activeViewId: string | null;
}

/**
 * Build the `views` section: for each open, file-backed source, the named
 * views of its owning profile plus which view is active for that document.
 * Sources the project references but that are NOT open now keep their
 * previously-saved views (merged from `existing`), so leaving a document out
 * on open never drops its saved views. Never captures cell data — a named view
 * is filter/sort/column configuration referencing columns by stable id.
 */
export function buildViewsSection(
  sources: ProjectSource[],
  tabs: DocumentMeta[],
  existing: SourceViewsSection[],
  viewsForTab: (tab: DocumentMeta) => NamedView[],
  activeViewForTab: (tab: DocumentMeta) => string | null,
): SourceViewsSection[] {
  const tabByPath = new Map<string, DocumentMeta>();
  for (const t of tabs) if (t.path) tabByPath.set(pathKey(t.path), t);
  const out: SourceViewsSection[] = [];
  const seen = new Set<string>();

  // Capture live view state for every referenced source that is open now.
  for (const src of sources) {
    const tab = tabByPath.get(pathKey(src.path));
    if (!tab) continue;
    seen.add(src.id);
    const prior = existing.find((e) => e.sourceId === src.id);
    // Prefer the live profile's views, but never DROP the project's own saved
    // views: opening a project reapplies its views without importing them into a
    // matching file profile, so `viewsForTab` is empty for a project-owned
    // source when the user has no global profile carrying them. Falling back to
    // the prior saved section keeps those definitions instead of overwriting
    // them with an empty list on the first save.
    const liveViews = viewsForTab(tab);
    const views = liveViews.length > 0 ? liveViews : (prior?.views ?? []);
    // Keep an active view id only when it actually names one of the saved views
    // (prefer the live selection, else the project's prior one), so a captured
    // section can never carry an `activeViewId` that points at nothing.
    const candidateActive = activeViewForTab(tab) ?? prior?.activeViewId ?? null;
    const activeViewId =
      candidateActive != null && views.some((v) => v.id === candidateActive)
        ? candidateActive
        : null;
    if (views.length > 0 || activeViewId != null) {
      out.push({ sourceId: src.id, views, activeViewId });
    }
  }
  // Preserve saved views for still-referenced sources that aren't open.
  const referenced = new Set(sources.map((s) => s.id));
  for (const prior of existing) {
    if (referenced.has(prior.sourceId) && !seen.has(prior.sourceId)) {
      out.push(prior);
      seen.add(prior.sourceId);
    }
  }
  return out;
}

/**
 * Order the given tab ids to match a plan's tab order: tabs whose path matches
 * a plan entry come first (in plan order), any others keep their relative order
 * at the end. Used to restore saved tab order after opening a project.
 */
export function orderTabsForPlan(
  tabs: Pick<DocumentMeta, "id" | "path">[],
  entries: PlanEntry[],
): number[] {
  const idsByPath = new Map<string, number[]>();
  for (const t of tabs) {
    if (!t.path) continue;
    const key = pathKey(t.path);
    const arr = idsByPath.get(key) ?? [];
    arr.push(t.id);
    idsByPath.set(key, arr);
  }
  const ordered: number[] = [];
  const taken = new Set<number>();
  for (const entry of entries) {
    const arr = idsByPath.get(pathKey(entry.path));
    const id = arr?.shift();
    if (id !== undefined) {
      ordered.push(id);
      taken.add(id);
    }
  }
  for (const t of tabs) {
    if (!taken.has(t.id)) ordered.push(t.id);
  }
  return ordered;
}

// ----- queued project open ----------------------------------------------------

/** The next move for a queued project open (see `nextProjectOpenStep`). */
export type ProjectOpenStep =
  | { kind: "open"; path: string; remaining: string[] }
  | { kind: "wait" }
  | { kind: "finalize" };

/**
 * Decide the next move when driving a project open one source at a time. While a
 * source is still resolving through a DEFERRED flow — the large-file editable/
 * indexed decision, archive extraction, or the JSON import dialog — the queue
 * must WAIT: issuing the next open would overwrite the single pending decision,
 * and finalizing now would capture a baseline that omits the not-yet-opened tab
 * (immediately dirtying the just-opened project and skipping its saved view).
 * Only once nothing is deferred does it open the next source, or finalize when
 * the queue has drained.
 */
export function nextProjectOpenStep(remaining: string[], deferred: boolean): ProjectOpenStep {
  if (deferred) return { kind: "wait" };
  if (remaining.length === 0) return { kind: "finalize" };
  const [path, ...rest] = remaining;
  return { kind: "open", path, remaining: rest };
}
