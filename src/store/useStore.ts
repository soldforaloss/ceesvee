import { create } from "zustand";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";

import * as api from "../lib/tauri";
import { applyReplace } from "../lib/replace";
import { rangeConditions, specOf, valueCondition, withAndConditions } from "../lib/explorer";
import { matchingProfiles, profileSettingsDiffer } from "../lib/profiles";
import { currentOpenOptions, fingerprintKey } from "../lib/reopen";
import { isLegacyEncoding } from "../lib/save";
import type {
  AppSettings,
  CellRect,
  ColumnProfile,
  ColumnSummary,
  DiagnosticsReport,
  DocumentMeta,
  EncodingCompatibility,
  ExportOptions,
  ExportScope,
  ExternalChange,
  FileProfile,
  FilterGroup,
  FindMatch,
  FindOptions,
  JobFinished,
  JobProgress,
  ClusterReport,
  ClusterSpec,
  CompareInfo,
  CompareSpec,
  DedupSpec,
  DuplicateKeepStrategy,
  DuplicateReport,
  OpenEstimate,
  OpenOptions,
  ProfileScope,
  ProfileValidation,
  BatchReport,
  CrossRule,
  CrossValReport,
  OutlierReport,
  PiiReport,
  PiiSpec,
  OutlierSpec,
  ReparsePreview,
  SemanticAction,
  SemanticReport,
  SemanticType,
  SortKey,
  SplitOptions,
  TransformErrorPolicy,
  TransformSpec,
  ZipEntryInfo,
} from "../types";

export type ThemePref = "light" | "dark" | "system";

const RECENT_KEY = "ceesvee-recent";
const THEME_KEY = "ceesvee-theme";
const MAX_RECENT = 10;

// Debounce timer for the (backend-computed) selection statistics.
let statsTimer: ReturnType<typeof setTimeout> | null = null;
// Debounce timer for the (backend-computed) per-column summaries.
let summariesTimer: ReturnType<typeof setTimeout> | null = null;

/** Find-match cap for indexed read-only documents (F10). */
export const INDEXED_FIND_LIMIT = 5000;

/** Modal dialogs owned by the store so commands can open them (F11). */
export type ModalName =
  | "sort"
  | "export"
  | "summaries"
  | "filter"
  | "profiles"
  | "transform"
  | "dedup"
  | "compare"
  | "shortcuts"
  | "copyAs"
  | "pasteSpecial"
  | "cluster"
  | "semantic"
  | "crossval"
  | "repair"
  | "outlier"
  | "append"
  | "join"
  | "groupBy"
  | "reshape"
  | "recipes"
  | "pii";

const FILE_FILTERS = [
  { name: "Delimited text", extensions: ["csv", "tsv", "tab", "txt", "psv", "dat"] },
  { name: "Compressed (F17)", extensions: ["gz", "zip"] },
  { name: "All files", extensions: ["*"] },
];

export interface SelectionInfo {
  count: number;
  numericCount: number;
  sum: number;
  avg: number | null;
  min: number | null;
  max: number | null;
}

export interface FindState {
  open: boolean;
  query: string;
  replacement: string;
  regex: boolean;
  caseSensitive: boolean;
  wholeCell: boolean;
  inSelection: boolean;
  matches: FindMatch[];
  index: number;
}

const initialFind: FindState = {
  open: false,
  query: "",
  replacement: "",
  regex: false,
  caseSensitive: false,
  wholeCell: false,
  inSelection: false,
  matches: [],
  index: 0,
};

export interface FilterState {
  open: boolean;
  /** The query-builder tree (kept even while not applied, for editing). */
  spec: FilterGroup;
}

/** Diagnostics scan/report state for one document. */
export interface DiagnosticsDocState {
  /** Last completed report (kept visible, marked stale, while rescanning). */
  report: DiagnosticsReport | null;
  /** Running scan job, if any. */
  jobId: number | null;
  processed: number;
  total: number | null;
  /** Terminal error of the last scan, if it failed. */
  scanError: string | null;
  /**
   * The user cancelled the last scan. Suppresses the panel's auto-scan so
   * Cancel actually sticks; cleared when a scan is explicitly started.
   */
  cancelled: boolean;
}

const initialDiagnosticsDocState: DiagnosticsDocState = {
  report: null,
  jobId: null,
  processed: 0,
  total: null,
  scanError: null,
  cancelled: false,
};

/** One-shot request for the grid to scroll to and select a cell. */
export interface JumpTarget {
  row: number;
  col: number;
  /** Distinguishes repeated jumps to the same cell. */
  nonce: number;
}

let jumpNonce = 0;

/** State of the "Reopen with settings" dialog (active document only). */
export interface ReopenState {
  open: boolean;
  /** Pending setting overrides; unset fields keep the current interpretation. */
  options: OpenOptions;
  preview: ReparsePreview | null;
  loading: boolean;
  error: string | null;
}

const initialReopen: ReopenState = {
  open: false,
  options: {},
  preview: null,
  loading: false,
  error: null,
};

/** A detected on-disk change awaiting the user's decision. */
export interface ExternalPrompt {
  docId: number;
  change: ExternalChange;
}

/** A running save/export job, for the status-bar progress line. */
export interface FileJobState {
  jobId: number;
  docId: number;
  kind: "save" | "export";
  processed: number;
  total: number | null;
  bytesWritten: number | null;
  part: number | null;
}

/** A blocked lossy save/export awaiting the user's encoding decision. */
export interface EncodingIssuesPrompt {
  docId: number;
  path: string;
  options: ExportOptions;
  compat: EncodingCompatibility;
  /** What to re-run once the user picks a workable encoding. */
  action:
    | { type: "save" }
    | { type: "export"; scope: ExportScope; split: SplitOptions; writeManifest: boolean };
}

// Resolvers for in-flight save/export jobs, keyed by job id. Module-level:
// promise callbacks don't belong in reactive state.
const jobWaiters = new Map<number, (finished: JobFinished) => void>();

// Fast jobs can emit `job-finished` BEFORE the invoke that started them
// resolves with the job id (the backend spawns the worker and returns
// immediately). Buffer recent terminal events so late subscribers — awaitJob
// and the kind-specific tracking below — reconcile instead of waiting
// forever. Bounded FIFO; job ids are never reused within a session.
const finishedEarly = new Map<number, JobFinished>();
const FINISHED_EARLY_LIMIT = 64;

function rememberFinished(finished: JobFinished) {
  finishedEarly.set(finished.jobId, finished);
  if (finishedEarly.size > FINISHED_EARLY_LIMIT) {
    const oldest = finishedEarly.keys().next().value;
    if (oldest !== undefined) finishedEarly.delete(oldest);
  }
}

function awaitJob(jobId: number): Promise<JobFinished> {
  const early = finishedEarly.get(jobId);
  if (early) {
    finishedEarly.delete(jobId);
    return Promise.resolve(early);
  }
  return new Promise((resolve) => jobWaiters.set(jobId, resolve));
}

// Trailing-debounce timer for persisting the grid scroll position, plus the
// value it will write. Kept module-level so a tab switch inside the debounce
// window can flush the pending position into the OUTGOING tab's snapshot
// instead of leaking it into the next tab's live state.
let scrollTimer: ReturnType<typeof setTimeout> | null = null;
let pendingScroll: { row: number; column: number } | null = null;

/**
 * Everything document-specific about the UI (F08), snapshotted and restored
 * on tab switches so nothing leaks between documents.
 */
export interface DocumentUiState {
  find: FindState;
  filter: FilterState;
  columnWidths: Record<number, number>;
  frozenColumnCount: number;
  selection: CellRect | null;
  selectedRows: number[];
  selectedColumns: number[];
  scrollPosition: { row: number; column: number };
  activeExplorerColumn: number | null;
  lastExportOptions?: ExportOptions;
}

/** A matched profile suggestion awaiting the user's decision. */
export interface ProfileSuggestion {
  docId: number;
  profile: FileProfile;
}

/** Compare state (F09): one comparison at a time, across two documents. */
export interface CompareState {
  jobId: number | null;
  processed: number;
  total: number | null;
  compareId: number | null;
  info: CompareInfo | null;
  error: string | null;
}

const initialCompare: CompareState = {
  jobId: null,
  processed: 0,
  total: null,
  compareId: null,
  info: null,
  error: null,
};

/** A large-file open awaiting the user's mode choice (F10/F17). */
export interface OpenDecisionState {
  path: string;
  estimate: OpenEstimate;
  /** Set when the decision is about an extracted archive entry (F17). */
  archiveToken?: number;
}

/** A running index-related job (open, convert, reload, or extract). */
export interface IndexingState {
  jobId: number;
  docId: number;
  kind: "openIndexed" | "convertEditable" | "reindex" | "archiveExtract";
  path: string | null;
  /** Bytes scanned (open/reindex/extract) or rows materialised (convert). */
  processed: number;
  total: number | null;
  /** Extraction bookkeeping (F17). */
  archiveToken?: number;
  archiveEntry?: string | null;
}

/** Fuzzy clustering state (F24), for the ACTIVE document. */
export interface ClusterState {
  scanJobId: number | null;
  processed: number;
  total: number | null;
  report: ClusterReport | null;
  /** The scope the CURRENT report was scanned with — applies use this, not
   * whatever the dialog's scope controls say now. */
  scanScope: ExportScope | null;
  error: string | null;
}

const initialCluster: ClusterState = {
  scanJobId: null,
  processed: 0,
  total: null,
  report: null,
  scanScope: null,
  error: null,
};

/** Semantic-type detection state (F26), for the ACTIVE document. */
export interface SemanticState {
  scanJobId: number | null;
  processed: number;
  total: number | null;
  report: SemanticReport | null;
  error: string | null;
}

const initialSemantic: SemanticState = {
  scanJobId: null,
  processed: 0,
  total: null,
  report: null,
  error: null,
};

/** Cross-column validation state (F27), for the ACTIVE document. */
export interface CrossValState {
  scanJobId: number | null;
  processed: number;
  total: number | null;
  /** The rules the report was computed with (needed to re-derive filters). */
  rules: CrossRule[] | null;
  report: CrossValReport | null;
  error: string | null;
}

const initialCrossVal: CrossValState = {
  scanJobId: null,
  processed: 0,
  total: null,
  rules: null,
  report: null,
  error: null,
};

/** PII scan state (F28), for the ACTIVE document. */
export interface PiiState {
  scanJobId: number | null;
  processed: number;
  total: number | null;
  spec: PiiSpec | null;
  report: PiiReport | null;
  error: string | null;
}

const initialPii: PiiState = {
  scanJobId: null,
  processed: 0,
  total: null,
  spec: null,
  report: null,
  error: null,
};

/** A running batch-recipe job (F25). */
export interface BatchState {
  jobId: number;
  processed: number;
  total: number | null;
  message: string | null;
  report: BatchReport | null;
  error: string | null;
}

/** A running derived-document job (F20–F23: append/join/group/pivot). */
export interface DeriveState {
  jobId: number;
  /** The id the NEW document will register under. */
  docId: number;
  kind: "append" | "join" | "groupBy" | "reshape";
  processed: number;
  total: number | null;
  message: string | null;
}

/** Outlier-finder state (F30), for the ACTIVE document. */
export interface OutlierState {
  scanJobId: number | null;
  processed: number;
  total: number | null;
  /** The spec the report was computed with (needed for filter/actions). */
  spec: OutlierSpec | null;
  report: OutlierReport | null;
  error: string | null;
}

const initialOutlier: OutlierState = {
  scanJobId: null,
  processed: 0,
  total: null,
  spec: null,
  report: null,
  error: null,
};

/** ZIP entry chooser state (F17). */
export interface ArchivePickState {
  path: string;
  entries: ZipEntryInfo[];
}

/** A blocked suspicious-ratio extraction awaiting confirmation (F17). */
export interface ArchiveLargeConfirmState {
  path: string;
  entry: string | null;
}

/** Duplicate-finder state (F07), for the ACTIVE document. */
export interface DedupState {
  scanJobId: number | null;
  processed: number;
  total: number | null;
  report: DuplicateReport | null;
  error: string | null;
}

const initialDedup: DedupState = {
  scanJobId: null,
  processed: 0,
  total: null,
  report: null,
  error: null,
};

/** Column-explorer panel state (F05); profile data is for the ACTIVE doc. */
export interface ExplorerState {
  open: boolean;
  scope: ProfileScope;
  profile: ColumnProfile | null;
  jobId: number | null;
  processed: number;
  total: number | null;
  error: string | null;
}

const initialExplorer: ExplorerState = {
  open: false,
  scope: "all",
  profile: null,
  jobId: null,
  processed: 0,
  total: null,
  error: null,
};

const initialFilter: FilterState = {
  open: false,
  spec: {
    type: "group",
    id: "root",
    conjunction: "and",
    nodes: [
      { type: "condition", id: "c0", column: 0, op: "contains", value: "", caseSensitive: false },
    ],
  },
};

interface Store {
  tabs: DocumentMeta[];
  activeId: number | null;
  recent: string[];
  theme: ThemePref;
  /** Bumped to invalidate the grid's row cache after structural changes. */
  dataVersion: number;
  busy: boolean;
  error: string | null;
  selection: SelectionInfo | null;
  selectionRect: CellRect | null;
  selectedRows: number[];
  selectedCols: number[];
  find: FindState;
  filter: FilterState;
  /** Column widths of the ACTIVE document (saved/restored on tab switch). */
  columnWidths: Record<number, number>;
  /** Pinned leading columns of the ACTIVE document. */
  frozenColumnCount: number;
  /** Last grid scroll position of the ACTIVE document. */
  scrollPosition: { row: number; column: number };
  /** Column focused in the explorer panel (F05), per active document. */
  activeExplorerColumn: number | null;
  /** Export options last used for the ACTIVE document, seeding the dialog. */
  lastExportOptions?: ExportOptions;
  /** Saved UI state of every non-active document, keyed by document id. */
  uiStates: Record<number, DocumentUiState>;
  /** Detected per-column type + summary for the active doc (null until loaded). */
  summaries: ColumnSummary[] | null;
  /** Which document `summaries` belong to (guards against cross-tab staleness). */
  summariesDocId: number | null;
  /** Whether the diagnostics side panel is shown. */
  diagnosticsOpen: boolean;
  /** Diagnostics reports and scan state, keyed by document id. */
  diagnostics: Record<number, DiagnosticsDocState>;
  /** One-shot cell-jump request consumed by the grid. */
  jumpTarget: JumpTarget | null;
  /** "Reopen with settings" dialog state (for the active document). */
  reopen: ReopenState;
  /** On-disk change currently awaiting a decision, if any. */
  externalPrompt: ExternalPrompt | null;
  /** Disk fingerprints the user chose to ignore, keyed by document id. */
  ignoredFingerprints: Record<number, string>;
  /** Whether the quit confirmation (dirty tabs) is showing. */
  quitPromptOpen: boolean;
  /**
   * One-shot scope preselection for the next export-dialog open (F28's
   * "Export non-PII columns" must not default to all columns). Consumed and
   * cleared by the dialog.
   */
  exportPreferredScope: "selectedColumns" | null;
  setExportPreferredScope: (scope: "selectedColumns" | null) => void;
  /** Running save/export jobs, keyed by job id (status-bar progress). */
  fileJobs: Record<number, FileJobState>;
  /** A lossy save blocked by the encoding-compatibility scan, if any. */
  encodingIssues: EncodingIssuesPrompt | null;
  /** Persisted profiles + preferences (null until loaded). */
  settings: AppSettings | null;
  /** A profile matching the active document, awaiting a decision. */
  profileSuggestion: ProfileSuggestion | null;
  /** Latest profile-validation result, for the profiles dialog. */
  profileValidation: ProfileValidation | null;
  /** Column-explorer panel state (F05). */
  explorer: ExplorerState;
  /** Duplicate-finder state (F07). */
  dedup: DedupState;
  /** Compare state (F09). */
  compare: CompareState;
  /** Large-file open awaiting a mode choice (F10). */
  openDecision: OpenDecisionState | null;
  /** Running index build / conversion / re-index / extraction job. */
  indexing: IndexingState | null;
  /** Fuzzy clustering state (F24). */
  cluster: ClusterState;
  /** Semantic-type detection state (F26). */
  semantic: SemanticState;
  /** Cross-column validation state (F27). */
  crossval: CrossValState;
  /** Outlier-finder state (F30). */
  outlier: OutlierState;
  /** Running derived-document job (F20–F23), if any. */
  derive: DeriveState | null;
  /** Error from the last derive job, for the dialog that started it. */
  deriveError: string | null;
  /** Running (or just finished) batch-recipe job (F25), if any. */
  batch: BatchState | null;
  /** PII scan state (F28). */
  pii: PiiState;
  /** ZIP entry chooser (F17). */
  archivePick: ArchivePickState | null;
  /** Suspicious-ratio extraction awaiting confirmation (F17). */
  archiveLargeConfirm: ArchiveLargeConfirmState | null;

  /** The one open modal dialog, if any (F11: commands open dialogs). */
  activeModal: ModalName | null;
  /** Whether the command palette is open (F11). */
  paletteOpen: boolean;
  /** Command id the palette should open in argument mode for (F11). */
  paletteArgCommandId: string | null;
  /** Target of the multiline cell editor, in display coordinates (F13). */
  cellEditor: { row: number; col: number } | null;

  // lifecycle / chrome
  init: () => void;
  setTheme: (theme: ThemePref) => void;
  setError: (error: string | null) => void;
  setModal: (modal: ModalName | null) => void;
  setPaletteOpen: (open: boolean) => void;
  /** Open the palette directly in argument mode for one command (F11). */
  openPaletteForArg: (commandId: string) => void;
  /** Open/close the multiline cell editor (F13). */
  openCellEditor: (row: number, col: number) => void;
  closeCellEditor: () => void;
  /**
   * Persist a shortcut override for a command (F11): a binding string
   * rebinds, `null` unbinds, `undefined` resets to the default.
   */
  setShortcutOverride: (commandId: string, binding: string | null | undefined) => Promise<void>;
  setActive: (id: number) => void;
  setSelection: (rect: CellRect | null, rows: number[], cols: number[]) => void;
  setFrozenCols: (count: number) => void;
  setColumnWidth: (col: number, width: number) => void;
  resetColumnWidths: () => void;
  setScrollPosition: (row: number, column: number) => void;
  loadSummaries: () => void;
  /** Invalidate the grid's row cache (e.g. after an out-of-grid cell save). */
  invalidateGrid: () => void;

  // documents
  openDialog: () => Promise<void>;
  openPath: (path: string) => Promise<void>;
  newDoc: () => Promise<void>;
  closeTab: (id: number) => Promise<void>;
  setHeaderMode: (hasHeader: boolean) => Promise<void>;
  /** Re-fetch the active document's meta and invalidate the grid cache. */
  refreshActiveDoc: () => Promise<void>;

  // indexed read-only mode (F10)
  /** "Open editable" in the open-mode dialog: load fully despite the estimate. */
  confirmOpenEditable: () => Promise<void>;
  /** "Open read-only" in the open-mode dialog: start the indexing job. */
  confirmOpenIndexed: () => Promise<void>;
  dismissOpenDecision: () => void;
  /** Materialise the active indexed document into an editable one. */
  convertActiveToEditable: (force: boolean) => Promise<void>;
  cancelIndexing: () => Promise<void>;

  // fuzzy clustering (F24)
  startClusterScan: (spec: ClusterSpec) => Promise<void>;
  cancelClusterScan: () => Promise<void>;
  clearClusterReport: () => void;
  /** Apply accepted mappings as one undo step; true on success. */
  applyClusters: (
    column: number,
    mapping: [string, string][],
    scope: ExportScope,
    expectedRevision: number,
  ) => Promise<boolean>;

  // semantic data types (F26)
  startSemanticScan: () => Promise<void>;
  cancelSemanticScan: () => Promise<void>;
  clearSemanticReport: () => void;
  /** Re-adopt the backend-cached report (used when the dialog opens). */
  loadCachedSemanticReport: () => Promise<void>;
  /** Filter to rows (in)valid for a type; blanks match neither. */
  applySemanticFilter: (
    column: number,
    semantic: SemanticType,
    keepValid: boolean,
    expectedRevision: number,
  ) => Promise<boolean>;
  /** Apply a previewed semantic quick action as one undo step. */
  applySemanticAction: (
    column: number,
    semantic: SemanticType,
    action: SemanticAction,
    expectedRevision: number,
  ) => Promise<boolean>;

  // cross-column validation (F27)
  startCrossvalScan: (rules: CrossRule[]) => Promise<void>;
  cancelCrossvalScan: () => Promise<void>;
  clearCrossvalReport: () => void;
  /** Re-adopt the backend-cached report (used when the dialog opens). */
  loadCachedCrossvalReport: () => Promise<void>;
  /** Filter to rows violating one rule (index) or any rule (null). */
  applyCrossvalFilter: (rule: number | null) => Promise<boolean>;

  // derived documents (F20–F23)
  /** Track a started derive job so completion adds the new tab. */
  trackDerive: (jobId: number, docId: number, kind: DeriveState["kind"]) => void;
  cancelDerive: () => Promise<void>;

  // batch recipes (F25)
  /** Track a started batch job; the report lands on completion. */
  trackBatch: (jobId: number) => void;
  cancelBatch: () => Promise<void>;
  clearBatch: () => void;

  // PII (F28)
  startPiiScan: (spec: PiiSpec) => Promise<void>;
  cancelPiiScan: () => Promise<void>;
  clearPiiReport: () => void;
  /** Re-adopt the backend-cached report (used when the dialog opens). */
  loadCachedPiiReport: () => Promise<void>;

  // outlier finder (F30)
  startOutlierScan: (spec: OutlierSpec) => Promise<void>;
  cancelOutlierScan: () => Promise<void>;
  clearOutlierReport: () => void;
  /** Re-adopt the backend-cached report (used when the dialog opens). */
  loadCachedOutlierReport: () => Promise<void>;
  /** Filter to the rows holding flagged values. */
  applyOutlierFilter: () => Promise<boolean>;

  // compressed files (F17)
  /** Start extracting an archive (gzip member or chosen ZIP entry). */
  startArchiveExtract: (path: string, entry: string | null, allowLarge: boolean) => Promise<void>;
  pickArchiveEntry: (entry: string) => Promise<void>;
  dismissArchivePick: () => void;
  confirmArchiveLarge: () => Promise<void>;
  dismissArchiveLarge: () => void;

  // reopen with settings (F02)
  openReopenDialog: (initial?: OpenOptions) => void;
  closeReopenDialog: () => void;
  setReopenOptions: (patch: OpenOptions) => void;
  refreshReopenPreview: () => Promise<void>;
  /** Apply the previewed settings; saves first unless `discard`. */
  confirmReopen: (discard: boolean) => Promise<void>;

  // external changes (F02)
  checkExternalChanges: () => Promise<void>;
  resolveExternalPrompt: (action: "reload" | "ignore" | "saveAs" | "openDisk") => Promise<void>;

  // quit flow (F02)
  setQuitPromptOpen: (open: boolean) => void;
  confirmQuit: (mode: "save" | "discard") => Promise<void>;

  // save / export pipeline (F03/F04)
  /** Resolve a blocked lossy save: retry with another encoding, or cancel. */
  resolveEncodingIssues: (retryEncoding: string | null) => Promise<void>;
  cancelFileJob: (jobId: number) => Promise<void>;
  /** Scoped/split export of the active document (F04). Prompts for a path. */
  exportScoped: (
    options: ExportOptions,
    scope: ExportScope,
    split: SplitOptions,
    writeManifest: boolean,
  ) => Promise<void>;

  // data-cleaning transforms (F06)
  /**
   * Apply a previewed transform (one undo step). Returns whether it was
   * committed; re-applies the active filter afterwards.
   */
  applyTransformSpec: (
    spec: TransformSpec,
    scope: ExportScope,
    policy: TransformErrorPolicy,
    expectedRevision: number,
  ) => Promise<boolean>;

  // compare (F09)
  runCompare: (rightDocId: number, spec: CompareSpec) => Promise<void>;
  cancelCompare: () => Promise<void>;
  clearCompare: () => void;
  /** Export added/removed/changed rows or the JSON report (prompts for path). */
  exportCompare: (which: import("../types").DiffStatus | "report") => Promise<void>;
  /** Activate a document and jump the grid to a row. */
  jumpToDocCell: (docId: number, row: number) => Promise<void>;

  // duplicate finder (F07)
  startDuplicateScan: (spec: DedupSpec, scope: ExportScope) => Promise<void>;
  cancelDuplicateScan: () => Promise<void>;
  clearDuplicateReport: () => void;
  filterToDuplicates: (spec: DedupSpec, scope: ExportScope) => Promise<void>;
  /** Remove duplicates (one undo step). Returns whether it was committed. */
  applyDedup: (
    spec: DedupSpec,
    scope: ExportScope,
    keep: DuplicateKeepStrategy,
    expectedRevision: number,
  ) => Promise<boolean>;

  // column explorer (F05)
  setExplorerOpen: (open: boolean) => void;
  setExplorerColumn: (column: number) => void;
  setExplorerScope: (scope: ProfileScope) => void;
  /** Fetch (cached) or compute the profile for the current column/scope. */
  refreshExplorerProfile: () => Promise<void>;
  cancelExplorerProfile: () => Promise<void>;
  /** Filter actions from a selected value ("only" | "exclude" | "and"). */
  applyValueFilter: (value: string, mode: "only" | "exclude" | "and") => Promise<void>;
  applyRangeFilter: (min: string | null, max: string | null) => Promise<void>;

  // file profiles (F08)
  saveProfiles: (profiles: FileProfile[]) => Promise<void>;
  /** Apply a profile via the previewed reopen flow (safe for dirty docs). */
  applyProfile: (profile: FileProfile) => void;
  dismissProfileSuggestion: () => void;
  runProfileValidation: (profile: FileProfile) => Promise<void>;
  clearProfileValidation: () => void;

  // editing
  setCell: (row: number, col: number, value: string) => Promise<void>;
  pasteBlock: (row: number, col: number, block: string[][]) => Promise<void>;
  insertRows: (at: number, count: number) => Promise<void>;
  deleteRows: (indices: number[]) => Promise<void>;
  moveRow: (from: number, to: number) => Promise<void>;
  insertColumn: (at: number, name: string) => Promise<void>;
  deleteColumns: (indices: number[]) => Promise<void>;
  renameColumn: (col: number, name: string) => Promise<void>;
  sortBy: (keys: SortKey[]) => Promise<void>;
  undo: () => Promise<void>;
  redo: () => Promise<void>;
  saveActive: (saveAs: boolean, exportOptions?: Partial<ExportOptions>) => Promise<void>;

  // find / replace
  setFindOpen: (open: boolean) => void;
  updateFind: (patch: Partial<FindState>) => void;
  runFind: () => Promise<void>;
  gotoMatch: (delta: number) => void;
  replaceCurrent: () => Promise<void>;
  replaceAllMatches: () => Promise<void>;

  // filter
  setFilterOpen: (open: boolean) => void;
  updateFilterSpec: (spec: FilterGroup) => void;
  applyFilter: (spec: FilterGroup) => Promise<void>;
  clearFilter: () => Promise<void>;

  // diagnostics
  setDiagnosticsOpen: (open: boolean) => void;
  runDiagnosticsScan: () => Promise<void>;
  cancelDiagnosticsScan: () => Promise<void>;
  applyIssueFilter: (issueId: string) => Promise<void>;
  jumpToCell: (row: number, col: number) => Promise<void>;

  // background-job events (wired to the Tauri event listeners in App)
  handleJobProgress: (progress: JobProgress) => void;
  handleJobFinished: (finished: JobFinished) => Promise<void>;
}

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}

function loadTheme(): ThemePref {
  const t = localStorage.getItem(THEME_KEY);
  return t === "light" || t === "dark" || t === "system" ? t : "system";
}

function applyThemeClass(theme: ThemePref) {
  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  const dark = theme === "dark" || (theme === "system" && prefersDark);
  document.documentElement.classList.toggle("dark", dark);
}

export const useStore = create<Store>((set, get) => {
  // ----- internal helpers -------------------------------------------------

  const activeMeta = (): DocumentMeta | null => {
    const { tabs, activeId } = get();
    return tabs.find((t) => t.id === activeId) ?? null;
  };

  const defaultUiState = (): DocumentUiState => ({
    find: initialFind,
    filter: initialFilter,
    columnWidths: {},
    frozenColumnCount: 0,
    selection: null,
    selectedRows: [],
    selectedColumns: [],
    scrollPosition: { row: 0, column: 0 },
    activeExplorerColumn: null,
    lastExportOptions: undefined,
  });

  /**
   * The state patch for making `id` the active document (F08): snapshot the
   * current document's UI state, then restore the target's (or defaults).
   * Nothing document-specific survives the switch outside its snapshot.
   */
  const switchPatch = (s: Store, id: number | null): Partial<Store> => {
    // Flush a scroll update still sitting in the debounce window: it belongs
    // to the OUTGOING document, so snapshot it there rather than letting the
    // timer fire later and pollute the incoming tab's live state.
    if (scrollTimer !== null) {
      clearTimeout(scrollTimer);
      scrollTimer = null;
    }
    const flushedScroll = pendingScroll ?? s.scrollPosition;
    pendingScroll = null;

    const uiStates = { ...s.uiStates };
    if (s.activeId != null && s.tabs.some((t) => t.id === s.activeId)) {
      uiStates[s.activeId] = {
        find: s.find,
        filter: s.filter,
        columnWidths: s.columnWidths,
        frozenColumnCount: s.frozenColumnCount,
        selection: s.selectionRect,
        selectedRows: s.selectedRows,
        selectedColumns: s.selectedCols,
        scrollPosition: flushedScroll,
        activeExplorerColumn: s.activeExplorerColumn,
        lastExportOptions: s.lastExportOptions,
      };
    }
    const next = (id != null ? uiStates[id] : undefined) ?? defaultUiState();
    return {
      uiStates,
      activeId: id,
      find: next.find,
      filter: next.filter,
      columnWidths: next.columnWidths,
      frozenColumnCount: next.frozenColumnCount,
      selectionRect: next.selection,
      selectedRows: next.selectedRows,
      selectedCols: next.selectedColumns,
      selection: null,
      scrollPosition: next.scrollPosition,
      activeExplorerColumn: next.activeExplorerColumn,
      lastExportOptions: next.lastExportOptions,
      summaries: null,
      summariesDocId: null,
      jumpTarget: null,
      reopen: initialReopen,
      profileSuggestion:
        s.profileSuggestion && s.profileSuggestion.docId === id ? s.profileSuggestion : null,
      // The panel stays open across switches, but the profile is per-doc.
      explorer: {
        ...s.explorer,
        profile: null,
        jobId: null,
        processed: 0,
        total: null,
        error: null,
      },
      dedup: initialDedup,
      // Cluster reports are per-document; never let one leak across tabs.
      cluster: initialCluster,
      // Per-doc reports; the dialogs re-adopt the backend cache on open.
      semantic: initialSemantic,
      crossval: initialCrossVal,
      outlier: initialOutlier,
      pii: initialPii,
    };
  };

  /** Surface (or auto-apply) a matching profile after a document opens. */
  const suggestProfileFor = async (meta: DocumentMeta) => {
    const profiles = get().settings?.profiles ?? [];
    if (!meta.path) return;
    const matches = matchingProfiles(profiles, meta.path);
    if (matches.length === 0) return;

    const current = get().tabs.find((t) => t.id === meta.id);
    if (!current) return;
    // First matching profile that would actually CHANGE the interpretation:
    // a broad already-satisfied profile must not shadow a later, more
    // specific one that differs.
    const profile = matches.find((m) => profileSettingsDiffer(m, current));
    if (!profile) return; // already interpreted this way

    // Automatic application requires the profile's explicit opt-in AND a
    // clean document — a dirty document is never silently reparsed.
    if (profile.autoApply && !current.dirty) {
      try {
        const updated = await api.applyReparse(
          meta.id,
          {
            delimiter: profile.delimiter ?? undefined,
            encoding: profile.encoding ?? undefined,
            hasHeaderRow: profile.hasHeaderRow ?? undefined,
          },
          current.revision,
        );
        reloadDoc(updated);
        return;
      } catch {
        // Fall through to a manual suggestion on any failure.
      }
    }
    set({ profileSuggestion: { docId: meta.id, profile } });
  };

  /** Replace a tab's metadata (dirty/undo flags) without reloading the grid. */
  const refreshMeta = (meta: DocumentMeta) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === meta.id ? meta : t)) }));

  /** Replace metadata AND invalidate the grid cache (structural change). */
  const reloadDoc = (meta: DocumentMeta) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === meta.id ? meta : t)),
      dataVersion: s.dataVersion + 1,
    }));

  const pushRecent = (path: string) => {
    const next = [path, ...get().recent.filter((p) => p !== path)].slice(0, MAX_RECENT);
    set({ recent: next });
    try {
      localStorage.setItem(RECENT_KEY, JSON.stringify(next));
    } catch {
      /* ignore */
    }
  };

  /** Run a structural mutation against the active doc with error handling. */
  const mutate = async (fn: (id: number) => Promise<DocumentMeta>, reload = true) => {
    const id = get().activeId;
    if (id == null) return;
    try {
      const meta = await fn(id);
      if (reload) reloadDoc(meta);
      else refreshMeta(meta);
    } catch (e) {
      set({ error: String(e) });
    }
  };

  /** Merge a partial update into one document's diagnostics state. */
  const patchDiagnostics = (docId: number, patch: Partial<DiagnosticsDocState>) => {
    set((s) => ({
      diagnostics: {
        ...s.diagnostics,
        [docId]: { ...(s.diagnostics[docId] ?? initialDiagnosticsDocState), ...patch },
      },
    }));
  };

  /**
   * A tracked job may have finished before its id was recorded (fast jobs
   * race the invoke's round trip). Call right after storing a job id: if the
   * terminal event already arrived, replay it through the normal handler.
   */
  const consumeEarlyFinish = (jobId: number) => {
    const finished = finishedEarly.get(jobId);
    if (finished) {
      finishedEarly.delete(jobId);
      void get().handleJobFinished(finished);
    }
  };

  /**
   * Run one atomic streaming save job to completion. Returns whether the file
   * was written (progress streams over the job events into `fileJobs`).
   */
  const runSaveJob = async (id: number, path: string, options: ExportOptions): Promise<boolean> => {
    const meta = get().tabs.find((t) => t.id === id);
    if (!meta) return false;
    try {
      const jobId = await api.startSave(id, path, options, meta.revision);
      const finished = await awaitJob(jobId);
      if (finished.status === "done") {
        const updated = await api.getMeta(id);
        refreshMeta(updated);
        if (updated.path) pushRecent(updated.path);
        return true;
      }
      if (finished.status === "failed") {
        set({ error: finished.error ?? "save failed" });
      }
      return false; // failed or cancelled
    } catch (e) {
      set({ error: String(e) });
      return false;
    }
  };

  /**
   * Save one document (any tab, not just the active one). Returns whether the
   * file was actually written — false when the user cancels the Save As
   * dialog, the encoding scan blocks the write, or the write fails, so
   * callers (reopen, quit) can abort safely.
   */
  const saveDocById = async (
    id: number,
    saveAs: boolean,
    exportOptions?: Partial<ExportOptions>,
  ): Promise<boolean> => {
    const meta = get().tabs.find((t) => t.id === id);
    if (!meta) return false;
    let path = meta.path;
    if (saveAs || !path) {
      const chosen = await saveFileDialog({
        defaultPath: meta.fileName,
        filters: FILE_FILTERS,
      });
      if (!chosen) return false;
      path = chosen;
    }
    const options: ExportOptions = {
      delimiter: meta.delimiter || ",",
      encoding: meta.encoding || "UTF-8",
      quoteStyle: "minimal",
      lineEnding: meta.lineEnding,
      bom: meta.hadBom,
      includeHeaders: true,
      backup: "none",
      ...exportOptions,
    };

    // Block lossy legacy-encoding writes up front (F03): the user decides
    // between UTF-8, another encoding, or cancelling — never silent loss.
    if (isLegacyEncoding(options.encoding)) {
      try {
        const compat = await api.checkEncodingCompatibility(
          id,
          options.encoding,
          undefined,
          options.includeHeaders,
        );
        if (!compat.compatible) {
          set({
            encodingIssues: { docId: id, path, options, compat, action: { type: "save" } },
          });
          return false;
        }
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    }

    return runSaveJob(id, path, options);
  };

  /** Run one scoped export job to completion (F04). */
  const runExportJob = async (
    id: number,
    path: string,
    options: ExportOptions,
    scope: ExportScope,
    split: SplitOptions,
    writeManifest: boolean,
  ): Promise<boolean> => {
    const meta = get().tabs.find((t) => t.id === id);
    if (!meta) return false;
    try {
      const jobId = await api.startExport(
        id,
        path,
        options,
        scope,
        split,
        writeManifest,
        meta.revision,
      );
      const finished = await awaitJob(jobId);
      if (finished.status === "failed") {
        set({ error: finished.error ?? "export failed" });
      }
      return finished.status === "done";
    } catch (e) {
      set({ error: String(e) });
      return false;
    }
  };

  return {
    tabs: [],
    activeId: null,
    recent: [],
    theme: "system",
    dataVersion: 0,
    busy: false,
    error: null,
    activeModal: null,
    paletteOpen: false,
    paletteArgCommandId: null,
    cellEditor: null,
    selection: null,
    selectionRect: null,
    selectedRows: [],
    selectedCols: [],
    find: initialFind,
    filter: initialFilter,
    columnWidths: {},
    frozenColumnCount: 0,
    scrollPosition: { row: 0, column: 0 },
    activeExplorerColumn: null,
    lastExportOptions: undefined,
    uiStates: {},
    summaries: null,
    summariesDocId: null,
    diagnosticsOpen: false,
    diagnostics: {},
    jumpTarget: null,
    reopen: initialReopen,
    openDecision: null,
    indexing: null,
    cluster: initialCluster,
    semantic: initialSemantic,
    crossval: initialCrossVal,
    outlier: initialOutlier,
    derive: null,
    deriveError: null,
    batch: null,
    pii: initialPii,
    archivePick: null,
    archiveLargeConfirm: null,
    externalPrompt: null,
    ignoredFingerprints: {},
    quitPromptOpen: false,
    exportPreferredScope: null,
    fileJobs: {},
    encodingIssues: null,
    settings: null,
    profileSuggestion: null,
    profileValidation: null,
    explorer: initialExplorer,
    dedup: initialDedup,
    compare: initialCompare,

    init: () => {
      const theme = loadTheme();
      applyThemeClass(theme);
      set({ recent: loadRecent(), theme });
      window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
        if (get().theme === "system") applyThemeClass("system");
      });
      // Load persisted profiles (corrupt files fall back to defaults in Rust).
      void api
        .getSettings()
        .then((settings) => set({ settings }))
        .catch(() => set({ settings: { version: 1, profiles: [] } }));
    },

    setModal: (modal) => set({ activeModal: modal }),

    setPaletteOpen: (open) =>
      set(open ? { paletteOpen: true } : { paletteOpen: false, paletteArgCommandId: null }),

    openPaletteForArg: (commandId) => set({ paletteOpen: true, paletteArgCommandId: commandId }),

    openCellEditor: (row, col) => set({ cellEditor: { row, col } }),

    closeCellEditor: () => set({ cellEditor: null }),

    setShortcutOverride: async (commandId, binding) => {
      const current = get().settings ?? { version: 1, profiles: [] };
      const overrides = { ...(current.shortcutOverrides ?? {}) };
      if (binding === undefined) {
        delete overrides[commandId];
      } else {
        overrides[commandId] = binding;
      }
      const next = { ...current, shortcutOverrides: overrides };
      try {
        await api.setSettings(next);
        set({ settings: next });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    setTheme: (theme) => {
      try {
        localStorage.setItem(THEME_KEY, theme);
      } catch {
        /* ignore */
      }
      applyThemeClass(theme);
      set({ theme });
    },

    setError: (error) => set({ error }),

    setExportPreferredScope: (scope) => set({ exportPreferredScope: scope }),

    setActive: (id) => set((s) => switchPatch(s, id)),

    setSelection: (rect, rows, cols) => {
      set({ selectionRect: rect, selectedRows: rows, selectedCols: cols });
      if (statsTimer !== null) clearTimeout(statsTimer);
      const id = get().activeId;
      if (id === null || !rect || rect.width * rect.height <= 1) {
        set({ selection: null });
        return;
      }
      // Compute aggregates in Rust over the full range (the front-end cache only
      // holds the visible window, so client-side stats would be wrong for large
      // selections). Debounced and guarded against stale results.
      statsTimer = setTimeout(() => {
        void api
          .selectionStats(id, rect)
          .then((stats) => {
            if (get().selectionRect === rect) set({ selection: stats });
          })
          .catch(() => undefined);
      }, 120);
    },

    setFrozenCols: (count) => set({ frozenColumnCount: Math.max(0, count) }),

    setColumnWidth: (col, width) =>
      set((s) => ({ columnWidths: { ...s.columnWidths, [col]: width } })),

    resetColumnWidths: () => set({ columnWidths: {} }),

    invalidateGrid: () => set((s) => ({ dataVersion: s.dataVersion + 1 })),

    setScrollPosition: (row, column) => {
      // Trailing debounce: visible-region events fire on every scroll frame.
      pendingScroll = { row, column };
      if (scrollTimer !== null) clearTimeout(scrollTimer);
      scrollTimer = setTimeout(() => {
        scrollTimer = null;
        const latest = pendingScroll;
        pendingScroll = null;
        if (!latest) return;
        const current = get().scrollPosition;
        if (current.row !== latest.row || current.column !== latest.column) {
          set({ scrollPosition: latest });
        }
      }, 250);
    },

    loadSummaries: () => {
      const id = get().activeId;
      if (id == null) {
        set({ summaries: null, summariesDocId: null });
        return;
      }
      // Computed in Rust over the full document (the front-end cache only holds
      // the visible window). Debounced and guarded against a tab switch.
      if (summariesTimer !== null) clearTimeout(summariesTimer);
      summariesTimer = setTimeout(() => {
        void api
          .columnSummaries(id)
          .then((summaries) => {
            if (get().activeId === id) set({ summaries, summariesDocId: id });
          })
          .catch(() => undefined);
      }, 150);
    },

    openDialog: async () => {
      const selected = await openFileDialog({ multiple: false, filters: FILE_FILTERS });
      if (typeof selected === "string") await get().openPath(selected);
    },

    openPath: async (path) => {
      const existing = get().tabs.find((t) => t.path === path);
      if (existing) {
        set((s) => switchPatch(s, existing.id));
        return;
      }
      // F17: compressed files route through extraction first.
      const lower = path.toLowerCase();
      if (lower.endsWith(".zip")) {
        set({ busy: true, error: null });
        try {
          const entries = await api.listArchiveEntries(path);
          const openable = entries.filter((e) => !e.encrypted);
          if (openable.length === 1 && entries.length === 1) {
            set({ busy: false });
            await get().startArchiveExtract(path, openable[0].name, false);
          } else {
            set({ busy: false, archivePick: { path, entries } });
          }
        } catch (e) {
          set({ error: String(e), busy: false });
        }
        return;
      }
      if (lower.endsWith(".gz")) {
        await get().startArchiveExtract(path, null, false);
        return;
      }
      set({ busy: true, error: null });
      try {
        // F10: estimate the in-memory cost first. Large files pause here and
        // let the user pick editable vs indexed read-only.
        const estimate = await api.probeOpen(path);
        if (estimate.needsDecision) {
          set({ busy: false, openDecision: { path, estimate } });
          return;
        }
        const meta = await api.openFile(path);
        set((s) => ({
          ...switchPatch(s, meta.id),
          tabs: [...s.tabs, meta],
          busy: false,
        }));
        pushRecent(path);
        void suggestProfileFor(meta);
      } catch (e) {
        set({ error: String(e), busy: false });
      }
    },

    // ----- indexed read-only mode (F10) --------------------------------------

    confirmOpenEditable: async () => {
      const decision = get().openDecision;
      if (!decision) return;
      set({ openDecision: null, busy: true, error: null });
      try {
        if (decision.archiveToken != null) {
          // F17: consume the parked extraction as a fully in-memory doc.
          const started = await api.openArchiveDocument(decision.archiveToken, "editable");
          const meta = await api.getMeta(started.docId);
          set((s) => ({ ...switchPatch(s, meta.id), tabs: [...s.tabs, meta], busy: false }));
          pushRecent(decision.path);
          return;
        }
        const meta = await api.openFile(decision.path, { forceInMemory: true });
        set((s) => ({
          ...switchPatch(s, meta.id),
          tabs: [...s.tabs, meta],
          busy: false,
        }));
        pushRecent(decision.path);
        void suggestProfileFor(meta);
      } catch (e) {
        set({ error: String(e), busy: false });
      }
    },

    confirmOpenIndexed: async () => {
      const decision = get().openDecision;
      if (!decision) return;
      set({ openDecision: null, error: null });
      try {
        if (decision.archiveToken != null) {
          // F17: index the extracted entry; the job reuses the openIndexed
          // completion path, which adds the tab.
          const started = await api.openArchiveDocument(decision.archiveToken, "indexed");
          set({
            indexing: {
              jobId: started.jobId,
              docId: started.docId,
              kind: "openIndexed",
              path: decision.path,
              processed: 0,
              total: decision.estimate.fileSize,
            },
          });
          consumeEarlyFinish(started.jobId);
          return;
        }
        const started = await api.startOpenIndexed(decision.path);
        set({
          indexing: {
            jobId: started.jobId,
            docId: started.docId,
            kind: "openIndexed",
            path: decision.path,
            processed: 0,
            total: decision.estimate.fileSize,
          },
        });
        consumeEarlyFinish(started.jobId);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    dismissOpenDecision: () => {
      const token = get().openDecision?.archiveToken;
      if (token != null) void api.discardArchive(token).catch(() => undefined);
      set({ openDecision: null });
    },

    convertActiveToEditable: async (force) => {
      const id = get().activeId;
      if (id == null || get().indexing) return;
      try {
        const jobId = await api.startConvertToEditable(id, force);
        set({
          indexing: {
            jobId,
            docId: id,
            kind: "convertEditable",
            path: null,
            processed: 0,
            total: null,
          },
          error: null,
        });
        consumeEarlyFinish(jobId);
      } catch (e) {
        // Typically the memory-estimate refusal; the SourceBar offers force.
        set({ error: String(e) });
      }
    },

    cancelIndexing: async () => {
      const indexing = get().indexing;
      if (indexing) await api.cancelJob(indexing.jobId).catch(() => undefined);
    },

    // ----- fuzzy clustering (F24) ---------------------------------------------

    startClusterScan: async (spec) => {
      const meta = activeMeta();
      if (!meta || get().cluster.scanJobId != null) return;
      try {
        const jobId = await api.startClusterScan(meta.id, spec, meta.revision);
        set((s) => ({
          cluster: {
            ...s.cluster,
            scanJobId: jobId,
            processed: 0,
            total: null,
            scanScope: spec.scope,
            error: null,
          },
        }));
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ cluster: { ...s.cluster, error: String(e) } }));
      }
    },

    cancelClusterScan: async () => {
      const jobId = get().cluster.scanJobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    clearClusterReport: () => set({ cluster: initialCluster }),

    applyClusters: async (column, mapping, scope, expectedRevision) => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        const updated = await api.applyValueClusters(
          meta.id,
          column,
          mapping,
          scope,
          expectedRevision,
        );
        reloadDoc(updated);
        // The document changed; the report is stale by definition.
        set({ cluster: initialCluster });
        return true;
      } catch (e) {
        set((s) => ({ cluster: { ...s.cluster, error: String(e) } }));
        return false;
      }
    },

    // ----- semantic data types (F26) -------------------------------------------

    startSemanticScan: async () => {
      const meta = activeMeta();
      if (!meta || get().semantic.scanJobId != null) return;
      try {
        const jobId = await api.startSemanticScan(meta.id, meta.revision);
        set((s) => ({
          semantic: { ...s.semantic, scanJobId: jobId, processed: 0, total: null, error: null },
        }));
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ semantic: { ...s.semantic, error: String(e) } }));
      }
    },

    cancelSemanticScan: async () => {
      const jobId = get().semantic.scanJobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    clearSemanticReport: () => set({ semantic: initialSemantic }),

    loadCachedSemanticReport: async () => {
      const meta = activeMeta();
      if (!meta || get().semantic.report !== null || get().semantic.scanJobId != null) return;
      const report = await api.getSemanticReport(meta.id).catch(() => null);
      if (report) set((s) => ({ semantic: { ...s.semantic, report } }));
    },

    applySemanticFilter: async (column, semantic, keepValid, expectedRevision) => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        const updated = await api.applySemanticFilter(
          meta.id,
          column,
          semantic,
          keepValid,
          expectedRevision,
        );
        reloadDoc(updated);
        return true;
      } catch (e) {
        set((s) => ({ semantic: { ...s.semantic, error: String(e) } }));
        return false;
      }
    },

    applySemanticAction: async (column, semantic, action, expectedRevision) => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        const updated = await api.applySemanticAction(
          meta.id,
          column,
          semantic,
          action,
          expectedRevision,
        );
        reloadDoc(updated);
        // The document changed; the report is stale by definition.
        set((s) => ({ semantic: { ...s.semantic, report: null } }));
        return true;
      } catch (e) {
        set((s) => ({ semantic: { ...s.semantic, error: String(e) } }));
        return false;
      }
    },

    // ----- cross-column validation (F27) ---------------------------------------

    startCrossvalScan: async (rules) => {
      const meta = activeMeta();
      if (!meta || get().crossval.scanJobId != null) return;
      try {
        const jobId = await api.startCrossvalScan(meta.id, rules, meta.revision);
        set((s) => ({
          crossval: {
            ...s.crossval,
            scanJobId: jobId,
            processed: 0,
            total: null,
            rules,
            error: null,
          },
        }));
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ crossval: { ...s.crossval, error: String(e) } }));
      }
    },

    cancelCrossvalScan: async () => {
      const jobId = get().crossval.scanJobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    clearCrossvalReport: () => set({ crossval: initialCrossVal }),

    loadCachedCrossvalReport: async () => {
      const meta = activeMeta();
      if (!meta || get().crossval.report !== null || get().crossval.scanJobId != null) return;
      const cached = await api.getCrossvalReport(meta.id).catch(() => null);
      if (cached) {
        const [rules, report] = cached;
        set((s) => ({ crossval: { ...s.crossval, rules, report } }));
      }
    },

    applyCrossvalFilter: async (rule) => {
      const meta = activeMeta();
      const { rules, report } = get().crossval;
      if (!meta || !rules || !report) return false;
      try {
        const updated = await api.applyCrossvalFilter(meta.id, rules, rule, report.revision);
        reloadDoc(updated);
        return true;
      } catch (e) {
        set((s) => ({ crossval: { ...s.crossval, error: String(e) } }));
        return false;
      }
    },

    // ----- derived documents (F20–F23) -------------------------------------------

    trackDerive: (jobId, docId, kind) => {
      set({
        derive: { jobId, docId, kind, processed: 0, total: null, message: null },
        deriveError: null,
      });
      consumeEarlyFinish(jobId);
    },

    cancelDerive: async () => {
      const derive = get().derive;
      if (derive) await api.cancelJob(derive.jobId).catch(() => undefined);
    },

    // ----- batch recipes (F25) ---------------------------------------------------

    trackBatch: (jobId) => {
      set({
        batch: { jobId, processed: 0, total: null, message: null, report: null, error: null },
      });
      consumeEarlyFinish(jobId);
    },

    cancelBatch: async () => {
      const batch = get().batch;
      if (batch) await api.cancelJob(batch.jobId).catch(() => undefined);
    },

    clearBatch: () => set({ batch: null }),

    // ----- PII (F28) -------------------------------------------------------------

    startPiiScan: async (spec) => {
      const meta = activeMeta();
      if (!meta || get().pii.scanJobId != null) return;
      try {
        const jobId = await api.startPiiScan(meta.id, spec, meta.revision);
        set((s) => ({
          pii: { ...s.pii, scanJobId: jobId, processed: 0, total: null, spec, error: null },
        }));
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ pii: { ...s.pii, error: String(e) } }));
      }
    },

    cancelPiiScan: async () => {
      const jobId = get().pii.scanJobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    clearPiiReport: () => set({ pii: initialPii }),

    loadCachedPiiReport: async () => {
      const meta = activeMeta();
      if (!meta || get().pii.report !== null || get().pii.scanJobId != null) return;
      const cached = await api.getPiiReport(meta.id).catch(() => null);
      if (cached) {
        const [spec, report] = cached;
        set((s) => ({ pii: { ...s.pii, spec, report } }));
      }
    },

    // ----- outlier finder (F30) --------------------------------------------------

    startOutlierScan: async (spec) => {
      const meta = activeMeta();
      if (!meta || get().outlier.scanJobId != null) return;
      try {
        const jobId = await api.startOutlierScan(meta.id, spec, meta.revision);
        set((s) => ({
          outlier: {
            ...s.outlier,
            scanJobId: jobId,
            processed: 0,
            total: null,
            spec,
            // The old report pairs with the PREVIOUS spec; if this scan
            // fails or is cancelled, actions must not re-enable against a
            // report whose rows the new spec never selected.
            report: null,
            error: null,
          },
        }));
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ outlier: { ...s.outlier, error: String(e) } }));
      }
    },

    cancelOutlierScan: async () => {
      const jobId = get().outlier.scanJobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    clearOutlierReport: () => set({ outlier: initialOutlier }),

    loadCachedOutlierReport: async () => {
      const meta = activeMeta();
      if (!meta || get().outlier.report !== null || get().outlier.scanJobId != null) return;
      const cached = await api.getOutlierReport(meta.id).catch(() => null);
      if (cached) {
        const [spec, report] = cached;
        set((s) => ({ outlier: { ...s.outlier, spec, report } }));
      }
    },

    applyOutlierFilter: async () => {
      const meta = activeMeta();
      const { spec, report } = get().outlier;
      if (!meta || !spec || !report) return false;
      try {
        const updated = await api.applyOutlierFilter(meta.id, spec, report.revision);
        reloadDoc(updated);
        return true;
      } catch (e) {
        set((s) => ({ outlier: { ...s.outlier, error: String(e) } }));
        return false;
      }
    },

    // ----- compressed files (F17) --------------------------------------------

    startArchiveExtract: async (path, entry, allowLarge) => {
      if (get().indexing) return;
      try {
        const started = await api.startArchiveExtract(path, entry, allowLarge);
        set({
          indexing: {
            jobId: started.jobId,
            docId: 0,
            kind: "archiveExtract",
            path,
            processed: 0,
            total: null, // decompressed size is unknown up front
            archiveToken: started.token,
            archiveEntry: entry,
          },
          error: null,
        });
        consumeEarlyFinish(started.jobId);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    pickArchiveEntry: async (entry) => {
      const pick = get().archivePick;
      if (!pick) return;
      set({ archivePick: null });
      await get().startArchiveExtract(pick.path, entry, false);
    },

    dismissArchivePick: () => set({ archivePick: null }),

    confirmArchiveLarge: async () => {
      const confirm = get().archiveLargeConfirm;
      if (!confirm) return;
      set({ archiveLargeConfirm: null });
      await get().startArchiveExtract(confirm.path, confirm.entry, true);
    },

    dismissArchiveLarge: () => set({ archiveLargeConfirm: null }),

    refreshActiveDoc: async () => {
      const id = get().activeId;
      if (id == null) return;
      try {
        reloadDoc(await api.getMeta(id));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    newDoc: async () => {
      try {
        const meta = await api.newDocument(50, 4);
        set((s) => ({ ...switchPatch(s, meta.id), tabs: [...s.tabs, meta] }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    closeTab: async (id) => {
      await api.closeDocument(id).catch(() => undefined);
      set((s) => {
        const tabs = s.tabs.filter((t) => t.id !== id);
        const closingActive = s.activeId === id;
        const nextActive = closingActive
          ? tabs.length
            ? tabs[tabs.length - 1].id
            : null
          : s.activeId;
        // Switch first (snapshots the outgoing active state), then drop every
        // trace of the closed document's transient state.
        const patch = closingActive ? switchPatch({ ...s, tabs } as Store, nextActive) : {};
        const uiStates = { ...(patch.uiStates ?? s.uiStates) };
        delete uiStates[id];
        const diagnostics = { ...s.diagnostics };
        delete diagnostics[id];
        const ignoredFingerprints = { ...s.ignoredFingerprints };
        delete ignoredFingerprints[id];
        // Only invalidate the grid cache when the active document actually
        // changed; closing a background tab must not refetch the active grid.
        return {
          ...patch,
          tabs,
          activeId: nextActive,
          uiStates,
          diagnostics,
          ignoredFingerprints,
          externalPrompt: s.externalPrompt?.docId === id ? null : s.externalPrompt,
          profileSuggestion: s.profileSuggestion?.docId === id ? null : s.profileSuggestion,
          dataVersion: closingActive ? s.dataVersion + 1 : s.dataVersion,
        };
      });
    },

    setHeaderMode: (hasHeader) => {
      // Promoting/demoting the header row re-interprets every column.
      set({ summaries: null, summariesDocId: null });
      return mutate((id) => api.setHeaderMode(id, hasHeader));
    },

    setCell: (row, col, value) => mutate((id) => api.setCell(id, row, col, value), false),
    pasteBlock: (row, col, block) => mutate((id) => api.paste(id, row, col, block)),
    insertRows: (at, count) => mutate((id) => api.insertRows(id, at, count)),
    deleteRows: (indices) => mutate((id) => api.deleteRows(id, indices)),
    moveRow: (from, to) => mutate((id) => api.moveRow(id, from, to)),
    insertColumn: (at, name) => {
      // Column identity shifts: invalidate summaries and keep the frozen
      // boundary on the same logical columns (shift it right if we inserted
      // within the frozen region).
      set((s) => ({
        summaries: null,
        summariesDocId: null,
        frozenColumnCount: at < s.frozenColumnCount ? s.frozenColumnCount + 1 : s.frozenColumnCount,
      }));
      return mutate((docId) => api.insertColumn(docId, at, name));
    },
    deleteColumns: (indices) => {
      set((s) => {
        const removedBelow = indices.filter((c) => c < s.frozenColumnCount).length;
        return {
          summaries: null,
          summariesDocId: null,
          frozenColumnCount: Math.max(0, s.frozenColumnCount - removedBelow),
        };
      });
      return mutate((docId) => api.deleteColumns(docId, indices));
    },
    renameColumn: (col, name) => mutate((id) => api.renameColumn(id, col, name)),
    sortBy: (keys) => mutate((id) => api.sort(id, keys)),
    undo: () => mutate((id) => api.undo(id)),
    redo: () => mutate((id) => api.redo(id)),

    saveActive: async (saveAs, exportOptions) => {
      const id = get().activeId;
      if (id == null) return;
      set({ busy: true });
      await saveDocById(id, saveAs, exportOptions);
      set({ busy: false });
    },

    // ----- find / replace -------------------------------------------------

    setFindOpen: (open) =>
      set((s) => ({ find: { ...s.find, open, matches: open ? s.find.matches : [] } })),

    updateFind: (patch) => set((s) => ({ find: { ...s.find, ...patch } })),

    runFind: async () => {
      const id = get().activeId;
      const { find, selectionRect, tabs } = get();
      if (id == null || find.query === "") {
        set((s) => ({ find: { ...s.find, matches: [], index: 0 } }));
        return;
      }
      const activeTab = tabs.find((t) => t.id === id);
      const options: FindOptions = {
        query: find.query,
        regex: find.regex,
        caseSensitive: find.caseSensitive,
        wholeCell: find.wholeCell,
        selection: find.inSelection && selectionRect ? selectionRect : undefined,
        // Indexed documents can be enormous: cap the match list so a broad
        // query cannot materialise millions of hits (F10).
        limit: activeTab?.backing === "indexedReadOnly" ? INDEXED_FIND_LIMIT : undefined,
      };
      try {
        const matches = await api.find(id, options);
        set((s) => ({ find: { ...s.find, matches, index: 0 } }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    gotoMatch: (delta) =>
      set((s) => {
        const n = s.find.matches.length;
        if (n === 0) return {};
        const index = (s.find.index + delta + n) % n;
        return { find: { ...s.find, index } };
      }),

    replaceCurrent: async () => {
      const id = get().activeId;
      const { find, selectionRect } = get();
      const match = find.matches[find.index];
      if (id == null || !match) return;
      try {
        const win = await api.getRows(id, match.row, 1);
        const current = win.rows[0]?.[match.col] ?? "";
        const options: FindOptions = {
          query: find.query,
          regex: find.regex,
          caseSensitive: find.caseSensitive,
          wholeCell: find.wholeCell,
        };
        const next = applyReplace(current, options, find.replacement);
        if (next !== current) {
          const meta = await api.setCell(id, match.row, match.col, next);
          reloadDoc(meta);
        }
        // Recompute matches and advance to the first one AFTER the replaced cell,
        // so sequential Replace moves forward (and doesn't loop on a replacement
        // that still matches the query).
        const matches = await api.find(id, {
          ...options,
          selection: find.inSelection && selectionRect ? selectionRect : undefined,
        });
        let index = matches.findIndex(
          (m) => m.row > match.row || (m.row === match.row && m.col > match.col),
        );
        if (index < 0) index = 0;
        set((s) => ({ find: { ...s.find, matches, index: matches.length ? index : 0 } }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    replaceAllMatches: async () => {
      const id = get().activeId;
      const { find, selectionRect } = get();
      if (id == null || find.query === "") return;
      const options: FindOptions = {
        query: find.query,
        regex: find.regex,
        caseSensitive: find.caseSensitive,
        wholeCell: find.wholeCell,
        selection: find.inSelection && selectionRect ? selectionRect : undefined,
      };
      try {
        const result = await api.replaceAll(id, options, find.replacement);
        reloadDoc(result.meta);
        set((s) => ({ find: { ...s.find, matches: [], index: 0 } }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    // ----- filter ---------------------------------------------------------

    setFilterOpen: (open) => set((s) => ({ filter: { ...s.filter, open } })),

    updateFilterSpec: (spec) => set((s) => ({ filter: { ...s.filter, spec } })),

    // Applying/clearing a filter changes the visible row set, so both go through
    // reloadDoc (bumping dataVersion) to refetch the grid against the new view.
    applyFilter: async (spec) => {
      set((s) => ({ filter: { ...s.filter, spec } }));
      await mutate((id) => api.setFilter(id, spec));
    },

    clearFilter: () => mutate((id) => api.clearFilter(id)),

    // ----- diagnostics ------------------------------------------------------

    setDiagnosticsOpen: (open) =>
      set((s) => ({
        diagnosticsOpen: open,
        // The side area shows one panel at a time.
        explorer: open ? { ...s.explorer, open: false } : s.explorer,
      })),

    runDiagnosticsScan: async () => {
      const meta = activeMeta();
      if (!meta) return;
      const existing = get().diagnostics[meta.id];
      if (existing?.jobId != null) return; // a scan is already running
      try {
        const jobId = await api.startDiagnosticsScan(meta.id, meta.revision);
        patchDiagnostics(meta.id, {
          jobId,
          processed: 0,
          total: null,
          scanError: null,
          cancelled: false,
        });
        consumeEarlyFinish(jobId);
      } catch (e) {
        patchDiagnostics(meta.id, { scanError: String(e) });
      }
    },

    cancelDiagnosticsScan: async () => {
      const id = get().activeId;
      if (id == null) return;
      const jobId = get().diagnostics[id]?.jobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    applyIssueFilter: async (issueId) => {
      const meta = activeMeta();
      if (!meta) return;
      try {
        const updated = await api.applyDiagnosticFilter(meta.id, issueId, meta.revision);
        reloadDoc(updated);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    jumpToCell: async (row, col) => {
      const meta = activeMeta();
      if (!meta) return;
      // Diagnostic samples use absolute row indices; a jump while filtered
      // would land on the wrong (display) row and the target may be hidden
      // anyway, so drop the filter first.
      if (meta.filtered) {
        try {
          const updated = await api.clearFilter(meta.id);
          reloadDoc(updated);
        } catch (e) {
          set({ error: String(e) });
          return;
        }
      }
      jumpNonce += 1;
      set({ jumpTarget: { row, col, nonce: jumpNonce } });
    },

    // ----- background-job events -------------------------------------------

    handleJobProgress: (progress) => {
      // Archive extraction (F17) runs before any document exists, so its
      // job carries no docId — route it by job id BEFORE the docId gate.
      if (
        progress.kind === "openIndexed" ||
        progress.kind === "convertEditable" ||
        progress.kind === "reindex" ||
        progress.kind === "archiveExtract"
      ) {
        if (get().indexing?.jobId !== progress.jobId) return;
        set((s) => ({
          indexing: s.indexing && {
            ...s.indexing,
            processed: progress.processed,
            total: progress.total ?? s.indexing.total,
          },
        }));
        return;
      }

      if (progress.docId == null) return;
      const docId = progress.docId;

      if (progress.kind === "save" || progress.kind === "export") {
        set((s) => ({
          fileJobs: {
            ...s.fileJobs,
            [progress.jobId]: {
              jobId: progress.jobId,
              docId,
              kind: progress.kind as "save" | "export",
              processed: progress.processed,
              total: progress.total,
              bytesWritten: progress.bytesWritten,
              part: progress.part,
            },
          },
        }));
        return;
      }

      if (progress.kind === "profile") {
        if (get().explorer.jobId !== progress.jobId) return;
        set((s) => ({
          explorer: { ...s.explorer, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      if (progress.kind === "dedup") {
        if (get().dedup.scanJobId !== progress.jobId) return;
        set((s) => ({
          dedup: { ...s.dedup, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      if (progress.kind === "compare") {
        if (get().compare.jobId !== progress.jobId) return;
        set((s) => ({
          compare: { ...s.compare, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      if (progress.kind === "semantic") {
        if (get().semantic.scanJobId !== progress.jobId) return;
        set((s) => ({
          semantic: { ...s.semantic, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      if (progress.kind === "crossval") {
        if (get().crossval.scanJobId !== progress.jobId) return;
        set((s) => ({
          crossval: { ...s.crossval, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      if (progress.kind === "outlier") {
        if (get().outlier.scanJobId !== progress.jobId) return;
        set((s) => ({
          outlier: { ...s.outlier, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      if (progress.kind === "derive") {
        const derive = get().derive;
        if (derive?.jobId !== progress.jobId) return;
        set({
          derive: {
            ...derive,
            processed: progress.processed,
            total: progress.total,
            message: progress.message ?? derive.message,
          },
        });
        return;
      }

      if (progress.kind === "pii") {
        if (get().pii.scanJobId !== progress.jobId) return;
        set((s) => ({
          pii: { ...s.pii, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      if (progress.kind === "batch") {
        const batch = get().batch;
        if (batch?.jobId !== progress.jobId) return;
        set({
          batch: {
            ...batch,
            processed: progress.processed,
            total: progress.total,
            message: progress.message ?? batch.message,
          },
        });
        return;
      }

      if (progress.kind === "cluster") {
        if (get().cluster.scanJobId !== progress.jobId) return;
        set((s) => ({
          cluster: { ...s.cluster, processed: progress.processed, total: progress.total },
        }));
        return;
      }

      // The indexing-family branch (openIndexed / convertEditable / reindex /
      // archiveExtract) is handled at the TOP of this handler, before the
      // docId gate — extraction jobs have no document yet.
      if (progress.kind !== "diagnostics") return;
      // Only track the scan we started (guards against reused ids after e.g.
      // an app-side restart of the job system).
      if (get().diagnostics[docId]?.jobId !== progress.jobId) return;
      patchDiagnostics(docId, { processed: progress.processed, total: progress.total });
    },

    handleJobFinished: async (finished) => {
      // Buffer first: a start-job invoke that has not resolved yet finds the
      // event here (see rememberFinished). Ids are unique, so a consumed
      // entry lingering until FIFO eviction is harmless.
      rememberFinished(finished);
      // Resolve any promise waiting on this job (save/export flows).
      const waiter = jobWaiters.get(finished.jobId);
      if (waiter) {
        jobWaiters.delete(finished.jobId);
        waiter(finished);
      }
      if (finished.kind === "save" || finished.kind === "export") {
        set((s) => {
          const fileJobs = { ...s.fileJobs };
          delete fileJobs[finished.jobId];
          return { fileJobs };
        });
        return;
      }

      if (finished.kind === "archiveExtract") {
        const indexing = get().indexing;
        if (indexing?.jobId !== finished.jobId) return;
        const token = indexing.archiveToken;
        const path = indexing.path ?? "";
        const entry = indexing.archiveEntry ?? null;
        set({ indexing: null });
        if (finished.status !== "done") {
          if (finished.status === "failed") {
            const message = finished.error ?? "extraction failed";
            if (message.includes("suspicious compression ratio")) {
              // The ratio guard tripped: offer an explicit confirmation.
              set({ archiveLargeConfirm: { path, entry } });
            } else {
              set({ error: message });
            }
          }
          return;
        }
        if (token == null) return;
        try {
          // Same decision flow as a plain-file open, over the extracted file.
          const estimate = await api.pendingArchiveEstimate(token);
          if (estimate.needsDecision) {
            set({ openDecision: { path, estimate, archiveToken: token } });
            return;
          }
          const started = await api.openArchiveDocument(token, "editable");
          const meta = await api.getMeta(started.docId);
          set((s) => ({ ...switchPatch(s, meta.id), tabs: [...s.tabs, meta] }));
          pushRecent(path);
        } catch (e) {
          set({ error: String(e) });
        }
        return;
      }

      if (
        finished.kind === "openIndexed" ||
        finished.kind === "convertEditable" ||
        finished.kind === "reindex"
      ) {
        const indexing = get().indexing;
        if (indexing?.jobId !== finished.jobId) return;
        set({ indexing: null });
        if (finished.status !== "done") {
          if (finished.status === "failed") {
            set({ error: finished.error ?? "indexing failed" });
          }
          return;
        }
        try {
          const meta = await api.getMeta(indexing.docId);
          if (finished.kind === "openIndexed") {
            set((s) => ({ ...switchPatch(s, meta.id), tabs: [...s.tabs, meta] }));
            if (indexing.path) pushRecent(indexing.path);
          } else {
            // Conversion / re-index changed the document in place.
            reloadDoc(meta);
            if (finished.kind === "reindex" && get().activeId === meta.id) {
              // Reload replaced the contents; derived state (find matches,
              // cached summaries) points at rows that may be gone.
              set((s) => ({
                find: { ...s.find, matches: [], index: 0 },
                summaries: null,
                summariesDocId: null,
              }));
            }
          }
        } catch (e) {
          set({ error: String(e) });
        }
        return;
      }

      if (finished.kind === "compare") {
        if (get().compare.jobId !== finished.jobId) return;
        if (finished.status === "done") {
          const info = await api.getCompareInfo(finished.jobId).catch(() => null);
          set((s) => ({
            compare: { ...s.compare, jobId: null, compareId: finished.jobId, info },
          }));
        } else {
          set((s) => ({
            compare: {
              ...s.compare,
              jobId: null,
              error: finished.status === "failed" ? (finished.error ?? "comparison failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind === "semantic") {
        if (get().semantic.scanJobId !== finished.jobId) return;
        if (finished.status === "done") {
          const report = finished.docId
            ? await api.getSemanticReport(finished.docId).catch(() => null)
            : null;
          set((s) => ({ semantic: { ...s.semantic, scanJobId: null, report, error: null } }));
        } else {
          set((s) => ({
            semantic: {
              ...s.semantic,
              scanJobId: null,
              error: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind === "crossval") {
        if (get().crossval.scanJobId !== finished.jobId) return;
        if (finished.status === "done") {
          const cached = finished.docId
            ? await api.getCrossvalReport(finished.docId).catch(() => null)
            : null;
          set((s) => ({
            crossval: {
              ...s.crossval,
              scanJobId: null,
              rules: cached ? cached[0] : s.crossval.rules,
              report: cached ? cached[1] : null,
              error: null,
            },
          }));
        } else {
          set((s) => ({
            crossval: {
              ...s.crossval,
              scanJobId: null,
              error: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind === "pii") {
        if (get().pii.scanJobId !== finished.jobId) return;
        if (finished.status === "done") {
          const cached = finished.docId
            ? await api.getPiiReport(finished.docId).catch(() => null)
            : null;
          set((s) => ({
            pii: {
              ...s.pii,
              scanJobId: null,
              spec: cached ? cached[0] : s.pii.spec,
              report: cached ? cached[1] : null,
              error: null,
            },
          }));
        } else {
          set((s) => ({
            pii: {
              ...s.pii,
              scanJobId: null,
              error: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind === "batch") {
        const batch = get().batch;
        if (batch?.jobId !== finished.jobId) return;
        if (finished.status === "done") {
          const report = await api.getBatchReport(batch.jobId).catch(() => null);
          set({ batch: { ...batch, report, error: null } });
        } else {
          set({
            batch: {
              ...batch,
              error:
                finished.status === "failed" ? (finished.error ?? "the batch failed") : "cancelled",
            },
          });
        }
        return;
      }

      if (finished.kind === "derive") {
        const derive = get().derive;
        if (derive?.jobId !== finished.jobId) return;
        set({ derive: null });
        if (finished.status !== "done") {
          if (finished.status === "failed") {
            set({ deriveError: finished.error ?? "the operation failed" });
          }
          return;
        }
        try {
          // The job registered the NEW document; add its tab and focus it.
          const meta = await api.getMeta(derive.docId);
          set((s) => ({ ...switchPatch(s, meta.id), tabs: [...s.tabs, meta] }));
        } catch (e) {
          set({ deriveError: String(e) });
        }
        return;
      }

      if (finished.kind === "outlier") {
        if (get().outlier.scanJobId !== finished.jobId) return;
        if (finished.status === "done") {
          const cached = finished.docId
            ? await api.getOutlierReport(finished.docId).catch(() => null)
            : null;
          set((s) => ({
            outlier: {
              ...s.outlier,
              scanJobId: null,
              spec: cached ? cached[0] : s.outlier.spec,
              report: cached ? cached[1] : null,
              error: null,
            },
          }));
        } else {
          set((s) => ({
            outlier: {
              ...s.outlier,
              scanJobId: null,
              error: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind === "cluster") {
        if (get().cluster.scanJobId !== finished.jobId) return;
        if (finished.status === "done") {
          // A report belongs to the document it was scanned on; if the user
          // switched tabs meanwhile, drop it rather than install it against
          // a different document (values could coincidentally match).
          if (finished.docId !== get().activeId) {
            set((s) => ({ cluster: { ...s.cluster, scanJobId: null } }));
            return;
          }
          const report = finished.docId
            ? await api.getClusterReport(finished.docId).catch(() => null)
            : null;
          set((s) => ({ cluster: { ...s.cluster, scanJobId: null, report, error: null } }));
        } else {
          set((s) => ({
            cluster: {
              ...s.cluster,
              scanJobId: null,
              error: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind === "dedup") {
        // Only the SCAN job feeds the report; apply jobs resolve via waiters.
        if (get().dedup.scanJobId !== finished.jobId) return;
        if (finished.status === "done") {
          const report = finished.docId
            ? await api.getDuplicateReport(finished.docId).catch(() => null)
            : null;
          set((s) => ({ dedup: { ...s.dedup, scanJobId: null, report, error: null } }));
        } else {
          set((s) => ({
            dedup: {
              ...s.dedup,
              scanJobId: null,
              error: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind === "profile") {
        if (get().explorer.jobId !== finished.jobId) return;
        if (finished.status === "done") {
          // The job cached its result; fetch it (still-valid or recompute).
          set((s) => ({ explorer: { ...s.explorer, jobId: null } }));
          void get().refreshExplorerProfile();
        } else {
          set((s) => ({
            explorer: {
              ...s.explorer,
              jobId: null,
              error: finished.status === "failed" ? (finished.error ?? "profiling failed") : null,
            },
          }));
        }
        return;
      }

      if (finished.kind !== "diagnostics" || finished.docId == null) return;
      const docId = finished.docId;
      if (get().diagnostics[docId]?.jobId !== finished.jobId) return;
      if (finished.status === "done") {
        const report = await api.getDiagnostics(docId).catch(() => null);
        patchDiagnostics(docId, { jobId: null, report, scanError: null, cancelled: false });
      } else {
        patchDiagnostics(docId, {
          jobId: null,
          scanError: finished.status === "failed" ? (finished.error ?? "scan failed") : null,
          // Remember an explicit cancel so the panel does not immediately
          // auto-start another scan of the same document.
          cancelled: finished.status === "cancelled",
        });
      }
    },

    // ----- reopen with settings (F02) ---------------------------------------

    openReopenDialog: (initial = {}) => {
      const meta = activeMeta();
      if (!meta?.path) return; // nothing on disk to reopen
      set({
        reopen: { ...initialReopen, open: true, options: initial, loading: true },
      });
      void get().refreshReopenPreview();
    },

    closeReopenDialog: () => set({ reopen: initialReopen }),

    setReopenOptions: (patch) => {
      set((s) => ({
        reopen: { ...s.reopen, options: { ...s.reopen.options, ...patch } },
      }));
      void get().refreshReopenPreview();
    },

    refreshReopenPreview: async () => {
      const meta = activeMeta();
      const { reopen } = get();
      if (!meta || !reopen.open) return;
      const requested = reopen.options;
      set((s) => ({ reopen: { ...s.reopen, loading: true, error: null } }));
      try {
        const preview = await api.previewReparse(meta.id, requested, 100);
        // Discard if the options changed while this preview was in flight.
        if (get().reopen.options === requested) {
          set((s) => ({ reopen: { ...s.reopen, preview, loading: false } }));
        }
      } catch (e) {
        if (get().reopen.options === requested) {
          set((s) => ({ reopen: { ...s.reopen, error: String(e), loading: false } }));
        }
      }
    },

    confirmReopen: async (discard) => {
      const meta = activeMeta();
      const { reopen } = get();
      const preview = reopen.preview;
      if (!meta || !preview) return;

      // A dirty document must be saved (or explicitly discarded) first.
      let expectedRevision = preview.expectedRevision;
      if (meta.dirty && !discard) {
        const saved = await saveDocById(meta.id, false);
        if (!saved) return; // save cancelled or failed: abort, keep the dialog
        // The save just wrote the CURRENT document, so the preview (parsed
        // from the pre-save bytes) may no longer describe what a reparse
        // loads. Re-parse the fresh bytes and apply against that instead.
        await get().refreshReopenPreview();
        const fresh = get().reopen.preview;
        if (!fresh) return; // refresh failed; its error is already showing
        expectedRevision = fresh.expectedRevision;
      }

      try {
        // Saving does not bump the revision, so the (possibly refreshed)
        // preview stays valid here.
        const updated = await api.applyReparse(meta.id, reopen.options, expectedRevision);
        reloadDoc(updated);
        set({
          reopen: initialReopen,
          // The reopened document has fresh contents; stale matches would
          // point at cells that may no longer exist.
          find: { ...get().find, matches: [], index: 0 },
          summaries: null,
          summariesDocId: null,
        });
      } catch (e) {
        // Most likely a stale revision (an edit raced the dialog): surface it
        // and refresh the preview so the next confirm can succeed.
        set((s) => ({ reopen: { ...s.reopen, error: String(e) } }));
        void get().refreshReopenPreview();
      }
    },

    // ----- external changes (F02) -------------------------------------------

    checkExternalChanges: async () => {
      const { tabs, externalPrompt, ignoredFingerprints, reopen, quitPromptOpen, indexing } = get();
      // One dialog at a time; don't stack prompts over other modal flows.
      // A running index job (open/convert/reload) also suppresses checks: a
      // reindex refreshes the fingerprint only when it finishes.
      if (externalPrompt || reopen.open || quitPromptOpen || indexing) return;
      for (const tab of tabs) {
        if (!tab.path) continue;
        try {
          const change = await api.checkExternalChange(tab.id);
          if (!change.changed) continue;
          if (ignoredFingerprints[tab.id] === fingerprintKey(change.disk)) continue;
          set({ externalPrompt: { docId: tab.id, change } });
          return;
        } catch {
          // Stat failures are not worth interrupting the user for.
        }
      }
    },

    resolveExternalPrompt: async (action) => {
      const prompt = get().externalPrompt;
      if (!prompt) return;
      const meta = get().tabs.find((t) => t.id === prompt.docId);
      set({ externalPrompt: null });
      if (!meta) return;

      switch (action) {
        case "reload": {
          try {
            if (meta.backing === "indexedReadOnly") {
              // Indexed documents reload by re-scanning the file (job-based);
              // the meta refreshes when the job finishes.
              const jobId = await api.startReindex(meta.id);
              set({
                indexing: {
                  jobId,
                  docId: meta.id,
                  kind: "reindex",
                  path: meta.path,
                  processed: 0,
                  total: null,
                },
              });
              consumeEarlyFinish(jobId);
              // Return (not break): the shared tail would immediately re-run
              // checkExternalChanges, and the still-old fingerprint would
              // re-surface this same prompt while the reload job runs.
              return;
            }
            // Reload keeps the current parse settings; never offered (or
            // valid) for dirty documents.
            const updated = await api.applyReparse(
              meta.id,
              currentOpenOptions(meta),
              meta.revision,
            );
            reloadDoc(updated);
            // The disk contents replaced the document: derived state (find
            // matches, cached summaries) points at rows that may be gone.
            set((s) => ({
              find: { ...s.find, matches: [], index: 0 },
              summaries: null,
              summariesDocId: null,
            }));
          } catch (e) {
            set({ error: String(e) });
          }
          break;
        }
        case "ignore":
          set((s) => ({
            ignoredFingerprints: {
              ...s.ignoredFingerprints,
              [meta.id]: fingerprintKey(prompt.change.disk),
            },
          }));
          break;
        case "saveAs": {
          const saved = await saveDocById(meta.id, true);
          if (!saved) {
            // Nothing was written; re-surface the prompt on the next check.
          }
          break;
        }
        case "openDisk": {
          // Minimal "compare with disk": open the on-disk version as its own
          // tab next to the edited one (a structured diff arrives with F09).
          if (meta.path) {
            set((s) => ({
              ignoredFingerprints: {
                ...s.ignoredFingerprints,
                [meta.id]: fingerprintKey(prompt.change.disk),
              },
            }));
            try {
              const opened = await api.openFile(meta.path);
              set((s) => ({ tabs: [...s.tabs, opened], activeId: opened.id }));
            } catch (e) {
              set({ error: String(e) });
            }
          }
          break;
        }
      }
      // Surface the next pending change, if any.
      void get().checkExternalChanges();
    },

    // ----- quit flow (F02) ----------------------------------------------------

    setQuitPromptOpen: (open) => set({ quitPromptOpen: open }),

    confirmQuit: async (mode) => {
      if (mode === "save") {
        const dirty = get().tabs.filter((t) => t.dirty);
        for (const tab of dirty) {
          const saved = await saveDocById(tab.id, false);
          if (!saved) {
            // Abort quitting: a save failed or was cancelled.
            set({
              quitPromptOpen: false,
              error: `Quit cancelled — “${tab.fileName}” was not saved.`,
            });
            return;
          }
        }
      }
      set({ quitPromptOpen: false });
      await getCurrentWindow().destroy();
    },

    // ----- save / export pipeline (F03) ---------------------------------------

    resolveEncodingIssues: async (retryEncoding) => {
      const prompt = get().encodingIssues;
      set({ encodingIssues: null });
      if (!prompt || retryEncoding === null) return;
      const options: ExportOptions = { ...prompt.options, encoding: retryEncoding };
      const scope = prompt.action.type === "export" ? prompt.action.scope : undefined;
      // Re-run the gate for the new encoding (a no-op for Unicode targets).
      if (isLegacyEncoding(retryEncoding)) {
        try {
          const compat = await api.checkEncodingCompatibility(
            prompt.docId,
            retryEncoding,
            scope,
            options.includeHeaders,
          );
          if (!compat.compatible) {
            set({ encodingIssues: { ...prompt, options, compat } });
            return;
          }
        } catch (e) {
          set({ error: String(e) });
          return;
        }
      }
      if (prompt.action.type === "save") {
        await runSaveJob(prompt.docId, prompt.path, options);
      } else {
        const { scope: s, split, writeManifest } = prompt.action;
        await runExportJob(prompt.docId, prompt.path, options, s, split, writeManifest);
      }
    },

    cancelFileJob: async (jobId) => {
      await api.cancelJob(jobId).catch(() => undefined);
    },

    exportScoped: async (options, scope, split, writeManifest) => {
      const meta = activeMeta();
      if (!meta) return;
      const chosen = await saveFileDialog({
        defaultPath: meta.fileName,
        filters: FILE_FILTERS,
      });
      if (!chosen) return;

      // Gate lossy encodings against exactly the cells this export writes.
      if (isLegacyEncoding(options.encoding)) {
        try {
          const compat = await api.checkEncodingCompatibility(
            meta.id,
            options.encoding,
            scope,
            options.includeHeaders,
          );
          if (!compat.compatible) {
            set({
              encodingIssues: {
                docId: meta.id,
                path: chosen,
                options,
                compat,
                action: { type: "export", scope, split, writeManifest },
              },
            });
            return;
          }
        } catch (e) {
          set({ error: String(e) });
          return;
        }
      }

      // Remember the options per document, to seed the next export dialog.
      set({ lastExportOptions: options });
      await runExportJob(meta.id, chosen, options, scope, split, writeManifest);
    },

    // ----- compare (F09) -----------------------------------------------------------

    runCompare: async (rightDocId, spec) => {
      const left = activeMeta();
      const right = get().tabs.find((t) => t.id === rightDocId);
      if (!left || !right || get().compare.jobId != null) return;
      try {
        const jobId = await api.startCompare(
          left.id,
          rightDocId,
          spec,
          left.revision,
          right.revision,
        );
        set({
          compare: { ...initialCompare, jobId },
        });
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ compare: { ...s.compare, error: String(e) } }));
      }
    },

    cancelCompare: async () => {
      const jobId = get().compare.jobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    clearCompare: () => set({ compare: initialCompare }),

    exportCompare: async (which) => {
      const { compare } = get();
      if (compare.compareId == null || !compare.info) return;
      const left = get().tabs.find((t) => t.id === compare.info?.leftDoc);
      const suggested =
        which === "report"
          ? `${left?.fileName ?? "compare"}.changes.json`
          : `${left?.fileName ?? "compare"}.${which}.csv`;
      const chosen = await saveFileDialog({ defaultPath: suggested, filters: FILE_FILTERS });
      if (!chosen) return;
      const options: ExportOptions = {
        delimiter: left?.delimiter || ",",
        encoding: "UTF-8",
        quoteStyle: "minimal",
        lineEnding: left?.lineEnding ?? "lf",
        bom: false,
        includeHeaders: true,
        backup: "none",
      };
      try {
        const jobId = await api.startCompareExport(compare.compareId, which, chosen, options);
        const finished = await awaitJob(jobId);
        if (finished.status === "failed") {
          set({ error: finished.error ?? "export failed" });
        }
      } catch (e) {
        set({ error: String(e) });
      }
    },

    jumpToDocCell: async (docId, row) => {
      if (get().activeId !== docId && get().tabs.some((t) => t.id === docId)) {
        get().setActive(docId);
      }
      await get().jumpToCell(row, 0);
    },

    // ----- duplicate finder (F07) -------------------------------------------------

    startDuplicateScan: async (spec, scope) => {
      const meta = activeMeta();
      if (!meta || get().dedup.scanJobId != null) return;
      try {
        const jobId = await api.startDuplicateScan(meta.id, spec, scope, meta.revision);
        set((s) => ({
          dedup: { ...s.dedup, scanJobId: jobId, processed: 0, total: null, error: null },
        }));
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ dedup: { ...s.dedup, error: String(e) } }));
      }
    },

    cancelDuplicateScan: async () => {
      const jobId = get().dedup.scanJobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    clearDuplicateReport: () => set({ dedup: initialDedup }),

    filterToDuplicates: async (spec, scope) => {
      const meta = activeMeta();
      if (!meta) return;
      try {
        const updated = await api.applyDuplicateFilter(meta.id, spec, scope, meta.revision);
        reloadDoc(updated);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    applyDedup: async (spec, scope, keep, expectedRevision) => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        const jobId = await api.applyDeduplicate(meta.id, spec, scope, keep, expectedRevision);
        const finished = await awaitJob(jobId);
        if (finished.status !== "done") {
          if (finished.status === "failed") {
            set({ error: finished.error ?? "deduplication failed" });
          }
          return false;
        }
        const updated = await api.getMeta(meta.id);
        reloadDoc(updated);
        set((s) => ({ dedup: { ...s.dedup, report: null } }));
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    // ----- data-cleaning transforms (F06) ----------------------------------------

    applyTransformSpec: async (spec, scope, policy, expectedRevision) => {
      const meta = activeMeta();
      if (!meta) return false;
      const hadFilter = meta.filtered;
      // Capture THIS document's filter spec now: by the time the job ends the
      // user may have switched tabs (restoring another document's filter
      // state) or closed the dialog.
      const filterSpec = get().filter.spec;
      try {
        const jobId = await api.applyTransform(meta.id, spec, scope, policy, expectedRevision);
        const finished = await awaitJob(jobId);
        if (finished.status !== "done") {
          if (finished.status === "failed") {
            set({ error: finished.error ?? "transform failed" });
          }
          return false;
        }
        const updated = await api.getMeta(meta.id);
        reloadDoc(updated);
        // The backend dropped the filter view before committing; recompute it
        // from the captured filter spec so the user's view survives the edit.
        if (hadFilter) {
          try {
            const refiltered = await api.setFilter(meta.id, filterSpec);
            reloadDoc(refiltered);
          } catch {
            // The spec may reference a column the transform removed.
          }
        }
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    // ----- column explorer (F05) ------------------------------------------------

    setExplorerOpen: (open) => {
      set((s) => ({
        explorer: { ...s.explorer, open },
        // The side area shows one panel at a time.
        diagnosticsOpen: open ? false : s.diagnosticsOpen,
      }));
      if (open) void get().refreshExplorerProfile();
    },

    setExplorerColumn: (column) => {
      set({ activeExplorerColumn: column });
      void get().refreshExplorerProfile();
    },

    setExplorerScope: (scope) => {
      set((s) => ({ explorer: { ...s.explorer, scope } }));
      void get().refreshExplorerProfile();
    },

    refreshExplorerProfile: async () => {
      const meta = activeMeta();
      const { explorer, activeExplorerColumn } = get();
      if (!meta || !explorer.open || meta.colCount === 0) return;
      const column = Math.min(activeExplorerColumn ?? 0, meta.colCount - 1);
      try {
        // Served from the per-column cache whenever it is still valid.
        const cached = await api.getColumnProfile(meta.id, column, explorer.scope);
        if (cached) {
          set((s) => ({
            explorer: { ...s.explorer, profile: cached, jobId: null, error: null },
          }));
          return;
        }
        const jobId = await api.startColumnProfile(meta.id, column, explorer.scope, meta.revision);
        set((s) => ({
          explorer: { ...s.explorer, jobId, processed: 0, total: null, error: null },
        }));
        consumeEarlyFinish(jobId);
      } catch (e) {
        set((s) => ({ explorer: { ...s.explorer, error: String(e), jobId: null } }));
      }
    },

    cancelExplorerProfile: async () => {
      const jobId = get().explorer.jobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    applyValueFilter: async (value, mode) => {
      const { activeExplorerColumn, filter } = get();
      const column = activeExplorerColumn ?? 0;
      const condition = valueCondition(column, value, mode === "exclude");
      const spec =
        mode === "and" ? withAndConditions(filter.spec, [condition]) : specOf([condition]);
      set((s) => ({ filter: { ...s.filter, spec } }));
      await mutate((id) => api.setFilter(id, spec));
      void get().refreshExplorerProfile();
    },

    applyRangeFilter: async (min, max) => {
      const { activeExplorerColumn, filter } = get();
      const column = activeExplorerColumn ?? 0;
      const conditions = rangeConditions(column, min, max);
      if (conditions.length === 0) return;
      const spec = withAndConditions(filter.spec, conditions);
      set((s) => ({ filter: { ...s.filter, spec } }));
      await mutate((id) => api.setFilter(id, spec));
      void get().refreshExplorerProfile();
    },

    // ----- file profiles (F08) -------------------------------------------------

    saveProfiles: async (profiles) => {
      const settings: AppSettings = {
        ...(get().settings ?? { version: 1, profiles: [] }),
        profiles,
      };
      set({ settings });
      try {
        await api.setSettings(settings);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    applyProfile: (profile) => {
      // Manual application always goes through the previewed reopen flow, so
      // a dirty document gets its Save/Discard/Cancel confirmation (F02).
      set({ profileSuggestion: null });
      get().openReopenDialog({
        delimiter: profile.delimiter ?? undefined,
        encoding: profile.encoding ?? undefined,
        hasHeaderRow: profile.hasHeaderRow ?? undefined,
      });
    },

    dismissProfileSuggestion: () => set({ profileSuggestion: null }),

    runProfileValidation: async (profile) => {
      const meta = activeMeta();
      if (!meta) return;
      try {
        const validation = await api.validateProfile(meta.id, profile);
        set({ profileValidation: validation });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    clearProfileValidation: () => set({ profileValidation: null }),
  };
});

/** Convenience selector for the active document's metadata. */
export function useActiveMeta(): DocumentMeta | null {
  return useStore((s) => s.tabs.find((t) => t.id === s.activeId) ?? null);
}
