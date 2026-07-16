// Thin, typed wrappers around the Rust command surface. Keeping every `invoke`
// in one place gives the rest of the app a clean, discoverable API and a single
// spot to evolve the contract.

import { invoke } from "@tauri-apps/api/core";
import type {
  AppendInput,
  AppendOptions,
  AppendPreview,
  AppendReport,
  AppSettings,
  ArchiveExtractStart,
  CellRect,
  ClusterReport,
  ClusterSpec,
  ColumnProfile,
  ColumnSummary,
  CompareInfo,
  ComparePage,
  CompareSpec,
  CopyFormat,
  CrossRule,
  CrossValReport,
  PastePreview,
  PasteSpecialOptions,
  DedupSpec,
  DiagnosticsReport,
  DiffStatus,
  DocumentMeta,
  DuplicateKeepStrategy,
  DuplicateReport,
  EncodingCompatibility,
  ExportOptions,
  ExportScope,
  ExternalChange,
  FileFingerprint,
  FileProfile,
  FilterGroup,
  FindMatch,
  FindOptions,
  GroupByPreview,
  GroupBySpec,
  IndexedOpenStart,
  JoinPreview,
  JoinSpec,
  OpenEstimate,
  OpenOptions,
  OutlierAction,
  OutlierActionPreview,
  OutlierReport,
  OutlierSpec,
  ProfileScope,
  ProfileValidation,
  RepairPreview,
  RepairSpec,
  ReparsePreview,
  ReplaceResult,
  ReshapePreview,
  ReshapeSpec,
  RowsResponse,
  ScopeCounts,
  SelectionStats,
  SemanticAction,
  SemanticActionPreview,
  SemanticReport,
  SemanticType,
  SortKey,
  SplitOptions,
  TransformErrorPolicy,
  TransformPreview,
  TransformSpec,
  ZipEntryInfo,
} from "../types";

export const openFile = (path: string, options?: OpenOptions) =>
  invoke<DocumentMeta>("open_file", { path, options });

/** The complete content of one cell, in display coordinates (F13). */
export const getCell = (docId: number, row: number, col: number) =>
  invoke<string>("get_cell", { docId, row, col });

/**
 * Serialize a selection into a structured clipboard format (F14). `rows` are
 * display indices; `null` copies every visible row.
 */
export const copyAs = (
  docId: number,
  rows: number[] | null,
  cols: number[],
  includeHeaders: boolean,
  format: CopyFormat,
) => invoke<string>("copy_as", { docId, rows, cols, includeHeaders, format });

/** Preview a Paste Special without mutating (F14). */
export const previewPasteSpecial = (
  docId: number,
  text: string,
  options: PasteSpecialOptions,
  anchorRow: number,
  anchorCol: number,
  selectionRows: number,
  selectionCols: number,
) =>
  invoke<PastePreview>("preview_paste_special", {
    docId,
    text,
    options,
    anchorRow,
    anchorCol,
    selectionRows,
    selectionCols,
  });

/** Apply a previewed Paste Special as one undo step (F14). */
export const applyPasteSpecial = (
  docId: number,
  text: string,
  options: PasteSpecialOptions,
  anchorRow: number,
  anchorCol: number,
  selectionRows: number,
  selectionCols: number,
  expectedRevision: number,
) =>
  invoke<DocumentMeta>("apply_paste_special", {
    docId,
    text,
    options,
    anchorRow,
    anchorCol,
    selectionRows,
    selectionCols,
    expectedRevision,
  });

/** Estimate the in-memory cost of opening a file editable (F10). */
export const probeOpen = (path: string) => invoke<OpenEstimate>("probe_open", { path });

/** List the entries of a ZIP archive (F17). */
export const listArchiveEntries = (path: string) =>
  invoke<ZipEntryInfo[]>("list_archive_entries", { path });

/** Extract a gzip member or ZIP entry as a cancellable job (F17). */
export const startArchiveExtract = (path: string, entry: string | null, allowLarge: boolean) =>
  invoke<ArchiveExtractStart>("start_archive_extract", { path, entry, allowLarge });

/** Estimate the extracted entry's in-memory cost (F17). */
export const pendingArchiveEstimate = (token: number) =>
  invoke<OpenEstimate>("pending_archive_estimate", { token });

/** Open a parked extraction as a document (F17). */
export const openArchiveDocument = (
  token: number,
  mode: "editable" | "indexed",
  options?: OpenOptions,
) => invoke<IndexedOpenStart>("open_archive_document", { token, mode, options });

/** Drop a parked extraction and delete its cache (F17). */
export const discardArchive = (token: number) => invoke<void>("discard_archive", { token });

/** The last completed cluster report for a document (F24). */
export const getClusterReport = (docId: number) =>
  invoke<ClusterReport | null>("get_cluster_report", { docId });

/** Start a fuzzy clustering scan as a cancellable job (F24). */
export const startClusterScan = (docId: number, spec: ClusterSpec, expectedRevision: number) =>
  invoke<number>("start_cluster_scan", { docId, spec, expectedRevision });

/** Apply accepted cluster mappings as one undo step (F24). */
export const applyValueClusters = (
  docId: number,
  column: number,
  mapping: [string, string][],
  scope: ExportScope,
  expectedRevision: number,
) =>
  invoke<DocumentMeta>("apply_value_clusters", {
    docId,
    column,
    mapping,
    scope,
    expectedRevision,
  });

/** The last completed semantic-type report for a document (F26). */
export const getSemanticReport = (docId: number) =>
  invoke<SemanticReport | null>("get_semantic_report", { docId });

/** Start a semantic-type scan over every column as a cancellable job (F26). */
export const startSemanticScan = (docId: number, expectedRevision: number) =>
  invoke<number>("start_semantic_scan", { docId, expectedRevision });

/** Filter to rows (in)valid for a semantic type; blanks match neither (F26). */
export const applySemanticFilter = (
  docId: number,
  column: number,
  semantic: SemanticType,
  keepValid: boolean,
  expectedRevision: number,
) =>
  invoke<DocumentMeta>("apply_semantic_filter", {
    docId,
    column,
    semantic,
    keepValid,
    expectedRevision,
  });

/** Preview exactly what a semantic quick action would change (F26). */
export const previewSemanticAction = (
  docId: number,
  column: number,
  semantic: SemanticType,
  action: SemanticAction,
  expectedRevision: number,
) =>
  invoke<SemanticActionPreview>("preview_semantic_action", {
    docId,
    column,
    semantic,
    action,
    expectedRevision,
  });

/** Apply a previewed semantic action as ONE undo step (F26). */
export const applySemanticAction = (
  docId: number,
  column: number,
  semantic: SemanticType,
  action: SemanticAction,
  expectedRevision: number,
) =>
  invoke<DocumentMeta>("apply_semantic_action", {
    docId,
    column,
    semantic,
    action,
    expectedRevision,
  });

/** Preview an append: schema, mappings, projections. Creates nothing (F20). */
export const previewAppend = (inputs: AppendInput[], options: AppendOptions) =>
  invoke<AppendPreview>("preview_append", { inputs, options });

/** Run an append as a cancellable "derive" job; a NEW document registers
 *  under the returned docId when it finishes (F20). */
export const startAppend = (inputs: AppendInput[], options: AppendOptions) =>
  invoke<IndexedOpenStart>("start_append", { inputs, options });

/** The per-input outcome report of a finished append (F20). */
export const getAppendReport = (docId: number) =>
  invoke<AppendReport | null>("get_append_report", { docId });

/** Preview a reshape — creates nothing (F23). */
export const previewReshape = (docId: number, spec: ReshapeSpec, expectedRevision: number) =>
  invoke<ReshapePreview>("preview_reshape", { docId, spec, expectedRevision });

/** Run a reshape as a cancellable "derive" job into a NEW document (F23). */
export const startReshape = (docId: number, spec: ReshapeSpec, expectedRevision: number) =>
  invoke<IndexedOpenStart>("start_reshape", { docId, spec, expectedRevision });

/** Preview a group-by — creates nothing (F22). */
export const previewGroupBy = (docId: number, spec: GroupBySpec, expectedRevision: number) =>
  invoke<GroupByPreview>("preview_group_by", { docId, spec, expectedRevision });

/** Run a group-by as a cancellable "derive" job into a NEW document (F22). */
export const startGroupBy = (docId: number, spec: GroupBySpec, expectedRevision: number) =>
  invoke<IndexedOpenStart>("start_group_by", { docId, spec, expectedRevision });

/** Cardinality preview of a join — creates nothing (F21). */
export const previewJoin = (
  leftDoc: number,
  rightDoc: number,
  spec: JoinSpec,
  leftRevision: number,
  rightRevision: number,
) => invoke<JoinPreview>("preview_join", { leftDoc, rightDoc, spec, leftRevision, rightRevision });

/** Run a join as a cancellable "derive" job into a NEW document (F21). */
export const startJoin = (
  leftDoc: number,
  rightDoc: number,
  spec: JoinSpec,
  leftRevision: number,
  rightRevision: number,
) =>
  invoke<IndexedOpenStart>("start_join", {
    leftDoc,
    rightDoc,
    spec,
    leftRevision,
    rightRevision,
  });

/** The last completed outlier report + the spec that produced it (F30). */
export const getOutlierReport = (docId: number) =>
  invoke<[OutlierSpec, OutlierReport] | null>("get_outlier_report", { docId });

/** Run an outlier scan as a cancellable job (F30). Read-only. */
export const startOutlierScan = (docId: number, spec: OutlierSpec, expectedRevision: number) =>
  invoke<number>("start_outlier_scan", { docId, spec, expectedRevision });

/** Filter the grid to the rows holding flagged values (F30). */
export const applyOutlierFilter = (docId: number, spec: OutlierSpec, expectedRevision: number) =>
  invoke<DocumentMeta>("apply_outlier_filter", { docId, spec, expectedRevision });

/** Preview a corrective outlier action (F30). */
export const previewOutlierAction = (
  docId: number,
  spec: OutlierSpec,
  action: OutlierAction,
  expectedRevision: number,
) =>
  invoke<OutlierActionPreview>("preview_outlier_action", { docId, spec, action, expectedRevision });

/** Apply a previewed corrective outlier action as ONE undo step (F30). */
export const applyOutlierAction = (
  docId: number,
  spec: OutlierSpec,
  action: OutlierAction,
  expectedRevision: number,
) => invoke<DocumentMeta>("apply_outlier_action", { docId, spec, action, expectedRevision });

/** Preview exactly what a missing-value repair would do (F29). */
export const previewRepair = (docId: number, spec: RepairSpec, expectedRevision: number) =>
  invoke<RepairPreview>("preview_repair", { docId, spec, expectedRevision });

/** Apply a previewed repair as ONE undo step (F29). */
export const applyRepair = (docId: number, spec: RepairSpec, expectedRevision: number) =>
  invoke<DocumentMeta>("apply_repair", { docId, spec, expectedRevision });

/** The last completed cross-validation report + the rules it ran (F27). */
export const getCrossvalReport = (docId: number) =>
  invoke<[CrossRule[], CrossValReport] | null>("get_crossval_report", { docId });

/** Run cross-column rules as a cancellable job (F27). */
export const startCrossvalScan = (docId: number, rules: CrossRule[], expectedRevision: number) =>
  invoke<number>("start_crossval_scan", { docId, rules, expectedRevision });

/** Filter to rows violating one rule (index) or any rule (null) (F27). */
export const applyCrossvalFilter = (
  docId: number,
  rules: CrossRule[],
  rule: number | null,
  expectedRevision: number,
) =>
  invoke<DocumentMeta>("apply_crossval_filter", {
    docId,
    rules,
    rule,
    expectedRevision,
  });

/**
 * Open a file in indexed read-only mode (F10). The document registers under
 * the returned docId when the job finishes.
 */
export const startOpenIndexed = (path: string, options?: OpenOptions) =>
  invoke<IndexedOpenStart>("start_open_indexed", { path, options });

/** Materialise an indexed document into an editable one (job id). */
export const startConvertToEditable = (docId: number, force: boolean) =>
  invoke<number>("start_convert_to_editable", { docId, force });

/** Rebuild an indexed document's index from its file (reload path; job id). */
export const startReindex = (docId: number) => invoke<number>("start_reindex", { docId });

/**
 * Parse the document's file with new settings and describe the outcome
 * without touching the open document.
 */
export const previewReparse = (docId: number, options: OpenOptions, maxRows: number) =>
  invoke<ReparsePreview>("preview_reparse", { docId, options, maxRows });

/**
 * Replace the open document by re-reading its file with new settings.
 * Rejected when the document changed since the preview's revision.
 */
export const applyReparse = (docId: number, options: OpenOptions, expectedRevision: number) =>
  invoke<DocumentMeta>("apply_reparse", { docId, options, expectedRevision });

/** The stored fingerprint of the document's backing file, if any. */
export const getFileFingerprint = (docId: number) =>
  invoke<FileFingerprint | null>("get_file_fingerprint", { docId });

/** Compare the stored source fingerprint with the file on disk. */
export const checkExternalChange = (docId: number) =>
  invoke<ExternalChange>("check_external_change", { docId });

export const newDocument = (rows?: number, cols?: number) =>
  invoke<DocumentMeta>("new_document", { rows, cols });

export const closeDocument = (docId: number) => invoke<void>("close_document", { docId });

export const getMeta = (docId: number) => invoke<DocumentMeta>("get_meta", { docId });

export const listEncodings = () => invoke<string[]>("list_encodings");

export const takePendingFiles = () => invoke<string[]>("take_pending_files");

/** Request cooperative cancellation of a running background job. */
export const cancelJob = (jobId: number) => invoke<boolean>("cancel_job", { jobId });

/** The last completed diagnostics report for a document, if any. */
export const getDiagnostics = (docId: number) =>
  invoke<DiagnosticsReport | null>("get_diagnostics", { docId });

/**
 * Start a background diagnostics scan; resolves with the job id. Progress and
 * completion arrive over the job events; fetch the report with
 * {@link getDiagnostics} once finished.
 */
export const startDiagnosticsScan = (docId: number, expectedRevision: number) =>
  invoke<number>("start_diagnostics_scan", { docId, expectedRevision });

/** Filter the grid to the rows affected by a row-filterable diagnostic. */
export const applyDiagnosticFilter = (docId: number, issueId: string, expectedRevision: number) =>
  invoke<DocumentMeta>("apply_diagnostic_filter", { docId, issueId, expectedRevision });

export const getRows = (docId: number, start: number, count: number) =>
  invoke<RowsResponse>("get_rows", { docId, start, count });

export const selectionStats = (docId: number, rect: CellRect) =>
  invoke<SelectionStats>("selection_stats", { docId, rect });

export const columnSummaries = (docId: number) =>
  invoke<ColumnSummary[]>("column_summaries", { docId });

export const setCell = (docId: number, row: number, col: number, value: string) =>
  invoke<DocumentMeta>("set_cell", { docId, row, col, value });

export const setCells = (docId: number, changes: [number, number, string][]) =>
  invoke<DocumentMeta>("set_cells", { docId, changes });

export const paste = (docId: number, anchorRow: number, anchorCol: number, block: string[][]) =>
  invoke<DocumentMeta>("paste", { docId, anchorRow, anchorCol, block });

export const insertRows = (docId: number, at: number, count: number) =>
  invoke<DocumentMeta>("insert_rows", { docId, at, count });

export const deleteRows = (docId: number, indices: number[]) =>
  invoke<DocumentMeta>("delete_rows", { docId, indices });

export const moveRow = (docId: number, from: number, to: number) =>
  invoke<DocumentMeta>("move_row", { docId, from, to });

export const insertColumn = (docId: number, at: number, name: string) =>
  invoke<DocumentMeta>("insert_column", { docId, at, name });

export const deleteColumns = (docId: number, indices: number[]) =>
  invoke<DocumentMeta>("delete_columns", { docId, indices });

export const renameColumn = (docId: number, col: number, name: string) =>
  invoke<DocumentMeta>("rename_column", { docId, col, name });

export const moveColumn = (docId: number, from: number, to: number) =>
  invoke<DocumentMeta>("move_column", { docId, from, to });

export const sort = (docId: number, keys: SortKey[]) =>
  invoke<DocumentMeta>("sort", { docId, keys });

export const setHeaderMode = (docId: number, hasHeader: boolean) =>
  invoke<DocumentMeta>("set_header_mode", { docId, hasHeader });

export const setFilter = (docId: number, spec: FilterGroup) =>
  invoke<DocumentMeta>("set_filter", { docId, spec });

export const clearFilter = (docId: number) => invoke<DocumentMeta>("clear_filter", { docId });

export const find = (docId: number, options: FindOptions) =>
  invoke<FindMatch[]>("find", { docId, options });

export const replaceAll = (docId: number, options: FindOptions, replacement: string) =>
  invoke<ReplaceResult>("replace_all", { docId, options, replacement });

export const undo = (docId: number) => invoke<DocumentMeta>("undo", { docId });

export const redo = (docId: number) => invoke<DocumentMeta>("redo", { docId });

/**
 * Scan for characters the target encoding cannot represent, optionally
 * limited to the slice an export will actually write.
 */
export const checkEncodingCompatibility = (
  docId: number,
  encoding: string,
  scope?: ExportScope,
  includeHeaders?: boolean,
) =>
  invoke<EncodingCompatibility>("check_encoding_compatibility", {
    docId,
    encoding,
    scope,
    includeHeaders,
  });

/** The row/column counts a scoped export would write. */
export const exportScopeCounts = (docId: number, scope: ExportScope) =>
  invoke<ScopeCounts>("export_scope_counts", { docId, scope });

/**
 * Start an atomic streaming save of the complete document; resolves with the
 * job id. Completion (and refreshed metadata) arrives via the job events +
 * getMeta.
 */
export const startSave = (
  docId: number,
  path: string,
  options: ExportOptions,
  expectedRevision: number,
) => invoke<number>("start_save", { docId, path, options, expectedRevision });

/** Start a read-only comparison of two open documents (F09). */
export const startCompare = (
  leftDocId: number,
  rightDocId: number,
  spec: CompareSpec,
  expectedLeftRevision: number,
  expectedRightRevision: number,
) =>
  invoke<number>("start_compare", {
    leftDocId,
    rightDocId,
    spec,
    expectedLeftRevision,
    expectedRightRevision,
  });

/** Summary + identity of a stored comparison (F09). */
export const getCompareInfo = (compareId: number) =>
  invoke<CompareInfo | null>("get_compare_info", { compareId });

/** One page of hydrated compare results, optionally status-filtered (F09). */
export const getCompareResults = (
  compareId: number,
  offset: number,
  count: number,
  statuses?: DiffStatus[],
) => invoke<ComparePage>("get_compare_results", { compareId, offset, count, statuses });

/** Export added/removed/changed rows or the JSON change report (F09). */
export const startCompareExport = (
  compareId: number,
  which: DiffStatus | "report",
  path: string,
  options: ExportOptions,
) => invoke<number>("start_compare_export", { compareId, which, path, options });

/** The last completed duplicate report, if any (F07). */
export const getDuplicateReport = (docId: number) =>
  invoke<DuplicateReport | null>("get_duplicate_report", { docId });

/** Start a background duplicate scan; resolves with the job id (F07). */
export const startDuplicateScan = (
  docId: number,
  spec: DedupSpec,
  scope: ExportScope,
  expectedRevision: number,
) => invoke<number>("start_duplicate_scan", { docId, spec, scope, expectedRevision });

/** Filter the grid to every row belonging to a duplicate group (F07). */
export const applyDuplicateFilter = (
  docId: number,
  spec: DedupSpec,
  scope: ExportScope,
  expectedRevision: number,
) => invoke<DocumentMeta>("apply_duplicate_filter", { docId, spec, scope, expectedRevision });

/** Remove duplicate rows (one undo step); resolves with the job id (F07). */
export const applyDeduplicate = (
  docId: number,
  spec: DedupSpec,
  scope: ExportScope,
  keepStrategy: DuplicateKeepStrategy,
  expectedRevision: number,
) => invoke<number>("apply_deduplicate", { docId, spec, scope, keepStrategy, expectedRevision });

/** Compute a transform's full effect without mutating anything (F06). */
export const previewTransform = (
  docId: number,
  spec: TransformSpec,
  scope: ExportScope,
  expectedRevision: number,
) => invoke<TransformPreview>("preview_transform", { docId, spec, scope, expectedRevision });

/**
 * Apply a previewed transform as one undoable operation (F06); resolves with
 * the job id (cancellable before commit via cancelJob).
 */
export const applyTransform = (
  docId: number,
  spec: TransformSpec,
  scope: ExportScope,
  policy: TransformErrorPolicy,
  expectedRevision: number,
) => invoke<number>("apply_transform", { docId, spec, scope, policy, expectedRevision });

/** A still-valid cached column profile, if one exists (F05). */
export const getColumnProfile = (docId: number, column: number, scope: ProfileScope) =>
  invoke<ColumnProfile | null>("get_column_profile", { docId, column, scope });

/** Start a background column-profile scan; resolves with the job id (F05). */
export const startColumnProfile = (
  docId: number,
  column: number,
  scope: ProfileScope,
  expectedRevision: number,
) =>
  invoke<number>("start_column_profile", {
    docId,
    column,
    scope,
    options: null,
    expectedRevision,
  });

/** Load persisted profiles + preferences (safe defaults on corruption). */
export const getSettings = () => invoke<AppSettings>("get_settings");

/** Persist profiles + preferences atomically. */
export const setSettings = (settings: AppSettings) => invoke<void>("set_settings", { settings });

/** Check a document against a profile's column and data rules (read-only). */
export const validateProfile = (docId: number, profile: FileProfile) =>
  invoke<ProfileValidation>("validate_profile", { docId, profile });

/**
 * Start a scoped, optionally split, atomic streaming export (never touches
 * the document's save point, path, or fingerprint).
 */
export const startExport = (
  docId: number,
  path: string,
  options: ExportOptions,
  scope: ExportScope,
  split: SplitOptions,
  writeManifest: boolean,
  expectedRevision: number,
) =>
  invoke<number>("start_export", {
    docId,
    path,
    options,
    scope,
    split,
    writeManifest,
    expectedRevision,
  });
