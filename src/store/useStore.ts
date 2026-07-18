import { create } from "zustand";
import { ask, open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";

import * as api from "../lib/tauri";
import { applyReplace } from "../lib/replace";
import { rangeConditions, specOf, valueCondition, withAndConditions } from "../lib/explorer";
import { matchingProfiles, profileFromDocument, profileSettingsDiffer } from "../lib/profiles";
import {
  contiguousRuns,
  emptyLayout,
  layoutIsTrivial,
  layoutOfView,
  projectColumns,
  remapFilterColumns,
  resolveSortKeys,
  widthsFromIds,
  type ColumnLayout,
} from "../lib/viewProjection";
import { hydrateFilter, snapshotView, uniqueViewName, upsertView } from "../lib/views";
import { annotationExportName } from "../lib/annotations";
import { currentOpenOptions, fingerprintKey } from "../lib/reopen";
import { defaultImportOptions } from "../lib/jsonImport";
import { suggestJsonFileName } from "../lib/jsonExport";
import { isLegacyEncoding } from "../lib/save";
import {
  availableOnlyChoices,
  buildAnnotationsSection,
  buildLayoutSection,
  buildResolutions,
  buildSources,
  buildTabsSection,
  buildViewsSection,
  deriveProjectDirty,
  gatingWarnings,
  orderTabsForPlan,
  panelsFromLayout,
  pathKey,
  projectSnapshot,
  type PanelLayout,
  type ProjectSnapshot,
  type SourceAnnotationsSection,
  type SourceChoice,
  type SourceViewsSection,
} from "../lib/project";
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
  JsonExportOptions,
  JsonImportOptions,
  JsonImportPreview,
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
  FollowAlert,
  FollowAlertKind,
  FollowUpdate,
  NamedView,
  RecoverableSession,
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
  ColumnSchema,
  ConvertPreview,
  InvalidSampleReport,
  SchemaInfo,
  ProjectMeta,
  ProjectOpenPreview,
  ProjectOpenPlan,
  FileFingerprint,
  DictionaryField,
  DictionaryFormat,
  DictionaryImportOutcome,
  DictionaryView,
  MergeMatchBy,
  MergePlan,
  MergeResolution,
  AnnotationsExport,
  AnnotationsView,
  AnnotationPredicate,
  AnnotationExportFormat,
  KeySpec,
  RowMarkPatch,
  TagDef,
  TagToColumnPreview,
  TagToColumnTarget,
  RecordLayout,
} from "../types";
import type { GatingWarning } from "../lib/project";
import { clampRecord, type RecordDraft } from "../lib/recordForm";

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
  | "pii"
  | "recovery"
  | "dialect"
  | "schema"
  | "dictionary"
  | "jsonExport"
  | "sampling"
  | "tagToColumn"
  | "annotationExport"
  | "views";

const FILE_FILTERS = [
  { name: "Delimited text", extensions: ["csv", "tsv", "tab", "txt", "psv", "dat"] },
  { name: "JSON (F33)", extensions: ["json", "jsonl", "ndjson"] },
  { name: "Compressed (F17)", extensions: ["gz", "zip"] },
  { name: "All files", extensions: ["*"] },
];

/** File filters for a JSON / JSON Lines export target (F33). */
const JSON_FILE_FILTERS = [
  { name: "JSON", extensions: ["json"] },
  { name: "JSON Lines", extensions: ["jsonl", "ndjson"] },
  { name: "All files", extensions: ["*"] },
];

/** File filter for the project open/save dialogs (F37). */
const PROJECT_FILTERS = [{ name: "CEESVEE project", extensions: ["ceesveeproj"] }];

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
let autoFitNonce = 0;

/** Merge per-run selection statistics (F12: split by physical column runs). */
function mergeStats(parts: SelectionInfo[]): SelectionInfo {
  const count = parts.reduce((a, p) => a + p.count, 0);
  const numericCount = parts.reduce((a, p) => a + p.numericCount, 0);
  const sum = parts.reduce((a, p) => a + p.sum, 0);
  const mins = parts.map((p) => p.min).filter((m): m is number => m !== null);
  const maxs = parts.map((p) => p.max).filter((m): m is number => m !== null);
  return {
    count,
    numericCount,
    sum,
    avg: numericCount > 0 ? sum / numericCount : null,
    min: mins.length > 0 ? Math.min(...mins) : null,
    max: maxs.length > 0 ? Math.max(...maxs) : null,
  };
}

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
  /** F12: hidden/pinned/reordered columns, by stable column ID. */
  columnLayout: ColumnLayout | null;
  /** F12: wrap long cell text (taller rows). */
  wrapText: boolean;
  /** F12: the named view last applied to this document, if any. */
  activeViewId: string | null;
  /** F12: the applied non-destructive sort, in physical columns. */
  viewSortKeys: SortKey[];
  /** F12: dismissible missing-column warning from the last view apply. */
  viewWarning: string | null;
  /** F41: per-document record-form position, draft and layout. */
  record: RecordUiState;
}

/**
 * The per-document record-form state (F41): which record is shown, the unsaved
 * field draft, and the persisted layout. Snapshotted/restored with the rest of
 * the document UI state on a tab switch, so a draft or grouping survives it.
 */
export interface RecordUiState {
  /** Current visible (display) row shown in the form. */
  row: number;
  /** Drafted raw values keyed by grid column position (empty = clean). */
  draft: RecordDraft;
  /** Document revision the current draft was started against (null = clean).
   * A refetch at a different revision discards the draft (the row may have
   * remapped under a changed filter), so an edit never lands on a moved row. */
  draftRevision: number | null;
  /** Persisted per-document form layout (null = automatic, schema order). */
  layout: RecordLayout | null;
}

/** A fresh, clean record-form state (fresh objects — never shared). */
const initialRecordUi = (): RecordUiState => ({
  row: 0,
  draft: {},
  draftRevision: null,
  layout: null,
});

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
  kind: "append" | "join" | "groupBy" | "reshape" | "jsonImport";
  processed: number;
  total: number | null;
  message: string | null;
}

/**
 * JSON / JSON Lines import flow state (F33). Non-null while the import dialog
 * is open. The preview scan runs through the job registry (progress + cancel);
 * the apply step reuses the shared `derive` slot (kind "jsonImport"), so the
 * finished document lands via the same pipeline as every other producer.
 */
export interface JsonImportState {
  path: string;
  fileName: string;
  /** The options the current `preview` was scanned under. */
  options: JsonImportOptions;
  /** In-flight preview scan job id; null when idle. */
  scanJobId: number | null;
  scanProcessed: number;
  scanTotal: number | null;
  preview: JsonImportPreview | null;
  scanError: string | null;
}

/** A running sampling/partitioning job (F48). Unlike a derive job it can emit
 * MANY new documents (one per partition) or none at all (a direct export). */
export interface SampleState {
  jobId: number;
  /** Ids the new derived documents will register under; empty for exports. */
  docIds: number[];
  destination: "derivedDocuments" | "export";
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

/** A running canonical column-conversion job (F31), for the ACTIVE document. */
export interface SchemaConvertState {
  jobId: number;
  /** The column being converted (stable ID). */
  columnId: string;
  processed: number;
  total: number | null;
}

/** In-flight cancellable schema-scan job (F31): inference, invalid-value
 * samples, or a conversion preview — all full-column/full-document scans that
 * run through the job registry so they report progress and can be cancelled. */
export interface SchemaScanState {
  jobId: number;
  kind: "infer" | "invalid" | "preview";
  /** The column being scanned (stable ID); null for whole-document inference. */
  columnId: string | null;
  processed: number;
  total: number | null;
}

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
  /** F12: the ACTIVE document's column layout (hidden/pinned/order, by ID). */
  columnLayout: ColumnLayout | null;
  /** F12: wrap long cell text in the ACTIVE document's grid. */
  wrapText: boolean;
  /** F12: named view last applied to the ACTIVE document, if any. */
  activeViewId: string | null;
  /** F12: the ACTIVE document's non-destructive sort (physical columns). */
  viewSortKeys: SortKey[];
  /** F12: dismissible missing-column warning from the last view apply. */
  viewWarning: string | null;
  /** F41: the ACTIVE document's record-form position, draft and layout. */
  record: RecordUiState;
  /** Whether the record-form side panel is open (F41). */
  recordFormOpen: boolean;
  /** Open/close the record-form panel (one side panel is shown at a time). */
  setRecordFormOpen: (open: boolean) => void;
  /** Move the form to a visible record; resets the draft (a draft can't move). */
  setRecordRow: (row: number) => void;
  /** Set one field's drafted raw value (seeds the draft revision on first edit). */
  setRecordDraftField: (col: number, value: string) => void;
  /** Discard the whole record draft. */
  clearRecordDraft: () => void;
  /** Commit a record draft as ONE set_cells batch; clears the draft on success. */
  saveRecordDraft: (cells: [number, number, string][]) => Promise<boolean>;
  /** Replace the ACTIVE document's record-form layout (null = automatic). */
  setRecordLayout: (layout: RecordLayout | null) => void;
  /** Scroll the grid to a field's column on the current record's row (F41). */
  jumpToRecordColumn: (col: number) => void;
  /** Persist the F41 auto-save-on-navigate preference with the app settings. */
  setAutoSaveRecordOnNavigate: (enabled: boolean) => Promise<void>;
  /** F12: one-shot auto-fit request consumed by the grid. */
  autoFitRequest: { cols: number[] | "all"; nonce: number } | null;
  /** Saved UI state of every non-active document, keyed by document id. */
  uiStates: Record<number, DocumentUiState>;
  /** Detected per-column type + summary for the active doc (null until loaded). */
  summaries: ColumnSummary[] | null;
  /** Which document `summaries` belong to (guards against cross-tab staleness). */
  summariesDocId: number | null;
  /** Whether the diagnostics side panel is shown. */
  diagnosticsOpen: boolean;
  /** Whether the Changes panel is open (F15). */
  changesOpen: boolean;
  setChangesOpen: (open: boolean) => void;
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
  /** The ACTIVE document's explicit schema (F31), refreshed on load/edit. */
  schemaInfo: SchemaInfo | null;
  /** A running canonical column-conversion job (F31), if any. */
  schemaConvert: SchemaConvertState | null;
  /** A running cancellable schema-scan job — infer / invalid / preview (F31). */
  schemaScan: SchemaScanState | null;
  /** Physical column the schema dialog should focus on open (F31). */
  schemaDialogColumn: number | null;
  /** The ACTIVE document's data dictionary (F38), refreshed on load/edit.
   * Drives the editor and the Grid header tooltips. */
  dictionaryView: DictionaryView | null;
  /** Physical column the dictionary dialog should focus on open (F38). */
  dictionaryDialogColumn: number | null;
  // ----- row bookmarks, tags & notes (F40) -----------------------------------
  /** The ACTIVE document's annotations, resolved against the current view.
   * Drives the grid gutter indicators and the annotations panel; refreshed on
   * load, structural edits and every annotation mutation. Null until loaded. */
  annotationsView: AnnotationsView | null;
  /** Whether the annotations side panel is open (F40). */
  annotationsPanelOpen: boolean;
  /** Note-editor target: a row note (`columnId` null) or a cell note (F40). */
  annotationNoteTarget: {
    displayRow: number;
    columnId: string | null;
    label: string;
    initialText: string;
  } | null;
  /** Tag-picker target: the display rows to apply tags to (F40). */
  annotationTagTarget: { displayRows: number[] } | null;
  /** The tag whose "copy to column" dialog is open, or null (F40). */
  tagToColumnTag: string | null;
  /** Running derived-document job (F20–F23), if any. */
  derive: DeriveState | null;
  /** Error from the last derive job, for the dialog that started it. */
  deriveError: string | null;
  /** JSON / JSON Lines import flow (F33); non-null while its dialog is open. */
  jsonImport: JsonImportState | null;
  /** Running sampling/partitioning job (F48), if any. */
  sample: SampleState | null;
  /** Error from the last sampling job, for the dialog that started it. */
  sampleError: string | null;
  /** Which tab the sampling dialog opens on (set by the command, F48). */
  samplingInitialMode: "sampling" | "partitioning";
  /** Running (or just finished) batch-recipe job (F25), if any. */
  batch: BatchState | null;
  /** PII scan state (F28). */
  pii: PiiState;
  /** Per-followed-document tail state (F19). */
  followState: Record<
    number,
    { baselineRows: number; newRows: number; paused: boolean; alert: FollowAlertKind | null }
  >;
  /** Start following a file into a new read-only tab. */
  startFollowFile: (path: string) => Promise<void>;
  toggleFollowPause: (docId: number) => Promise<void>;
  stopFollowing: (docId: number) => Promise<void>;
  /** Route a follow-update event (rows appended by the watcher). */
  handleFollowUpdate: (update: FollowUpdate) => void;
  handleFollowAlert: (alert: FollowAlert) => void;

  /** Recoverable sessions found at startup (F16). */
  recoverySessions: RecoverableSession[];
  setRecoverySessions: (sessions: RecoverableSession[]) => void;
  /** Add a freshly recovered document as a tab and focus it (F16). */
  adoptRecoveredDoc: (meta: DocumentMeta) => void;
  /** Persist the recovery-journaling opt-in (F16). */
  setRecoveryEnabled: (enabled: boolean) => Promise<void>;
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
  setColumnWidthsBulk: (widths: Record<number, number>) => void;
  resetColumnWidths: () => void;
  setScrollPosition: (row: number, column: number) => void;
  loadSummaries: () => void;
  /** Invalidate the grid's row cache (e.g. after an out-of-grid cell save). */
  invalidateGrid: () => void;

  // named views & column layout (F12)
  /** Hide/unhide one physical column (at least one column stays visible). */
  setColumnHidden: (physicalCol: number, hidden: boolean) => void;
  unhideAllColumns: () => void;
  /** Pin/unpin one physical column (pinned columns display first, frozen). */
  pinColumn: (physicalCol: number, pin: boolean) => void;
  /** Move a DISPLAY column to a new display position (from the grid drag). */
  reorderColumns: (fromDisplay: number, toDisplay: number) => void;
  setWrapText: (wrap: boolean) => void;
  /** Apply (or clear, with empty keys) the non-destructive view sort. */
  applyViewSort: (keys: SortKey[]) => Promise<void>;
  /** Ask the grid to auto-fit the given physical columns (or all visible). */
  requestAutoFit: (cols: number[] | "all") => void;
  clearAutoFitRequest: () => void;
  /** Apply a named view to the active document (never dirties it). */
  applyNamedView: (view: NamedView) => Promise<void>;
  /** Snapshot the current state as a new named view and persist it. */
  saveCurrentViewAs: (name: string) => Promise<void>;
  /** Overwrite an existing view with the current state. */
  replaceNamedView: (viewId: string) => Promise<void>;
  renameNamedView: (viewId: string, name: string) => Promise<void>;
  duplicateNamedView: (viewId: string) => Promise<void>;
  deleteNamedView: (viewId: string) => Promise<void>;
  /** Reset filter, view sort, layout, widths and wrap — never touches data. */
  resetView: () => Promise<void>;
  dismissViewWarning: () => void;
  /** The saved views (and owning profile) matching the active document. */
  viewsForActive: () => { profile: FileProfile | null; views: NamedView[] };
  /**
   * The current selection rectangle in PHYSICAL columns, or null when the
   * display selection maps to more than one physical run (reordered columns)
   * — range-shaped operations (range export) are unavailable then.
   */
  selectionPhysicalRect: () => CellRect | null;
  /** One DISPLAY column translated to its physical column. */
  displayColToPhysical: (col: number) => number;
  /**
   * The selection rectangle's columns as PHYSICAL indices in DISPLAY order
   * (what the user sees left-to-right), or null without a rectangle.
   */
  selectionRectPhysicalCols: () => number[] | null;

  // documents
  openDialog: () => Promise<void>;
  /** File picker filtered to JSON / JSON Lines, routed through the open flow. */
  openJsonDialog: () => Promise<void>;
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

  // sampling & partitioning (F48)
  /** Track a started sample job so completion opens the new documents. */
  trackSample: (jobId: number, docIds: number[], destination: SampleState["destination"]) => void;
  cancelSample: () => Promise<void>;
  clearSampleError: () => void;
  /** Open the sampling dialog on the sampling or partitioning tab (F48). */
  openSamplingDialog: (mode: "sampling" | "partitioning") => void;

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

  // explicit schemas & typed columns (F31)
  /** Open the schema editor, optionally focused on one physical column. */
  openSchemaDialog: (col?: number) => void;
  /** Fetch the active document's schema into the store (badges, formatting). */
  loadSchema: () => Promise<void>;
  /** Assign or replace one column's schema (never dirties the document). */
  setColumnSchema: (schema: ColumnSchema) => Promise<boolean>;
  /** Drop one column's schema entry (back to implicit text). */
  removeColumnSchema: (columnId: string) => Promise<void>;
  /** Infer a schema from the data (cancellable job) and apply every entry. */
  inferAndApplySchema: () => Promise<boolean>;
  /** Import a versioned schema JSON file (REPLACES the schema); prompts. */
  importSchemaFromFile: () => Promise<string | null>;
  /** Export the schema to a versioned JSON file; prompts for a path. */
  exportSchemaToFile: () => Promise<void>;
  /** Scan a column's invalid values as a cancellable job; null on cancel/error. */
  runSchemaInvalidSamples: (
    columnId: string,
    maxSamples: number,
  ) => Promise<InvalidSampleReport | null>;
  /** Compute a conversion preview as a cancellable job; null on cancel/error. */
  runSchemaConvertPreview: (columnId: string, maxSamples: number) => Promise<ConvertPreview | null>;
  /** Cancel the in-flight schema scan (infer / invalid / preview), if any. */
  cancelSchemaScan: () => Promise<void>;
  /** Apply a previewed canonical conversion as ONE undo step (job). Guarded
   * against both the preview's data revision and its schema revision. */
  applyColumnConversion: (
    columnId: string,
    expectedRevision: number,
    expectedSchemaRevision: number,
  ) => Promise<boolean>;
  cancelColumnConversion: () => Promise<void>;

  // data dictionary (F38)
  /** Open the dictionary editor, optionally focused on one physical column. */
  openDictionaryDialog: (col?: number) => void;
  /** Fetch the active document's dictionary into the store (editor + tooltips). */
  loadDictionary: () => Promise<void>;
  /** Insert or replace one column's documentation (metadata: never dirties). */
  setDictionaryField: (field: DictionaryField) => Promise<boolean>;
  /** Drop one column's documentation entry (clear a column, or an orphan). */
  removeDictionaryField: (columnId: string) => Promise<boolean>;
  /** Discard every orphaned entry (documentation whose column is gone). */
  discardDictionaryOrphans: () => Promise<boolean>;
  /** Export the dictionary as JSON / Markdown / CSV; prompts for a path. */
  exportDictionaryToFile: (format: DictionaryFormat) => Promise<void>;
  /** Prompt for a CEESVEE dictionary JSON file to import; null if cancelled. */
  pickDictionaryImportFile: () => Promise<string | null>;
  /** Plan an import merge for a chosen file; null on error. Read-only. */
  previewDictionaryImport: (path: string, matchBy: MergeMatchBy) => Promise<MergePlan | null>;
  /**
   * Apply an import merge under an explicit resolution; null on error.
   * `expectedDictionaryRevision` MUST be the revision the plan was computed
   * against (`MergePlan.dictionaryRevision`), NOT the store's current view — the
   * backend rejects the apply if the dictionary moved since the plan was taken.
   */
  applyDictionaryImport: (
    path: string,
    matchBy: MergeMatchBy,
    resolution: MergeResolution,
    expectedDictionaryRevision: number,
  ) => Promise<DictionaryImportOutcome | null>;

  // row bookmarks, tags & notes (F40)
  /** Fetch the active document's annotations into the store (grid + panel).
   * Reads the doc_id-keyed registry, re-resolving against the current view. */
  loadAnnotations: () => Promise<void>;
  /** Load the active document's annotations from its `.ceesvee-notes.json`
   * sidecar, replacing any current store. Called once when a file opens. */
  hydrateAnnotationsFromSidecar: (docId: number, sourcePath: string | null) => Promise<void>;
  /** Open / close the annotations side panel (mutually exclusive with the
   * other right-rail panels). */
  setAnnotationsPanelOpen: (open: boolean) => void;
  /** Apply a star/flag/tag patch to a set of DISPLAY rows (threads the
   * annotations revision across the batch). Returns false on error. */
  applyRowMarks: (displayRows: number[], patch: RowMarkPatch) => Promise<boolean>;
  /** Set (or clear, with `text = null`) the ROW note on a display row. */
  setRowNote: (displayRow: number, text: string | null) => Promise<boolean>;
  /** Set (or clear, with `text = null`) a CELL note on a display row + column. */
  setCellNote: (displayRow: number, columnId: string, text: string | null) => Promise<boolean>;
  /** Define or update a tag in the per-document namespace. */
  defineAnnotationTag: (tag: TagDef) => Promise<boolean>;
  /** Remove a tag from the namespace and from every row that carries it. */
  removeAnnotationTag: (name: string) => Promise<boolean>;
  /** Delete one whole annotation entry by its stable handle. */
  removeAnnotation: (handle: number) => Promise<boolean>;
  /** Discard every orphaned annotation (no matching row in the document). */
  discardAnnotationOrphans: () => Promise<boolean>;
  /** Set (or clear, with `null`) the default author label for new notes. */
  setAnnotationAuthor: (author: string | null) => Promise<boolean>;
  /** Set (or clear, with `null`) the key columns anchoring new annotations. */
  setAnnotationKeySpec: (keySpec: KeySpec | null) => Promise<boolean>;
  /** Filter the grid to the rows matching an annotation-state predicate. */
  applyAnnotationFilter: (predicate: AnnotationPredicate) => Promise<void>;
  /** Preview copying a tag into a column (read-only); null on error. */
  previewTagToColumn: (tag: string) => Promise<TagToColumnPreview | null>;
  /** Copy a tag into a real column as one undoable op; false on error. */
  applyTagToColumn: (tag: string, target: TagToColumnTarget) => Promise<boolean>;
  /** Export the annotations as JSON / CSV; prompts for a path. */
  exportAnnotationsToFile: (format: AnnotationExportFormat) => Promise<void>;
  /** Note-editor dialog: open on a row note, a cell note, or close. The
   * `initialText` prefills the editor with any existing note. */
  openRowNoteEditor: (displayRow: number, label: string, initialText?: string) => void;
  openCellNoteEditor: (
    displayRow: number,
    columnId: string,
    label: string,
    initialText?: string,
  ) => void;
  closeNoteEditor: () => void;
  /** Tag-picker dialog: open for a set of display rows, or close. */
  openTagPicker: (displayRows: number[]) => void;
  closeTagPicker: () => void;
  /** Tag-to-column dialog: open for a tag, or close. */
  openTagToColumn: (tag: string) => void;
  closeTagToColumn: () => void;

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

  // project workspaces (F37)
  /** The open project's header (name/path/version), or null. */
  project: ProjectMeta | null;
  /** UI snapshot captured at the last save/open, for dirty derivation. */
  projectBaseline: ProjectSnapshot | null;
  /** The open-dialog preview awaiting per-source resolutions, if any. */
  projectOpen: ProjectOpenPreview | null;
  /** Per-source choices in the open dialog, by source id. */
  projectOpenChoices: Record<string, SourceChoice>;
  /** Sources whose saved views were gated on the last open (warn, never break). */
  projectWarnings: GatingWarning[];
  /** Whether the unsaved-project close confirmation is showing. */
  projectClosePromptOpen: boolean;
  /** Whether the open project has drifted from its last-saved snapshot. */
  isProjectDirty: () => boolean;
  /** Create a new project (optionally from a template file), replacing any open one. */
  projectNew: (templatePath?: string) => Promise<void>;
  /** Pick a template file, then create a new project from it. */
  projectNewFromTemplate: () => Promise<void>;
  /** Pick a `.ceesveeproj` and load its per-source open preview into the dialog. */
  projectPickAndOpen: () => Promise<void>;
  /** Set one source's choice in the open dialog. */
  setProjectChoice: (sourceId: string, choice: SourceChoice) => void;
  /** Open a file picker to relink one missing/changed source. */
  projectLocateSource: (sourceId: string) => Promise<void>;
  /** Cancel the whole open (leaves any current project untouched). */
  cancelProjectOpen: () => void;
  /** Apply the open with the current per-source choices. */
  applyProjectOpen: () => Promise<void>;
  /** Open every present source and leave missing/moved ones out. */
  projectOpenAvailableOnly: () => Promise<void>;
  /** Capture live sections and save the project (prompting for a path if new). */
  projectSave: (saveAs?: boolean) => Promise<boolean>;
  /** Export the open project as a reusable template (config only, no sources). */
  projectSaveTemplate: () => Promise<void>;
  /** Close the project, guarding unsaved changes. */
  requestCloseProject: () => void;
  /** Close the project immediately (documents stay open). */
  closeProjectNow: () => Promise<void>;
  /** Dismiss the project-warnings banner. */
  dismissProjectWarnings: () => void;

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

  // JSON / JSON Lines interoperability (F33)
  /** Open the JSON import dialog for a file and run the first preview scan. */
  openJsonImport: (path: string) => Promise<void>;
  /** (Re)run the preview scan under the given options (supersedes any in-flight). */
  runJsonScan: (options: JsonImportOptions) => Promise<void>;
  /** Cancel the running preview scan, if any. */
  cancelJsonScan: () => Promise<void>;
  /** Import the file into a NEW document (reuses the shared derive slot). */
  applyJsonImport: (options: JsonImportOptions) => Promise<void>;
  /** Close the JSON import dialog, cancelling any in-flight scan. */
  dismissJsonImport: () => void;
  /** Prompt for a path and export the active document as JSON (F33). */
  exportJson: (options: JsonExportOptions, scope: ExportScope) => Promise<void>;

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

  // True only while a project open is applying its plan (F40). During that
  // window the per-source sidecar must never be read — the project's own
  // `annotations` section is authoritative — but `get().project` is not set
  // until the plan finishes, so this flag bridges the gap for the fire-and-
  // forget sidecar hydration that `openPath` kicks off mid-plan.
  let openingProject = false;

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
    columnLayout: null,
    wrapText: false,
    activeViewId: null,
    viewSortKeys: [],
    viewWarning: null,
    record: initialRecordUi(),
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
        columnLayout: s.columnLayout,
        wrapText: s.wrapText,
        activeViewId: s.activeViewId,
        viewSortKeys: s.viewSortKeys,
        viewWarning: s.viewWarning,
        record: s.record,
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
      columnLayout: next.columnLayout,
      wrapText: next.wrapText,
      activeViewId: next.activeViewId,
      viewSortKeys: next.viewSortKeys,
      viewWarning: next.viewWarning,
      record: next.record,
      autoFitRequest: null,
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
      // F31: the schema is per-document; the Grid reloads it for the new tab.
      schemaInfo: null,
      schemaConvert: null,
      schemaScan: null,
      schemaDialogColumn: null,
      // F38: the dictionary is per-document; the Grid reloads it for the tab.
      dictionaryView: null,
      dictionaryDialogColumn: null,
      // F40: annotations are per-document (the Grid reloads them for the tab);
      // any open annotation editor targets the outgoing document, so drop it.
      annotationsView: null,
      annotationNoteTarget: null,
      annotationTagTarget: null,
      tagToColumnTag: null,
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

  // ----- named views (F12) -------------------------------------------------

  /**
   * Map a DISPLAY-space selection rectangle onto PHYSICAL column runs. The
   * identity layout returns the rect unchanged; a hidden/reordered layout may
   * split it into several rectangles (columns adjacent on screen need not be
   * adjacent in the file).
   */
  const selectionPhysicalRects = (rect: CellRect): CellRect[] => {
    const meta = activeMeta();
    const layout = get().columnLayout;
    if (!meta || layoutIsTrivial(layout)) return [rect];
    const proj = projectColumns(meta.columnIds, layout);
    const phys = proj.physical.slice(rect.x, rect.x + rect.width).sort((a, b) => a - b);
    return contiguousRuns(phys).map((run) => ({
      x: run.start,
      y: rect.y,
      width: run.len,
      height: rect.height,
    }));
  };

  /**
   * Selection scope for find/replace, translated to physical columns. The
   * backend takes ONE rectangle; a display selection that maps to several
   * physical runs (reordered columns) is reported as blocked rather than
   * silently searching the wrong columns.
   */
  const findSelectionScope = (
    inSelection: boolean,
    rect: CellRect | null,
  ): { rect?: CellRect; blocked: boolean } => {
    if (!inSelection || !rect) return { blocked: false };
    const rects = selectionPhysicalRects(rect);
    if (rects.length !== 1) return { blocked: true };
    return { rect: rects[0], blocked: false };
  };

  const FIND_SELECTION_BLOCKED =
    "Find in selection isn't available while columns are reordered around the selection — search the whole file, or reset the column order.";

  /** The profile owning the active doc's views (first path match), if any. */
  const viewProfileFor = (meta: DocumentMeta): FileProfile | null => {
    if (!meta.path) return null;
    const profiles = get().settings?.profiles ?? [];
    const matches = matchingProfiles(profiles, meta.path);
    // Prefer a matching profile that already stores views.
    return matches.find((p) => (p.namedViews?.length ?? 0) > 0) ?? matches[0] ?? null;
  };

  /** Persist a profile change (upserting the profile when it is new). */
  const persistProfile = async (profile: FileProfile) => {
    const current = get().settings ?? { version: 1, profiles: [] };
    const exists = current.profiles.some((p) => p.id === profile.id);
    const next = {
      ...current,
      profiles: exists
        ? current.profiles.map((p) => (p.id === profile.id ? profile : p))
        : [...current.profiles, profile],
    };
    await api.setSettings(next);
    set({ settings: next });
  };

  /** Update the owning profile's views (creating a profile if needed). */
  const persistViews = async (
    meta: DocumentMeta,
    update: (views: NamedView[]) => NamedView[],
    lastViewId?: string | null,
  ) => {
    const owner = viewProfileFor(meta) ?? profileFromDocument(meta.fileName || "Views", meta);
    const namedViews = update(owner.namedViews ?? []);
    const next: FileProfile = {
      ...owner,
      namedViews,
      lastViewId: lastViewId === undefined ? (owner.lastViewId ?? null) : lastViewId,
    };
    await persistProfile(next);
  };

  /**
   * Apply a named view to the ACTIVE document. Row parts (filter + view
   * sort) go through the backend — never entering the undo stack or marking
   * the document dirty — and column parts resolve by stable ID. Columns that
   * no longer exist produce a recoverable warning; the view itself is never
   * modified.
   */
  const applyNamedViewInner = async (view: NamedView, persistLast: boolean) => {
    const meta = activeMeta();
    if (!meta) return;
    const ids = meta.columnIds;
    const missing: string[] = [];
    const note = (more: string[]) => {
      for (const id of more) if (!missing.includes(id)) missing.push(id);
    };

    const layout = layoutOfView(view);
    note(projectColumns(ids, layout).missing);

    let appliedSort: SortKey[] = [];
    try {
      // Row filter (all-or-nothing per filter; skipping a single condition
      // could silently widen an AND group).
      if (view.filter) {
        const remapped = remapFilterColumns(view.filter, view.filterColumnIds, ids);
        note(remapped.missing);
        if (remapped.filter) {
          const hydrated = hydrateFilter(remapped.filter);
          set((s) => ({ filter: { ...s.filter, spec: hydrated } }));
          reloadDoc(await api.setFilter(meta.id, hydrated));
        } else if (meta.filtered) {
          // The view's filter could not be applied (missing columns) — a
          // PREVIOUS filter must not survive as if it were this view's.
          reloadDoc(await api.clearFilter(meta.id));
        }
      } else if (meta.filtered) {
        reloadDoc(await api.clearFilter(meta.id));
      }

      // Non-destructive view sort (missing keys are skipped — benign).
      const resolved = resolveSortKeys(view.sortKeys, ids);
      note(resolved.missing);
      if (resolved.keys.length > 0 || meta.viewSorted) {
        reloadDoc(await api.setViewSort(meta.id, resolved.keys));
      }
      appliedSort = resolved.keys;
    } catch (e) {
      set({ error: String(e) });
      return;
    }

    set(() => ({
      columnLayout: layoutIsTrivial(layout) ? null : layout,
      wrapText: view.wrapText,
      // REPLACE the width map: widths the view does not specify return to
      // defaults — merging would leave the previous layout's widths behind
      // and the named view would not actually be restored.
      columnWidths: widthsFromIds(view.columnWidths, ids),
      activeViewId: view.id,
      viewSortKeys: appliedSort,
      viewWarning:
        missing.length > 0
          ? `This view references ${missing.length} column${missing.length === 1 ? "" : "s"} that no longer exist (deleted or from an older file layout). The rest of the view was applied; the view itself is unchanged.`
          : null,
    }));

    if (persistLast && meta.path) {
      await persistViews(meta, (views) => views, view.id).catch(() => undefined);
    }
  };

  /** Restore a matching profile's last-selected view after a file opens. */
  const restoreLastViewFor = async (meta: DocumentMeta) => {
    if (!meta.path) return;
    const owner = viewProfileFor(meta);
    const view = owner?.lastViewId
      ? (owner.namedViews ?? []).find((v) => v.id === owner.lastViewId)
      : undefined;
    if (!view) return;
    // Only restore into the still-active document: layout state is applied
    // to the active slots, and the user may have switched tabs meanwhile.
    if (get().activeId !== meta.id) return;
    await applyNamedViewInner(view, false).catch(() => undefined);
  };

  /** Replace a tab's metadata (dirty/undo flags) without reloading the grid. */
  const refreshMeta = (meta: DocumentMeta) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === meta.id ? meta : t)) }));

  /** Replace metadata AND invalidate the grid cache (structural change). */
  const reloadDoc = (meta: DocumentMeta) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === meta.id ? meta : t)),
      dataVersion: s.dataVersion + 1,
      // Structural mutations drop the backend row view (filter + view sort);
      // keep the front end's applied-sort mirror in sync with the truth.
      viewSortKeys:
        meta.id === s.activeId && !meta.viewSorted && s.viewSortKeys.length > 0
          ? []
          : s.viewSortKeys,
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

  /**
   * Persist a document's annotations after an edit (F40), fire-and-forget.
   *
   * The two backing stores are mutually exclusive per the spec's migration
   * rule: when a PROJECT is open the annotations live in the project's
   * `annotations` section (captured into the ProjectStore on the next project
   * save, exactly like tabs/layout/views — never in the sidecar), so this is a
   * no-op. With no project open the durable store is the source's
   * `.ceesvee-notes.json` sidecar; an empty store deletes it (the backend
   * handles that). A path-less document (new/derived) has nowhere to write, so
   * this is a no-op there too. Write failures are swallowed — they surface on
   * the next explicit save/export instead of interrupting an annotation edit.
   */
  const persistAnnotationSidecar = (docId: number, path: string | null) => {
    if (get().project) return;
    if (!path) return;
    void api.annotationsSaveSidecar(docId, path).catch(() => undefined);
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

  // ----- project workspaces (F37) ------------------------------------------

  /** Current panel-open flags, as the project's layout section tracks them. */
  const currentPanels = (s: Store): PanelLayout => ({
    diagnostics: s.diagnosticsOpen,
    explorer: s.explorer.open,
    changes: s.changesOpen,
  });

  /** Each open tab's active named-view id (live active tab + saved per-tab). */
  const activeViewByTab = (s: Store): Record<number, string | null> => {
    const out: Record<number, string | null> = {};
    for (const t of s.tabs) {
      out[t.id] = t.id === s.activeId ? s.activeViewId : (s.uiStates[t.id]?.activeViewId ?? null);
    }
    return out;
  };

  /** The live project-relevant UI snapshot (open docs, active, panels, views). */
  const currentProjectSnapshot = (): ProjectSnapshot => {
    const s = get();
    return projectSnapshot(s.tabs, s.activeId, currentPanels(s), activeViewByTab(s));
  };

  /**
   * Confirm discarding the current project's unsaved changes before an action
   * that would replace it (new / open). Returns whether to proceed.
   */
  const guardDiscardProject = async (): Promise<boolean> => {
    const s = get();
    if (!s.project || !deriveProjectDirty(s.projectBaseline, currentProjectSnapshot())) return true;
    return ask("Discard unsaved changes to the current project?", {
      title: "Discard project changes",
      kind: "warning",
    });
  };

  /**
   * Push the live sources/tabs/layout/views/annotations into the backend
   * ProjectStore (THE persistence boundary). Reuses existing source ids by path
   * so ids stay stable across saves. Captures configuration only — never cell
   * data (the annotations envelope references rows by identity + content hash).
   * Schemas, recipes, joins, comparisons and row keys are NOT captured from the
   * live session this cycle; they round-trip when a template or existing
   * project file carries them (see CHANGELOG for the deferred scope).
   */
  const captureProjectSections = async () => {
    const s = get();
    const docs = s.tabs.filter((t) => t.path);
    const fingerprints: Record<number, FileFingerprint | null> = {};
    const exports: Record<number, AnnotationsExport | null> = {};
    await Promise.all(
      docs.map(async (t) => {
        fingerprints[t.id] = await api.getFileFingerprint(t.id).catch(() => null);
        // F40: the backend registry is the live source of truth; pull each open
        // document's export envelope so the project absorbs its annotations.
        exports[t.id] = await api.annotationsGetExport(t.id).catch(() => null);
      }),
    );
    const state = await api.projectGet().catch(() => null);
    const existingSources = state?.sections.sources ?? [];
    const existingViews = (state?.sections.views as SourceViewsSection[] | undefined) ?? [];
    const existingAnnotations =
      (state?.sections.annotations as SourceAnnotationsSection[] | undefined) ?? [];
    const sources = buildSources(s.tabs, existingSources, fingerprints);
    const views = buildViewsSection(
      sources,
      s.tabs,
      existingViews,
      (tab) => viewProfileFor(tab)?.namedViews ?? [],
      (tab) =>
        tab.id === s.activeId ? s.activeViewId : (s.uiStates[tab.id]?.activeViewId ?? null),
    );
    const annotations = buildAnnotationsSection(
      sources,
      s.tabs,
      existingAnnotations,
      (tab) => exports[tab.id] ?? null,
    );
    await api.projectSetSection("sources", sources);
    await api.projectSetSection("tabs", buildTabsSection(sources, s.tabs, s.activeId));
    await api.projectSetSection("layout", buildLayoutSection(currentPanels(s)));
    await api.projectSetSection("views", views);
    await api.projectSetSection("annotations", annotations);
  };

  /**
   * Drive an open plan: open each referenced document through the ordinary
   * open pipeline, restore tab order/panel layout, reapply EVERY opened
   * source's saved view (not just the active tab's) when the gating allows,
   * then end focused on the saved active tab. NEVER runs recipes, queries,
   * joins, comparisons or exports.
   */
  const applyProjectPlan = async (plan: ProjectOpenPlan) => {
    // Suppress the per-source sidecar hydration `openPath` fires while the plan
    // is applying — the project's `annotations` section (loaded below) wins.
    openingProject = true;
    try {
      for (const entry of plan.entries) {
        await get().openPath(entry.path);
      }
    } finally {
      openingProject = false;
    }
    // Restore saved tab order (project docs first, extras appended).
    set((s) => {
      const order = orderTabsForPlan(s.tabs, plan.entries);
      const byId = new Map(s.tabs.map((t) => [t.id, t]));
      const tabs = order.map((id) => byId.get(id)).filter((t): t is DocumentMeta => !!t);
      return { tabs };
    });
    const state = await api.projectGet().catch(() => null);
    // Restore the saved panel layout (front-end config only; runs nothing).
    const panels = panelsFromLayout(state?.sections.layout ?? null);
    set((s) => ({
      diagnosticsOpen: panels.diagnostics,
      changesOpen: panels.changes,
      explorer: { ...s.explorer, open: panels.explorer },
    }));

    const findTab = (path: string) =>
      get().tabs.find((t) => t.path && pathKey(t.path) === pathKey(path));

    // F40: hydrate each opened source's annotations FROM THE PROJECT SECTION
    // (authoritative over any sidecar), by matching the section's stable source
    // id to the plan entry that opened the tab. A source still pending a
    // large-file decision or archive extraction has no tab yet — it is skipped,
    // mirroring the view-reapply gate below.
    const savedAnnotations =
      (state?.sections.annotations as SourceAnnotationsSection[] | undefined) ?? [];
    const bySourceId = new Map(savedAnnotations.map((a) => [a.sourceId, a]));
    for (const entry of plan.entries) {
      const saved = bySourceId.get(entry.sourceId);
      const tab = saved ? findTab(entry.path) : undefined;
      if (!saved || !tab) continue;
      await api.annotationsLoadExport(tab.id, saved.annotations).catch(() => undefined);
    }

    // Reapply each opened source's saved view against ITS OWN document. A
    // source still pending a large-file decision or archive extraction has no
    // tab yet — skip it rather than applying its view to whatever document is
    // active. applyNamedView targets the active document, so switch to each
    // source's tab first; the saved active tab is done LAST so the session ends
    // focused on it.
    const reapplyOrder = [
      ...plan.entries.filter((e) => e.sourceId !== plan.activeTab),
      ...plan.entries.filter((e) => e.sourceId === plan.activeTab),
    ];
    for (const entry of reapplyOrder) {
      if (!entry.reapplyViews || !entry.activeViewId) continue;
      const tab = findTab(entry.path);
      if (!tab) continue;
      const view = entry.views.find((v) => v.id === entry.activeViewId);
      if (!view) continue;
      set((s) => switchPatch(s, tab.id));
      await get()
        .applyNamedView(view)
        .catch(() => undefined);
    }
    // End focused on the saved active tab when it actually opened.
    const activeEntry = plan.entries.find((e) => e.sourceId === plan.activeTab);
    const activeTab = activeEntry ? findTab(activeEntry.path) : undefined;
    if (activeTab) set((s) => switchPatch(s, activeTab.id));

    set({
      project: plan.meta,
      projectBaseline: currentProjectSnapshot(),
      projectWarnings: gatingWarnings(plan.entries),
      projectOpen: null,
      projectOpenChoices: {},
    });
    // Refresh the annotations surface for the now-active document from the
    // registry the project section just populated.
    void get().loadAnnotations();
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
    columnLayout: null,
    wrapText: false,
    activeViewId: null,
    viewSortKeys: [],
    viewWarning: null,
    record: initialRecordUi(),
    recordFormOpen: false,
    autoFitRequest: null,
    uiStates: {},
    summaries: null,
    summariesDocId: null,
    diagnosticsOpen: false,
    changesOpen: false,
    diagnostics: {},
    jumpTarget: null,
    reopen: initialReopen,
    openDecision: null,
    indexing: null,
    cluster: initialCluster,
    semantic: initialSemantic,
    crossval: initialCrossVal,
    outlier: initialOutlier,
    schemaInfo: null,
    schemaConvert: null,
    schemaScan: null,
    schemaDialogColumn: null,
    dictionaryView: null,
    dictionaryDialogColumn: null,
    annotationsView: null,
    annotationsPanelOpen: false,
    annotationNoteTarget: null,
    annotationTagTarget: null,
    tagToColumnTag: null,
    derive: null,
    deriveError: null,
    jsonImport: null,
    sample: null,
    sampleError: null,
    samplingInitialMode: "sampling",
    batch: null,
    pii: initialPii,
    followState: {},
    recoverySessions: [],
    archivePick: null,
    archiveLargeConfirm: null,
    externalPrompt: null,
    ignoredFingerprints: {},
    quitPromptOpen: false,
    project: null,
    projectBaseline: null,
    projectOpen: null,
    projectOpenChoices: {},
    projectWarnings: [],
    projectClosePromptOpen: false,
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
      // F16: offer crash recovery when journals survived a previous session.
      void api
        .listRecoverySessions()
        .then((sessions) => {
          if (sessions.length > 0) {
            set({ recoverySessions: sessions, activeModal: "recovery" });
          }
        })
        .catch(() => undefined);
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
      // selections). Debounced and guarded against stale results. Under a
      // non-trivial column layout (F12) the display rectangle maps to one or
      // more PHYSICAL column runs; stats are computed per run and merged, so
      // they stay correct with hidden or reordered columns.
      statsTimer = setTimeout(() => {
        const rects = selectionPhysicalRects(rect);
        void Promise.all(rects.map((r) => api.selectionStats(id, r)))
          .then((parts) => {
            if (get().selectionRect === rect) set({ selection: mergeStats(parts) });
          })
          .catch(() => undefined);
      }, 120);
    },

    setFrozenCols: (count) => set({ frozenColumnCount: Math.max(0, count) }),

    setColumnWidth: (col, width) =>
      set((s) => ({ columnWidths: { ...s.columnWidths, [col]: width } })),

    setColumnWidthsBulk: (widths) =>
      set((s) => ({ columnWidths: { ...s.columnWidths, ...widths } })),

    resetColumnWidths: () => set({ columnWidths: {} }),

    // ----- named views & column layout (F12) --------------------------------

    setColumnHidden: (physicalCol, hidden) => {
      const meta = activeMeta();
      if (!meta) return;
      const id = meta.columnIds[physicalCol];
      if (id === undefined) return;
      const layout = get().columnLayout ?? emptyLayout();
      const hiddenIds = hidden
        ? layout.hiddenColumnIds.includes(id)
          ? layout.hiddenColumnIds
          : [...layout.hiddenColumnIds, id]
        : layout.hiddenColumnIds.filter((h) => h !== id);
      const next = { ...layout, hiddenColumnIds: hiddenIds };
      if (hidden && projectColumns(meta.columnIds, next).physical.length === 0) {
        set({ error: "At least one column must stay visible" });
        return;
      }
      set({ columnLayout: layoutIsTrivial(next) ? null : next });
    },

    unhideAllColumns: () =>
      set((s) => {
        if (!s.columnLayout) return {};
        const next = { ...s.columnLayout, hiddenColumnIds: [] };
        return { columnLayout: layoutIsTrivial(next) ? null : next };
      }),

    pinColumn: (physicalCol, pin) => {
      const meta = activeMeta();
      if (!meta) return;
      const id = meta.columnIds[physicalCol];
      if (id === undefined) return;
      const layout = get().columnLayout ?? emptyLayout();
      const pinnedIds = pin
        ? layout.pinnedColumnIds.includes(id)
          ? layout.pinnedColumnIds
          : [...layout.pinnedColumnIds, id]
        : layout.pinnedColumnIds.filter((p) => p !== id);
      const next = { ...layout, pinnedColumnIds: pinnedIds };
      set({ columnLayout: layoutIsTrivial(next) ? null : next });
    },

    reorderColumns: (fromDisplay, toDisplay) => {
      const meta = activeMeta();
      if (!meta) return;
      const s = get();
      const proj = projectColumns(meta.columnIds, s.columnLayout);
      const count = proj.physical.length;
      if (fromDisplay < 0 || fromDisplay >= count || fromDisplay === toDisplay) return;
      // Drags stay within their region: a pinned column reorders among the
      // pins, an unpinned one among the rest (pin/unpin is its own action).
      const inPins = fromDisplay < proj.frozen;
      const lo = inPins ? 0 : proj.frozen;
      const hi = inPins ? proj.frozen - 1 : count - 1;
      const target = Math.min(Math.max(toDisplay, lo), hi);
      if (target === fromDisplay) return;

      const displayIds = proj.physical.map((p) => meta.columnIds[p]);
      const [moved] = displayIds.splice(fromDisplay, 1);
      displayIds.splice(target, 0, moved);

      const layout = s.columnLayout ?? emptyLayout();
      const next: ColumnLayout = {
        ...layout,
        pinnedColumnIds: displayIds.slice(0, proj.frozen),
        columnOrder: displayIds.slice(proj.frozen),
      };
      set({ columnLayout: layoutIsTrivial(next) ? null : next });
    },

    setWrapText: (wrap) => set({ wrapText: wrap }),

    applyViewSort: async (keys) => {
      await mutate((id) => api.setViewSort(id, keys));
      const meta = activeMeta();
      set({ viewSortKeys: meta?.viewSorted ? keys : [] });
    },

    requestAutoFit: (cols) => {
      autoFitNonce += 1;
      set({ autoFitRequest: { cols, nonce: autoFitNonce } });
    },

    clearAutoFitRequest: () => set({ autoFitRequest: null }),

    applyNamedView: (view) => applyNamedViewInner(view, true),

    saveCurrentViewAs: async (name) => {
      const meta = activeMeta();
      if (!meta) return;
      if (!meta.path) {
        set({ error: "Save the file first — named views are stored per source file" });
        return;
      }
      const s = get();
      const existing = get().viewsForActive().views;
      const view = snapshotView({
        name: uniqueViewName(
          existing.map((v) => v.name),
          name,
        ),
        meta,
        filter: meta.filtered ? s.filter.spec : null,
        viewSortKeys: s.viewSortKeys,
        layout: s.columnLayout,
        columnWidths: s.columnWidths,
        wrapText: s.wrapText,
      });
      try {
        await persistViews(meta, (views) => upsertView(views, view), view.id);
        set({ activeViewId: view.id, viewWarning: null });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    replaceNamedView: async (viewId) => {
      const meta = activeMeta();
      if (!meta) return;
      const s = get();
      const existing = get()
        .viewsForActive()
        .views.find((v) => v.id === viewId);
      if (!existing) return;
      const view = snapshotView({
        id: viewId,
        name: existing.name,
        meta,
        filter: meta.filtered ? s.filter.spec : null,
        viewSortKeys: s.viewSortKeys,
        layout: s.columnLayout,
        columnWidths: s.columnWidths,
        wrapText: s.wrapText,
      });
      try {
        await persistViews(meta, (views) => upsertView(views, view), viewId);
        set({ activeViewId: viewId });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    renameNamedView: async (viewId, name) => {
      const meta = activeMeta();
      if (!meta) return;
      const views = get().viewsForActive().views;
      const target = views.find((v) => v.id === viewId);
      if (!target) return;
      const unique = uniqueViewName(
        views.filter((v) => v.id !== viewId).map((v) => v.name),
        name,
      );
      await persistViews(meta, (all) =>
        all.map((v) => (v.id === viewId ? { ...v, name: unique } : v)),
      ).catch((e) => set({ error: String(e) }));
    },

    duplicateNamedView: async (viewId) => {
      const meta = activeMeta();
      if (!meta) return;
      const views = get().viewsForActive().views;
      const target = views.find((v) => v.id === viewId);
      if (!target) return;
      const copy: NamedView = {
        ...target,
        id: `${viewId}-copy-${Date.now().toString(36)}`,
        name: uniqueViewName(
          views.map((v) => v.name),
          target.name,
        ),
      };
      await persistViews(meta, (all) => [...all, copy]).catch((e) => set({ error: String(e) }));
    },

    deleteNamedView: async (viewId) => {
      const meta = activeMeta();
      if (!meta) return;
      const owner = viewProfileFor(meta);
      if (!owner) return;
      const nextLast = owner.lastViewId === viewId ? null : undefined;
      try {
        await persistViews(meta, (all) => all.filter((v) => v.id !== viewId), nextLast);
        if (get().activeViewId === viewId) set({ activeViewId: null });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    resetView: async () => {
      const meta = activeMeta();
      if (!meta) return;
      try {
        if (meta.filtered || meta.viewSorted) {
          reloadDoc(await api.resetRowView(meta.id));
        }
      } catch (e) {
        set({ error: String(e) });
        return;
      }
      set((s) => ({
        columnLayout: null,
        wrapText: false,
        columnWidths: {},
        frozenColumnCount: 0,
        activeViewId: null,
        viewSortKeys: [],
        viewWarning: null,
        filter: { ...initialFilter, open: s.filter.open },
      }));
      if (meta.path) {
        await persistViews(meta, (views) => views, null).catch(() => undefined);
      }
    },

    dismissViewWarning: () => set({ viewWarning: null }),

    viewsForActive: () => {
      const meta = activeMeta();
      if (!meta) return { profile: null, views: [] };
      const profile = viewProfileFor(meta);
      return { profile, views: profile?.namedViews ?? [] };
    },

    selectionPhysicalRect: () => {
      const rect = get().selectionRect;
      if (!rect) return null;
      const rects = selectionPhysicalRects(rect);
      return rects.length === 1 ? rects[0] : null;
    },

    displayColToPhysical: (col) => {
      const meta = activeMeta();
      const layout = get().columnLayout;
      if (!meta || layoutIsTrivial(layout)) return col;
      return projectColumns(meta.columnIds, layout).physical[col] ?? col;
    },

    selectionRectPhysicalCols: () => {
      const rect = get().selectionRect;
      const meta = activeMeta();
      if (!rect || !meta) return null;
      const layout = get().columnLayout;
      const projection = layoutIsTrivial(layout) ? null : projectColumns(meta.columnIds, layout);
      const cols: number[] = [];
      for (let c = rect.x; c < rect.x + rect.width; c++) {
        cols.push(projection ? (projection.physical[c] ?? c) : c);
      }
      return cols;
    },

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

    openJsonDialog: async () => {
      const selected = await openFileDialog({ multiple: false, filters: JSON_FILE_FILTERS });
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
      // F33: structured JSON / JSON Lines routes through the import preview
      // dialog instead of the CSV open path.
      if (lower.endsWith(".json") || lower.endsWith(".jsonl") || lower.endsWith(".ndjson")) {
        await get().openJsonImport(path);
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
        // F40: hydrate annotations from the source's sidecar so bookmarks /
        // tags / notes persist across sessions when no project is open.
        void get().hydrateAnnotationsFromSidecar(meta.id, meta.path);
        void suggestProfileFor(meta)
          .then(() => restoreLastViewFor(meta))
          .catch(() => undefined);
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
          void get().hydrateAnnotationsFromSidecar(meta.id, meta.path);
          return;
        }
        const meta = await api.openFile(decision.path, { forceInMemory: true });
        set((s) => ({
          ...switchPatch(s, meta.id),
          tabs: [...s.tabs, meta],
          busy: false,
        }));
        pushRecent(decision.path);
        void get().hydrateAnnotationsFromSidecar(meta.id, meta.path);
        void suggestProfileFor(meta)
          .then(() => restoreLastViewFor(meta))
          .catch(() => undefined);
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

    // ----- sampling & partitioning (F48) -----------------------------------------

    trackSample: (jobId, docIds, destination) => {
      set({
        sample: { jobId, docIds, destination, processed: 0, total: null, message: null },
        sampleError: null,
      });
      consumeEarlyFinish(jobId);
    },

    cancelSample: async () => {
      const sample = get().sample;
      // A cancel removes any incomplete outputs (the backend cleans partial
      // exports and never registers derived docs until the whole job succeeds).
      if (sample) await api.cancelJob(sample.jobId).catch(() => undefined);
    },

    clearSampleError: () => set({ sampleError: null }),

    openSamplingDialog: (mode) => set({ samplingInitialMode: mode, activeModal: "sampling" }),

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

    // ----- follow mode (F19) -------------------------------------------------------

    startFollowFile: async (path) => {
      try {
        const meta = await api.startFollow(path);
        set((s) => ({
          ...switchPatch(s, meta.id),
          tabs: [...s.tabs, meta],
          followState: {
            ...s.followState,
            [meta.id]: {
              baselineRows: meta.totalRowCount,
              newRows: 0,
              paused: false,
              alert: null,
            },
          },
        }));
        pushRecent(path);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    toggleFollowPause: async (docId) => {
      const current = get().followState[docId];
      if (!current) return;
      const paused = !current.paused;
      try {
        await api.setFollowPaused(docId, paused);
        set((s) => ({
          followState: { ...s.followState, [docId]: { ...current, paused } },
        }));
      } catch (e) {
        set({ error: String(e) });
      }
    },

    stopFollowing: async (docId) => {
      try {
        await api.stopFollow(docId);
        set((s) => {
          const next = { ...s.followState };
          delete next[docId];
          return { followState: next };
        });
        await get().refreshActiveDoc();
      } catch (e) {
        set({ error: String(e) });
      }
    },

    handleFollowUpdate: (update) => {
      const current = get().followState[update.docId];
      set((s) => ({
        followState: {
          ...s.followState,
          [update.docId]: {
            baselineRows: current?.baselineRows ?? 0,
            newRows: (current?.newRows ?? 0) + update.newRows,
            paused: current?.paused ?? false,
            alert: current?.alert ?? null,
          },
        },
      }));
      if (get().activeId === update.docId) {
        void get().refreshActiveDoc();
      } else {
        // Background tab: patch its metadata from the event so switching
        // back shows the appended rows immediately (row counts + revision;
        // the grid refetches on activation via the doc change). While a
        // filter is active, the visible count is refreshed on activation.
        set((s) => ({
          tabs: s.tabs.map((t) =>
            t.id === update.docId
              ? {
                  ...t,
                  totalRowCount: update.totalRows,
                  rowCount: t.filtered ? t.rowCount : update.totalRows,
                  revision: update.revision,
                }
              : t,
          ),
        }));
      }
    },

    handleFollowAlert: (alert) => {
      const current = get().followState[alert.docId];
      if (!current) return;
      set((s) => ({
        followState: {
          ...s.followState,
          [alert.docId]: { ...current, alert: alert.kind },
        },
      }));
    },

    // ----- crash recovery (F16) --------------------------------------------------

    setRecoverySessions: (sessions) => set({ recoverySessions: sessions }),

    adoptRecoveredDoc: (meta) =>
      set((s) => ({ ...switchPatch(s, meta.id), tabs: [...s.tabs, meta] })),

    setRecoveryEnabled: async (enabled) => {
      const settings: AppSettings = {
        ...(get().settings ?? { version: 1, profiles: [] }),
        recoveryEnabled: enabled,
      };
      set({ settings });
      try {
        await api.setSettings(settings);
      } catch (e) {
        set({ error: String(e) });
      }
    },

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

    // ----- explicit schemas & typed columns (F31) --------------------------------

    openSchemaDialog: (col) => set({ schemaDialogColumn: col ?? null, activeModal: "schema" }),

    loadSchema: async () => {
      const meta = activeMeta();
      if (!meta) {
        set({ schemaInfo: null });
        return;
      }
      try {
        const info = await api.getSchema(meta.id);
        // A tab switch may have landed during the await; don't cross-install.
        if (get().activeId === meta.id) set({ schemaInfo: info });
      } catch {
        // A schema fetch failure is non-fatal: badges/formatting stay off.
      }
    },

    setColumnSchema: async (schema) => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        const info = await api.setColumnSchema(meta.id, schema);
        // Schema edits never dirty the document (metadata only); just refresh
        // the schema so the grid repaints declared badges + display formats.
        if (get().activeId === meta.id) set({ schemaInfo: info });
        // The declared type re-derives any open column profile (typed stats,
        // inferredKind, null exclusion), which the backend keys on the schema
        // revision — re-request so the explorer panel repaints, not stale.
        void get().refreshExplorerProfile();
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    removeColumnSchema: async (columnId) => {
      const meta = activeMeta();
      if (!meta) return;
      try {
        const info = await api.removeColumnSchema(meta.id, columnId);
        if (get().activeId === meta.id) set({ schemaInfo: info });
        void get().refreshExplorerProfile();
      } catch (e) {
        set({ error: String(e) });
      }
    },

    inferAndApplySchema: async () => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        // Inference is a full-document scan (every column, every candidate
        // type) — run it as a cancellable job, then apply the entries.
        const jobId = await api.startInferSchema(meta.id, meta.revision);
        set({ schemaScan: { jobId, kind: "infer", columnId: null, processed: 0, total: null } });
        consumeEarlyFinish(jobId);
        const finished = await awaitJob(jobId);
        set({ schemaScan: null });
        if (finished.status !== "done") {
          if (finished.status === "failed") {
            set({ error: finished.error ?? "schema inference failed" });
          }
          return false;
        }
        const inferred = await api.takeInferredSchema(meta.id);
        if (!inferred) return false;
        // Apply each inferred entry (cheap, non-dirty, non-undoable metadata).
        for (const entry of Object.values(inferred.columns)) {
          await api.setColumnSchema(meta.id, entry);
        }
        const info = await api.getSchema(meta.id);
        if (get().activeId === meta.id) set({ schemaInfo: info });
        void get().refreshExplorerProfile();
        return true;
      } catch (e) {
        set({ schemaScan: null, error: String(e) });
        return false;
      }
    },

    importSchemaFromFile: async () => {
      const meta = activeMeta();
      if (!meta) return null;
      const chosen = await openFileDialog({
        filters: [{ name: "Schema JSON", extensions: ["json"] }],
      });
      if (typeof chosen !== "string") return null;
      try {
        const outcome = await api.importSchema(meta.id, chosen);
        if (get().activeId === meta.id) set({ schemaInfo: outcome.info });
        const applied = `Applied ${outcome.applied} column schema${outcome.applied === 1 ? "" : "s"}`;
        return outcome.skippedUnknown.length > 0
          ? `${applied}; skipped ${outcome.skippedUnknown.length} entr${
              outcome.skippedUnknown.length === 1 ? "y" : "ies"
            } for columns not in this document.`
          : `${applied}.`;
      } catch (e) {
        set({ error: String(e) });
        return null;
      }
    },

    exportSchemaToFile: async () => {
      const meta = activeMeta();
      if (!meta) return;
      const base = (meta.fileName || "schema").replace(/\.[^.]+$/, "");
      const chosen = await saveFileDialog({
        defaultPath: `${base}.schema.json`,
        filters: [{ name: "Schema JSON", extensions: ["json"] }],
      });
      if (!chosen) return;
      try {
        await api.exportSchema(meta.id, chosen);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    runSchemaInvalidSamples: async (columnId, maxSamples) => {
      const meta = activeMeta();
      if (!meta) return null;
      try {
        const jobId = await api.startSchemaInvalidSamples(
          meta.id,
          columnId,
          maxSamples,
          meta.revision,
        );
        set({ schemaScan: { jobId, kind: "invalid", columnId, processed: 0, total: null } });
        consumeEarlyFinish(jobId);
        const finished = await awaitJob(jobId);
        set({ schemaScan: null });
        if (finished.status !== "done") {
          if (finished.status === "failed") set({ error: finished.error ?? "scan failed" });
          return null;
        }
        const report = await api.takeSchemaInvalidSamples(meta.id);
        // Defensive: the cache is keyed by document, so verify the report is
        // for the column we asked about before surfacing it.
        return report && report.columnId === columnId ? report : null;
      } catch (e) {
        set({ schemaScan: null, error: String(e) });
        return null;
      }
    },

    runSchemaConvertPreview: async (columnId, maxSamples) => {
      const meta = activeMeta();
      if (!meta) return null;
      try {
        const jobId = await api.startConvertColumnPreview(
          meta.id,
          columnId,
          maxSamples,
          meta.revision,
        );
        set({ schemaScan: { jobId, kind: "preview", columnId, processed: 0, total: null } });
        consumeEarlyFinish(jobId);
        const finished = await awaitJob(jobId);
        set({ schemaScan: null });
        if (finished.status !== "done") {
          if (finished.status === "failed") set({ error: finished.error ?? "preview failed" });
          return null;
        }
        const preview = await api.takeConvertColumnPreview(meta.id);
        return preview && preview.columnId === columnId ? preview : null;
      } catch (e) {
        set({ schemaScan: null, error: String(e) });
        return null;
      }
    },

    cancelSchemaScan: async () => {
      const scan = get().schemaScan;
      if (scan) await api.cancelJob(scan.jobId).catch(() => undefined);
    },

    applyColumnConversion: async (columnId, expectedRevision, expectedSchemaRevision) => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        const jobId = await api.convertColumnApply(
          meta.id,
          columnId,
          expectedRevision,
          expectedSchemaRevision,
        );
        set({ schemaConvert: { jobId, columnId, processed: 0, total: null } });
        consumeEarlyFinish(jobId);
        const finished = await awaitJob(jobId);
        set({ schemaConvert: null });
        if (finished.status !== "done") {
          if (finished.status === "failed") {
            set({ error: finished.error ?? "conversion failed" });
          }
          return false;
        }
        const updated = await api.getMeta(meta.id);
        reloadDoc(updated);
        // The document revision moved; keep the badge/formatting schema fresh.
        void get().loadSchema();
        return true;
      } catch (e) {
        set({ schemaConvert: null, error: String(e) });
        return false;
      }
    },

    cancelColumnConversion: async () => {
      const convert = get().schemaConvert;
      if (convert) await api.cancelJob(convert.jobId).catch(() => undefined);
    },

    // ----- data dictionary (F38) ---------------------------------------------

    openDictionaryDialog: (col) =>
      set({ dictionaryDialogColumn: col ?? null, activeModal: "dictionary" }),

    loadDictionary: async () => {
      const meta = activeMeta();
      if (!meta) {
        set({ dictionaryView: null });
        return;
      }
      try {
        const view = await api.getDictionary(meta.id);
        // A tab switch may have landed during the await; don't cross-install.
        if (get().activeId === meta.id) set({ dictionaryView: view });
      } catch {
        // A dictionary fetch failure is non-fatal: header tooltips stay off.
      }
    },

    setDictionaryField: async (field) => {
      const meta = activeMeta();
      const view = get().dictionaryView;
      if (!meta || !view) return false;
      try {
        // Guarded by the metadata revision the current view was taken at; a
        // documentation edit never dirties the document or moves `revision`.
        const next = await api.setDictionaryField(meta.id, field, view.dictionaryRevision);
        if (get().activeId === meta.id) set({ dictionaryView: next });
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    removeDictionaryField: async (columnId) => {
      const meta = activeMeta();
      const view = get().dictionaryView;
      if (!meta || !view) return false;
      try {
        const next = await api.removeDictionaryField(meta.id, columnId, view.dictionaryRevision);
        if (get().activeId === meta.id) set({ dictionaryView: next });
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    discardDictionaryOrphans: async () => {
      const meta = activeMeta();
      const view = get().dictionaryView;
      if (!meta || !view) return false;
      try {
        const next = await api.discardDictionaryOrphans(meta.id, view.dictionaryRevision);
        if (get().activeId === meta.id) set({ dictionaryView: next });
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    exportDictionaryToFile: async (format) => {
      const meta = activeMeta();
      if (!meta) return;
      const base = (meta.fileName || "dictionary").replace(/\.[^.]+$/, "");
      const ext = format === "json" ? "json" : format === "markdown" ? "md" : "csv";
      const filterName =
        format === "json"
          ? "Dictionary JSON"
          : format === "markdown"
            ? "Markdown documentation"
            : "CSV documentation";
      const chosen = await saveFileDialog({
        defaultPath: `${base}.dictionary.${ext}`,
        filters: [{ name: filterName, extensions: [ext] }],
      });
      if (!chosen) return;
      try {
        await api.exportDictionary(meta.id, chosen, format);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    pickDictionaryImportFile: async () => {
      const chosen = await openFileDialog({
        filters: [{ name: "Dictionary JSON", extensions: ["json"] }],
      });
      return typeof chosen === "string" ? chosen : null;
    },

    previewDictionaryImport: async (path, matchBy) => {
      const meta = activeMeta();
      if (!meta) return null;
      try {
        // Read-only: nothing changes until the user resolves conflicts + applies.
        return await api.previewDictionaryImport(meta.id, path, matchBy);
      } catch (e) {
        set({ error: String(e) });
        return null;
      }
    },

    applyDictionaryImport: async (path, matchBy, resolution, expectedDictionaryRevision) => {
      const meta = activeMeta();
      if (!meta) return null;
      try {
        // Guard with the revision the PLAN was computed against — not the live
        // view — so a documentation edit landed after the preview (which bumps
        // the dictionary revision) makes the backend reject this stale apply
        // instead of silently overwriting the fresher edit.
        const outcome = await api.applyDictionaryImport(
          meta.id,
          path,
          matchBy,
          resolution,
          expectedDictionaryRevision,
        );
        if (get().activeId === meta.id) set({ dictionaryView: outcome.view });
        return outcome;
      } catch (e) {
        set({ error: String(e) });
        return null;
      }
    },

    // ----- row bookmarks, tags & notes (F40) ---------------------------------

    loadAnnotations: async () => {
      const meta = activeMeta();
      if (!meta) {
        set({ annotationsView: null });
        return;
      }
      try {
        // Reads the registry and re-resolves against the current view; a tab
        // switch may have landed during the await, so don't cross-install.
        const view = await api.annotationsView(meta.id);
        if (get().activeId === meta.id) set({ annotationsView: view });
      } catch {
        // Non-fatal: the grid simply shows no indicators until the next load.
      }
    },

    hydrateAnnotationsFromSidecar: async (docId, sourcePath) => {
      try {
        // With a project open (or one mid-open), the project's `annotations`
        // section is authoritative — never let the per-source sidecar clobber
        // it (F40 migration rule: the project absorbs the sidecar on save).
        // Fall back to a plain registry view so the grid indicators still
        // refresh without reading — or overwriting — the sidecar.
        const useSidecar = !!sourcePath && !openingProject && !get().project;
        const view = useSidecar
          ? await api.annotationsLoadSidecar(docId, sourcePath as string)
          : await api.annotationsView(docId);
        if (get().activeId === docId) set({ annotationsView: view });
      } catch {
        // A malformed/absent sidecar must never block opening the file.
      }
    },

    setAnnotationsPanelOpen: (open) => {
      set((s) => ({
        annotationsPanelOpen: open,
        diagnosticsOpen: open ? false : s.diagnosticsOpen,
        changesOpen: open ? false : s.changesOpen,
        explorer: open ? { ...s.explorer, open: false } : s.explorer,
      }));
      if (open) void get().loadAnnotations();
    },

    applyRowMarks: async (displayRows, patch) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view || displayRows.length === 0) return false;
      let rev = view.annotationsRevision;
      let next = view;
      try {
        // Each edit bumps the annotations revision, so thread it across the
        // batch. A mid-batch failure leaves the store ahead of our revision;
        // resync by reloading.
        for (const row of displayRows) {
          next = await api.annotationsEditRow(meta.id, row, patch, rev);
          rev = next.annotationsRevision;
        }
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        void get().loadAnnotations();
        return false;
      }
    },

    setRowNote: async (displayRow, text) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        // author = null: the backend fills the store's configured default.
        const next = await api.annotationsSetRowNote(
          meta.id,
          displayRow,
          text,
          null,
          view.annotationsRevision,
        );
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    setCellNote: async (displayRow, columnId, text) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        const next = await api.annotationsSetCellNote(
          meta.id,
          displayRow,
          columnId,
          text,
          null,
          view.annotationsRevision,
        );
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    defineAnnotationTag: async (tag) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        const next = await api.annotationsDefineTag(meta.id, tag, view.annotationsRevision);
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    removeAnnotationTag: async (name) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        const next = await api.annotationsRemoveTag(meta.id, name, view.annotationsRevision);
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    removeAnnotation: async (handle) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        const next = await api.annotationsRemoveRow(meta.id, handle, view.annotationsRevision);
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    discardAnnotationOrphans: async () => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        const next = await api.annotationsDiscardOrphans(meta.id, view.annotationsRevision);
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    setAnnotationAuthor: async (author) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        const next = await api.annotationsSetAuthor(meta.id, author, view.annotationsRevision);
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    setAnnotationKeySpec: async (keySpec) => {
      const meta = activeMeta();
      const view = get().annotationsView;
      if (!meta || !view) return false;
      try {
        const next = await api.annotationsSetKeySpec(meta.id, keySpec, view.annotationsRevision);
        if (get().activeId === meta.id) set({ annotationsView: next });
        persistAnnotationSidecar(meta.id, meta.path);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    applyAnnotationFilter: async (predicate) => {
      const meta = activeMeta();
      if (!meta) return;
      try {
        // Integrates with the existing row-filter view; only MATCHED rows are
        // filtered onto (the backend refuses to filter onto uncertain rows).
        const updated = await api.applyAnnotationFilter(meta.id, predicate, meta.revision);
        reloadDoc(updated);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    previewTagToColumn: async (tag) => {
      const meta = activeMeta();
      if (!meta) return null;
      try {
        return await api.previewTagToColumn(meta.id, tag);
      } catch (e) {
        set({ error: String(e) });
        return null;
      }
    },

    applyTagToColumn: async (tag, target) => {
      const meta = activeMeta();
      if (!meta) return false;
      try {
        // One undoable document op; the notes themselves are untouched. The
        // grid reload re-resolves the annotations against the new structure.
        const updated = await api.applyTagToColumn(meta.id, tag, target, meta.revision);
        reloadDoc(updated);
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    exportAnnotationsToFile: async (format) => {
      const meta = activeMeta();
      if (!meta) return;
      const ext = format === "json" ? "json" : "csv";
      const chosen = await saveFileDialog({
        defaultPath: annotationExportName(meta.fileName || "annotations", ext),
        filters: [
          { name: format === "json" ? "Annotations JSON" : "Annotations CSV", extensions: [ext] },
        ],
      });
      if (typeof chosen !== "string") return;
      try {
        await api.exportAnnotations(meta.id, chosen, format);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    openRowNoteEditor: (displayRow, label, initialText = "") =>
      set({ annotationNoteTarget: { displayRow, columnId: null, label, initialText } }),
    openCellNoteEditor: (displayRow, columnId, label, initialText = "") =>
      set({ annotationNoteTarget: { displayRow, columnId, label, initialText } }),
    closeNoteEditor: () => set({ annotationNoteTarget: null }),
    openTagPicker: (displayRows) => set({ annotationTagTarget: { displayRows } }),
    closeTagPicker: () => set({ annotationTagTarget: null }),
    openTagToColumn: (tag) => set({ tagToColumnTag: tag, activeModal: "tagToColumn" }),
    closeTagToColumn: () => set({ tagToColumnTag: null, activeModal: null }),

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

    setCell: async (row, col, value) => {
      const id = get().activeId;
      if (id == null) return;
      try {
        const meta = await api.setCell(id, row, col, value);
        refreshMeta(meta);
      } catch (e) {
        // F31 strict mode rejects an invalid edit server-side. An inline grid
        // edit already wrote the value into the windowed cache optimistically;
        // invalidate it so the true (unchanged) value repaints instead of the
        // rejected one lingering. The error toast explains why.
        set({ error: String(e) });
        get().invalidateGrid();
      }
    },
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
      const scope = findSelectionScope(find.inSelection, selectionRect);
      if (scope.blocked) {
        set({ error: FIND_SELECTION_BLOCKED });
        return;
      }
      const options: FindOptions = {
        query: find.query,
        regex: find.regex,
        caseSensitive: find.caseSensitive,
        wholeCell: find.wholeCell,
        selection: scope.rect,
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
        const scope = findSelectionScope(find.inSelection, selectionRect);
        const matches = await api.find(id, {
          ...options,
          selection: scope.blocked ? undefined : scope.rect,
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
      const scope = findSelectionScope(find.inSelection, selectionRect);
      if (scope.blocked) {
        set({ error: FIND_SELECTION_BLOCKED });
        return;
      }
      const options: FindOptions = {
        query: find.query,
        regex: find.regex,
        caseSensitive: find.caseSensitive,
        wholeCell: find.wholeCell,
        selection: scope.rect,
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
        changesOpen: open ? false : s.changesOpen,
        explorer: open ? { ...s.explorer, open: false } : s.explorer,
        annotationsPanelOpen: open ? false : s.annotationsPanelOpen,
        recordFormOpen: open ? false : s.recordFormOpen,
      })),

    setChangesOpen: (open) =>
      set((s) => ({
        changesOpen: open,
        diagnosticsOpen: open ? false : s.diagnosticsOpen,
        explorer: open ? { ...s.explorer, open: false } : s.explorer,
        annotationsPanelOpen: open ? false : s.annotationsPanelOpen,
        recordFormOpen: open ? false : s.recordFormOpen,
      })),

    // ----- record form (F41) ------------------------------------------------

    setRecordFormOpen: (open) =>
      set((s) => {
        const meta = s.tabs.find((t) => t.id === s.activeId);
        // Clamp the remembered record into the current visible range on open,
        // so a filter applied since it was last shown can't strand the form.
        const row = clampRecord(s.record.row, meta?.rowCount ?? 0) ?? 0;
        // If the row had to move, its draft no longer belongs to the shown
        // record — drop it; an unchanged row keeps its draft across a reopen.
        const record = !open
          ? s.record
          : row === s.record.row
            ? { ...s.record, row }
            : { ...s.record, row, draft: {}, draftRevision: null };
        return {
          recordFormOpen: open,
          record,
          // One side panel at a time.
          changesOpen: open ? false : s.changesOpen,
          diagnosticsOpen: open ? false : s.diagnosticsOpen,
          explorer: open ? { ...s.explorer, open: false } : s.explorer,
        };
      }),

    setRecordRow: (row) =>
      set((s) => ({
        // Moving to another record always starts clean — a draft belongs to the
        // record it was made on; the caller resolves it (save/discard) first.
        record: { ...s.record, row, draft: {}, draftRevision: null },
      })),

    setRecordDraftField: (col, value) =>
      set((s) => ({
        record: {
          ...s.record,
          draft: { ...s.record.draft, [col]: value },
          // Pin the draft to the revision it began at, so a later refetch at a
          // different revision can safely discard it (the row may have moved).
          draftRevision: s.record.draftRevision ?? activeMeta()?.revision ?? null,
        },
      })),

    clearRecordDraft: () =>
      set((s) => ({ record: { ...s.record, draft: {}, draftRevision: null } })),

    saveRecordDraft: async (cells) => {
      const id = get().activeId;
      if (id == null || cells.length === 0) return false;
      try {
        // One batched, F31-validated commit → exactly one undo step for the
        // whole record. A strict-invalid batch is rejected here (the UI gates
        // Save on the pre-check, so this is defence in depth).
        const meta = await api.setCells(id, cells);
        reloadDoc(meta);
        set((s) => ({ record: { ...s.record, draft: {}, draftRevision: null } }));
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    setRecordLayout: (layout) => set((s) => ({ record: { ...s.record, layout } })),

    jumpToRecordColumn: (col) => {
      // The record row is a DISPLAY coordinate already (fetch_record used
      // display coords), so — unlike a diagnostics jump — the row view must NOT
      // be reset; the grid maps the physical column to its display position.
      jumpNonce += 1;
      set((s) => ({ jumpTarget: { row: s.record.row, col, nonce: jumpNonce } }));
    },

    setAutoSaveRecordOnNavigate: async (enabled) => {
      const settings: AppSettings = {
        ...(get().settings ?? { version: 1, profiles: [] }),
        autoSaveRecordOnNavigate: enabled,
      };
      set({ settings });
      try {
        await api.setSettings(settings);
      } catch (e) {
        set({ error: String(e) });
      }
    },

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
      // or view-sorted (F12) would land on the wrong (display) row and the
      // target may be hidden anyway, so drop the whole row view first.
      if (meta.filtered || meta.viewSorted) {
        try {
          const updated = await api.resetRowView(meta.id);
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

      if (progress.kind === "sample") {
        const sample = get().sample;
        if (sample?.jobId !== progress.jobId) return;
        set({
          sample: {
            ...sample,
            processed: progress.processed,
            total: progress.total,
            message: progress.message ?? sample.message,
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

      // F33: JSON import preview scan progress (guarded by our own job id so a
      // stray "scan"-kind job from elsewhere never touches this state).
      if (progress.kind === "scan") {
        const st = get().jsonImport;
        if (!st || st.scanJobId !== progress.jobId) return;
        set({
          jsonImport: { ...st, scanProcessed: progress.processed, scanTotal: progress.total },
        });
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

      if (progress.kind === "schemaConvert") {
        const convert = get().schemaConvert;
        if (convert?.jobId !== progress.jobId) return;
        set({
          schemaConvert: { ...convert, processed: progress.processed, total: progress.total },
        });
        return;
      }

      if (progress.kind === "schemaScan") {
        const scan = get().schemaScan;
        if (scan?.jobId !== progress.jobId) return;
        set({ schemaScan: { ...scan, processed: progress.processed, total: progress.total } });
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
          // F40: hydrate annotations from the extracted file's sidecar so a
          // later edit merges into any existing notes instead of overwriting
          // an empty store on top of them.
          void get().hydrateAnnotationsFromSidecar(meta.id, meta.path);
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
            // F40: hydrate annotations from the source's sidecar. Indexed
            // (F10) opens land here too; without this an annotation edit on a
            // large read-only document would overwrite its existing sidecar
            // with an otherwise-empty store. Skipped for the in-place
            // convert/reindex cases below — the doc id (and its registry
            // store) is stable across those.
            void get().hydrateAnnotationsFromSidecar(meta.id, meta.path);
            // F12: restore the matching profile's last-selected view (works
            // on indexed documents — layout, filter and view sort are all
            // non-destructive).
            void restoreLastViewFor(meta).catch(() => undefined);
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
          // F33: a successful JSON import closes its dialog (the document is
          // now open); a failure above leaves it open to show the error.
          if (derive.kind === "jsonImport") set({ jsonImport: null });
        } catch (e) {
          set({ deriveError: String(e) });
        }
        return;
      }

      if (finished.kind === "sample") {
        const sample = get().sample;
        if (sample?.jobId !== finished.jobId) return;
        set({ sample: null });
        if (finished.status !== "done") {
          // A cancel is silent (incomplete outputs were already removed); only
          // a genuine failure surfaces to the dialog.
          if (finished.status === "failed") {
            set({ sampleError: finished.error ?? "the operation failed" });
          }
          return;
        }
        // Direct exports leave nothing to open; derived outputs each registered
        // a NEW document — add every tab and focus the first.
        if (sample.destination === "derivedDocuments" && sample.docIds.length > 0) {
          try {
            const metas = await Promise.all(sample.docIds.map((id) => api.getMeta(id)));
            set((s) => ({ ...switchPatch(s, metas[0].id), tabs: [...s.tabs, ...metas] }));
          } catch (e) {
            set({ sampleError: String(e) });
          }
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
        // Persist the project too, so a dirty workspace isn't lost on quit.
        if (get().isProjectDirty()) {
          const saved = await get().projectSave(false);
          if (!saved) {
            set({
              quitPromptOpen: false,
              error: "Quit cancelled — the project was not saved.",
            });
            return;
          }
        }
      }
      set({ quitPromptOpen: false });
      await getCurrentWindow().destroy();
    },

    // ----- project workspaces (F37) -------------------------------------------

    isProjectDirty: () => {
      const s = get();
      if (!s.project) return false;
      return deriveProjectDirty(s.projectBaseline, currentProjectSnapshot());
    },

    projectNew: async (templatePath) => {
      if (!(await guardDiscardProject())) return;
      try {
        const meta = await api.projectNew(templatePath);
        set({
          project: meta,
          projectBaseline: currentProjectSnapshot(),
          projectWarnings: [],
          projectOpen: null,
          projectOpenChoices: {},
        });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    projectNewFromTemplate: async () => {
      const chosen = await openFileDialog({ multiple: false, filters: PROJECT_FILTERS });
      if (typeof chosen === "string") await get().projectNew(chosen);
    },

    projectPickAndOpen: async () => {
      if (!(await guardDiscardProject())) return;
      const chosen = await openFileDialog({ multiple: false, filters: PROJECT_FILTERS });
      if (typeof chosen !== "string") return;
      try {
        const preview = await api.projectOpenPreview(chosen);
        set({ projectOpen: preview, projectOpenChoices: {} });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    setProjectChoice: (sourceId, choice) =>
      set((s) => ({ projectOpenChoices: { ...s.projectOpenChoices, [sourceId]: choice } })),

    projectLocateSource: async (sourceId) => {
      const chosen = await openFileDialog({ multiple: false, filters: FILE_FILTERS });
      if (typeof chosen !== "string") return;
      set((s) => ({
        projectOpenChoices: {
          ...s.projectOpenChoices,
          [sourceId]: { action: "locate", locatePath: chosen },
        },
      }));
    },

    cancelProjectOpen: () => set({ projectOpen: null, projectOpenChoices: {} }),

    applyProjectOpen: async () => {
      const preview = get().projectOpen;
      if (!preview) return;
      const resolutions = buildResolutions(preview.sources, get().projectOpenChoices);
      try {
        const plan = await api.projectOpenApply(preview.path, resolutions);
        await applyProjectPlan(plan);
        if (preview.path) pushRecent(preview.path);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    projectOpenAvailableOnly: async () => {
      const preview = get().projectOpen;
      if (!preview) return;
      set({ projectOpenChoices: availableOnlyChoices(preview.sources) });
      await get().applyProjectOpen();
    },

    projectSave: async (saveAs) => {
      const project = get().project;
      if (!project) return false;
      try {
        await captureProjectSections();
        let meta: ProjectMeta;
        if (saveAs || !project.path) {
          const chosen = await saveFileDialog({
            defaultPath: `${project.name || "project"}.ceesveeproj`,
            filters: PROJECT_FILTERS,
          });
          if (!chosen) return false;
          meta = await api.projectSaveAs(chosen);
        } else {
          meta = await api.projectSave();
        }
        set({ project: meta, projectBaseline: currentProjectSnapshot() });
        return true;
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    },

    projectSaveTemplate: async () => {
      if (!get().project) return;
      const chosen = await saveFileDialog({
        defaultPath: "template.ceesveeproj",
        filters: PROJECT_FILTERS,
      });
      if (!chosen) return;
      try {
        // Persist live sections first so the template reflects current config.
        await captureProjectSections();
        await api.projectSaveTemplate(chosen);
      } catch (e) {
        set({ error: String(e) });
      }
    },

    requestCloseProject: () => {
      if (get().isProjectDirty()) set({ projectClosePromptOpen: true });
      else void get().closeProjectNow();
    },

    closeProjectNow: async () => {
      await api.projectClose().catch(() => undefined);
      set({
        project: null,
        projectBaseline: null,
        projectWarnings: [],
        projectClosePromptOpen: false,
        projectOpen: null,
        projectOpenChoices: {},
      });
    },

    dismissProjectWarnings: () => set({ projectWarnings: [] }),

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

    // ----- JSON / JSON Lines interoperability (F33) -------------------------------

    openJsonImport: async (path) => {
      const fileName = path.split(/[\\/]/).pop() ?? path;
      set({
        jsonImport: {
          path,
          fileName,
          options: defaultImportOptions(),
          scanJobId: null,
          scanProcessed: 0,
          scanTotal: null,
          preview: null,
          scanError: null,
        },
      });
      await get().runJsonScan(defaultImportOptions());
    },

    runJsonScan: async (options) => {
      const st = get().jsonImport;
      if (!st) return;
      // Supersede any in-flight scan: cancel it and reset progress.
      if (st.scanJobId != null) void api.cancelJob(st.scanJobId).catch(() => undefined);
      set((s) =>
        s.jsonImport
          ? {
              jsonImport: {
                ...s.jsonImport,
                scanJobId: null,
                scanProcessed: 0,
                scanTotal: null,
                scanError: null,
              },
            }
          : {},
      );
      try {
        const jobId = await api.jsonImportPreview(st.path, options);
        // The dialog may have closed while the invoke was in flight.
        if (get().jsonImport?.path !== st.path) return;
        set((s) => (s.jsonImport ? { jsonImport: { ...s.jsonImport, scanJobId: jobId } } : {}));
        const finished = await awaitJob(jobId);
        // A newer scan may have superseded this one.
        if (get().jsonImport?.scanJobId !== jobId) return;
        if (finished.status === "done") {
          const preview = await api.getJsonImportPreview(jobId);
          set((s) =>
            s.jsonImport
              ? {
                  jsonImport: {
                    ...s.jsonImport,
                    scanJobId: null,
                    preview,
                    options,
                    scanError: null,
                  },
                }
              : {},
          );
        } else if (finished.status === "failed") {
          set((s) =>
            s.jsonImport
              ? {
                  jsonImport: {
                    ...s.jsonImport,
                    scanJobId: null,
                    scanError: finished.error ?? "scan failed",
                  },
                }
              : {},
          );
        } else {
          // Cancelled: just clear the in-flight marker.
          set((s) => (s.jsonImport ? { jsonImport: { ...s.jsonImport, scanJobId: null } } : {}));
        }
      } catch (e) {
        if (get().jsonImport?.path !== st.path) return;
        set((s) =>
          s.jsonImport
            ? { jsonImport: { ...s.jsonImport, scanJobId: null, scanError: String(e) } }
            : {},
        );
      }
    },

    cancelJsonScan: async () => {
      const jobId = get().jsonImport?.scanJobId;
      if (jobId != null) await api.cancelJob(jobId).catch(() => undefined);
    },

    applyJsonImport: async (options) => {
      const st = get().jsonImport;
      // One derive slot at a time (shared with append/join/group/reshape).
      if (!st || get().derive) return;
      set({ deriveError: null });
      try {
        const started = await api.jsonImportApply(st.path, options);
        get().trackDerive(started.jobId, started.docId, "jsonImport");
        pushRecent(st.path);
      } catch (e) {
        set({ deriveError: String(e) });
      }
    },

    dismissJsonImport: () => {
      const jobId = get().jsonImport?.scanJobId;
      if (jobId != null) void api.cancelJob(jobId).catch(() => undefined);
      set({ jsonImport: null });
    },

    exportJson: async (options, scope) => {
      const meta = activeMeta();
      if (!meta) return;
      const chosen = await saveFileDialog({
        defaultPath: suggestJsonFileName(meta.fileName, options.format),
        filters: JSON_FILE_FILTERS,
      });
      if (!chosen) return;
      try {
        // plan() rejects invalid options / duplicate output paths here, BEFORE
        // any job spawns — the invoke itself throws and we surface it.
        const jobId = await api.jsonExport(meta.id, chosen, options, scope, meta.revision);
        const finished = await awaitJob(jobId);
        if (finished.status === "failed") {
          set({ error: finished.error ?? "JSON export failed" });
        }
      } catch (e) {
        set({ error: String(e) });
      }
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
        recordFormOpen: open ? false : s.recordFormOpen,
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

/**
 * Whether the open project has unsaved changes (F37). Derived reactively from
 * the open documents, active tab, panel layout and each document's active named
 * view versus the snapshot captured at the last save/open, so the dirty dot
 * updates without event bookkeeping.
 */
export function useProjectDirty(): boolean {
  return useStore((s) => {
    if (s.project == null) return false;
    const activeViews: Record<number, string | null> = {};
    for (const t of s.tabs) {
      activeViews[t.id] =
        t.id === s.activeId ? s.activeViewId : (s.uiStates[t.id]?.activeViewId ?? null);
    }
    return deriveProjectDirty(
      s.projectBaseline,
      projectSnapshot(
        s.tabs,
        s.activeId,
        {
          diagnostics: s.diagnosticsOpen,
          explorer: s.explorer.open,
          changes: s.changesOpen,
        },
        activeViews,
      ),
    );
  });
}
