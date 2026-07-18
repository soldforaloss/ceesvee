// Thin, typed wrappers around the Rust command surface. Keeping every `invoke`
// in one place gives the rest of the app a clean, discoverable API and a single
// spot to evolve the contract.

import { invoke } from "@tauri-apps/api/core";
import type {
  AnnotationExportFormat,
  AnnotationPredicate,
  AnnotationsExport,
  AnnotationsView,
  KeySpec,
  RematchReport,
  RowMarkPatch,
  TagDef,
  TagToColumnPreview,
  TagToColumnTarget,
  AppendInput,
  AppendOptions,
  AppendPreview,
  AppendReport,
  AppSettings,
  ArchiveExtractStart,
  BatchOptions,
  BatchReport,
  CellRect,
  ChangeSummary,
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
  CsvDialectOptions,
  DialectPreview,
  PastePreview,
  PasteSpecialOptions,
  PiiReport,
  PiiSpec,
  RedactionAction,
  RedactionPreview,
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
  JsonExportOptions,
  JsonImportOptions,
  JsonImportPreview,
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
  RecoverableSession,
  RepairPreview,
  RepairSpec,
  ReparsePreview,
  ReplaceResult,
  ReshapePreview,
  ReshapeSpec,
  RowsResponse,
  SamplePreview,
  SampleRequest,
  SampleStart,
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
  CellEditValidation,
  ColumnSchema,
  ConvertPreview,
  DocumentSchema,
  InvalidSampleReport,
  SchemaImportOutcome,
  SchemaInfo,
  SchemaIssue,
  ProjectMeta,
  ProjectStateDto,
  ProjectOpenPreview,
  ProjectOpenPlan,
  ResolutionEntry,
  DictionaryField,
  DictionaryFormat,
  DictionaryImportOutcome,
  DictionaryView,
  MergeMatchBy,
  MergePlan,
  MergeResolution,
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

/** Open a plain file in READ-ONLY follow mode (F19). */
export const startFollow = (path: string) => invoke<DocumentMeta>("start_follow", { path });

/** Pause/resume a follow watcher (view updates only; no bytes lost) (F19). */
export const setFollowPaused = (docId: number, paused: boolean) =>
  invoke<void>("set_follow_paused", { docId, paused });

/** Stop following; the tab stays open as a read-only snapshot (F19). */
export const stopFollow = (docId: number) => invoke<void>("stop_follow", { docId });

/** Filter to rows from `fromRow` onward (F19: newly added rows) (F19). */
export const setRowRangeFilter = (docId: number, fromRow: number) =>
  invoke<DocumentMeta>("set_row_range_filter", { docId, fromRow });

/** Parse the document's file under a full dialect, without applying (F18). */
export const previewDialect = (docId: number, dialect: CsvDialectOptions) =>
  invoke<DialectPreview>("preview_dialect", { docId, dialect });

/** Reinterpret the document under the previewed dialect (guarded) (F18). */
export const applyDialect = (docId: number, dialect: CsvDialectOptions, expectedRevision: number) =>
  invoke<DocumentMeta>("apply_dialect", { docId, dialect, expectedRevision });

/** Recoverable sessions found at startup (expired journals swept) (F16). */
export const listRecoverySessions = () => invoke<RecoverableSession[]>("list_recovery_sessions");

/** Recover a journaled session (never writes the source) (F16). */
export const recoverSession = (journalPath: string, openCopy: boolean) =>
  invoke<DocumentMeta>("recover_session", { journalPath, openCopy });

/** Discard one recovery session (deletes its journal) (F16). */
export const discardRecoverySession = (journalPath: string) =>
  invoke<void>("discard_recovery_session", { journalPath });

/** Delete ALL recovery data (F16). */
export const deleteAllRecovery = () => invoke<number>("delete_all_recovery");

/** Every unsaved operation, oldest first, with cell samples (F15). */
export const getChanges = (docId: number) =>
  invoke<{ savedInRedo: boolean; changes: ChangeSummary[] }>("get_changes", { docId });

/** Revert one whole operation (a NEW, undoable operation) (F15). */
export const revertChange = (docId: number, opId: number, expectedRevision: number) =>
  invoke<DocumentMeta>("revert_change", { docId, opId, expectedRevision });

/** Revert specific cells of one cell-edit operation (F15). */
export const revertChangeCells = (
  docId: number,
  opId: number,
  cells: [number, number][],
  expectedRevision: number,
) => invoke<DocumentMeta>("revert_change_cells", { docId, opId, cells, expectedRevision });

/** Revert every unsaved edit in one column (F15). */
export const revertColumnChanges = (docId: number, col: number, expectedRevision: number) =>
  invoke<DocumentMeta>("revert_column_changes", { docId, col, expectedRevision });

/** Revert everything since the last save as one undoable operation (F15). */
export const revertAllChanges = (docId: number, expectedRevision: number) =>
  invoke<DocumentMeta>("revert_all_changes", { docId, expectedRevision });

/** The last completed PII report + the spec that produced it (F28). */
export const getPiiReport = (docId: number) =>
  invoke<[PiiSpec, PiiReport] | null>("get_pii_report", { docId });

/** Run a PII scan as a cancellable job — samples are masked (F28). */
export const startPiiScan = (docId: number, spec: PiiSpec, expectedRevision: number) =>
  invoke<number>("start_pii_scan", { docId, spec, expectedRevision });

/** Preview a redaction: counts + MASKED before/after examples (F28). */
export const previewRedaction = (
  docId: number,
  spec: PiiSpec,
  detector: number,
  column: number,
  action: RedactionAction,
  expectedRevision: number,
) =>
  invoke<RedactionPreview>("preview_redaction", {
    docId,
    spec,
    detector,
    column,
    action,
    expectedRevision,
  });

/** Apply a previewed redaction as ONE undo step (audited locally) (F28). */
export const applyRedaction = (
  docId: number,
  spec: PiiSpec,
  detector: number,
  column: number,
  action: RedactionAction,
  expectedRevision: number,
) =>
  invoke<DocumentMeta>("apply_redaction", {
    docId,
    spec,
    detector,
    column,
    action,
    expectedRevision,
  });

/** Validate a batch (recipe version, steps, templates) without running (F25). */
export const validateRecipeBatch = (options: BatchOptions) =>
  invoke<void>("validate_recipe_batch", { options });

/** Run a batch recipe as a cancellable job; report lands under the job id (F25). */
export const startRecipeBatch = (options: BatchOptions) =>
  invoke<number>("start_recipe_batch", { options });

/** The structured report of a finished batch (F25). */
export const getBatchReport = (jobId: number) =>
  invoke<BatchReport | null>("get_batch_report", { jobId });

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

/**
 * Preview a sampling/partitioning run (F48): resolves the seed, sizes the
 * scope, and reports projected + exact per-output counts (plus a strata table
 * and warnings). Read-only and revision-guarded; nothing is created.
 */
export const previewSample = (docId: number, request: SampleRequest, expectedRevision: number) =>
  invoke<SamplePreview>("preview_sample", { docId, request, expectedRevision });

/**
 * Run a sampling/partitioning operation as a cancellable job (F48). `seed` is
 * the value surfaced by `previewSample` — pass it back for reproducibility.
 * Outputs become NEW derived documents (the returned `docIds`) or CSV files.
 */
export const startSample = (
  docId: number,
  request: SampleRequest,
  seed: number,
  expectedRevision: number,
) => invoke<SampleStart>("start_sample", { docId, request, seed, expectedRevision });

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

// ----- explicit schemas and typed columns (F31) ----------------------------

/** The document's explicit schema, names refreshed from headers by ID (F31). */
export const getSchema = (docId: number) => invoke<SchemaInfo>("get_schema", { docId });

/**
 * Start a schema-inference scan over every column as a cancellable job (F31).
 * Read-only; nothing is assigned. Returns the job id; fetch the inferred
 * schema with `takeInferredSchema` once the job finishes.
 */
export const startInferSchema = (docId: number, expectedRevision: number) =>
  invoke<number>("start_infer_schema", { docId, expectedRevision });

/** The completed inference result for a finished `startInferSchema` job (F31). */
export const takeInferredSchema = (docId: number) =>
  invoke<DocumentSchema | null>("take_inferred_schema", { docId });

/** Assign or replace ONE column's schema (metadata: never dirties) (F31). */
export const setColumnSchema = (docId: number, schema: ColumnSchema) =>
  invoke<SchemaInfo>("set_column_schema", { docId, schema });

/** Drop one column's schema entry, back to implicit text (F31). */
export const removeColumnSchema = (docId: number, columnId: string) =>
  invoke<SchemaInfo>("remove_column_schema", { docId, columnId });

/** Export the schema as versioned JSON (atomic write) (F31). */
export const exportSchema = (docId: number, path: string) =>
  invoke<void>("export_schema", { docId, path });

/** Import a versioned schema JSON file, REPLACING the schema (F31). */
export const importSchema = (docId: number, path: string) =>
  invoke<SchemaImportOutcome>("import_schema", { docId, path });

/** Pure pre-check: how the declared schema judges a proposed value (F31). */
export const validateCellEdit = (docId: number, col: number, value: string) =>
  invoke<CellEditValidation>("validate_cell_edit", { docId, col, value });

/** The advisory-validation issues recorded on the document (F31). */
export const getSchemaIssues = (docId: number) =>
  invoke<SchemaIssue[]>("get_schema_issues", { docId });

/** Clear the recorded advisory-validation issues (F31). */
export const clearSchemaIssues = (docId: number) => invoke<void>("clear_schema_issues", { docId });

/**
 * Start a bounded invalid-value scan of one column as a cancellable job (F31).
 * Returns the job id; fetch the report with `takeSchemaInvalidSamples`.
 */
export const startSchemaInvalidSamples = (
  docId: number,
  columnId: string,
  maxSamples: number,
  expectedRevision: number,
) =>
  invoke<number>("start_schema_invalid_samples", {
    docId,
    columnId,
    maxSamples,
    expectedRevision,
  });

/** The report for a finished `startSchemaInvalidSamples` job (F31). */
export const takeSchemaInvalidSamples = (docId: number) =>
  invoke<InvalidSampleReport | null>("take_schema_invalid_samples", { docId });

/**
 * Start a conversion preview of one column (no mutation) as a cancellable
 * job (F31). Returns the job id; fetch the preview with
 * `takeConvertColumnPreview`.
 */
export const startConvertColumnPreview = (
  docId: number,
  columnId: string,
  maxSamples: number,
  expectedRevision: number,
) =>
  invoke<number>("start_convert_column_preview", {
    docId,
    columnId,
    maxSamples,
    expectedRevision,
  });

/** The preview for a finished `startConvertColumnPreview` job (F31). */
export const takeConvertColumnPreview = (docId: number) =>
  invoke<ConvertPreview | null>("take_convert_column_preview", { docId });

/**
 * Apply a previewed conversion as ONE undoable job (F31). Guarded against both
 * the data revision AND the schema revision the preview was computed under, so
 * a schema edit between preview and apply is rejected.
 */
export const convertColumnApply = (
  docId: number,
  columnId: string,
  expectedRevision: number,
  expectedSchemaRevision: number,
) =>
  invoke<number>("convert_column_apply", {
    docId,
    columnId,
    expectedRevision,
    expectedSchemaRevision,
  });

// ----- data dictionary (F38) ------------------------------------------------

/** The dictionary editor surface: one row per current column plus orphans (F38). */
export const getDictionary = (docId: number) => invoke<DictionaryView>("get_dictionary", { docId });

/**
 * Insert or replace one column's documentation (F38). Metadata only: never
 * dirties the document. An all-blank entry is removed rather than stored.
 * Guarded by the dictionary revision.
 */
export const setDictionaryField = (
  docId: number,
  field: DictionaryField,
  expectedDictionaryRevision: number,
) =>
  invoke<DictionaryView>("set_dictionary_field", {
    docId,
    field,
    expectedDictionaryRevision,
  });

/** Drop one column's documentation entry (clear a column, or an orphan) (F38). */
export const removeDictionaryField = (
  docId: number,
  columnId: string,
  expectedDictionaryRevision: number,
) =>
  invoke<DictionaryView>("remove_dictionary_field", {
    docId,
    columnId,
    expectedDictionaryRevision,
  });

/** Discard EVERY orphaned entry (documentation whose column is gone) (F38). */
export const discardDictionaryOrphans = (docId: number, expectedDictionaryRevision: number) =>
  invoke<DictionaryView>("discard_dictionary_orphans", {
    docId,
    expectedDictionaryRevision,
  });

/** Export the dictionary as JSON / Markdown / CSV (atomic write) (F38). */
export const exportDictionary = (docId: number, path: string, format: DictionaryFormat) =>
  invoke<void>("export_dictionary", { docId, path, format });

/**
 * Plan a dictionary import (F38): parse the CEESVEE dictionary JSON at `path`,
 * match its entries to current columns, and return the merge plan (clean
 * additions + the field-level conflicts that must be resolved). Read-only.
 */
export const previewDictionaryImport = (docId: number, path: string, matchBy: MergeMatchBy) =>
  invoke<MergePlan>("preview_dictionary_import", { docId, path, matchBy });

/**
 * Apply a dictionary import under an explicit conflict resolution (F38). Fails
 * (changing nothing) if any reported conflict is left unresolved, or if the
 * dictionary moved since the plan was taken. Metadata only.
 */
export const applyDictionaryImport = (
  docId: number,
  path: string,
  matchBy: MergeMatchBy,
  resolution: MergeResolution,
  expectedDictionaryRevision: number,
) =>
  invoke<DictionaryImportOutcome>("apply_dictionary_import", {
    docId,
    path,
    matchBy,
    resolution,
    expectedDictionaryRevision,
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

/**
 * Absolute source record numbers for a DISPLAY-row window (F40). Lets the grid
 * place per-row annotation indicators on the right rows under any sort/filter,
 * where display row != record. `null` for a display index past the end.
 */
export const displayRecords = (docId: number, start: number, count: number) =>
  invoke<(number | null)[]>("display_records", { docId, start, count });

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

/** F12: set (or clear, with empty keys) the non-destructive view sort. */
export const setViewSort = (docId: number, keys: SortKey[]) =>
  invoke<DocumentMeta>("set_view_sort", { docId, keys });

/** F12: drop BOTH row-view ingredients (filter and view sort) in one step. */
export const resetRowView = (docId: number) => invoke<DocumentMeta>("reset_row_view", { docId });

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

// ----- JSON / JSON Lines interoperability (F33) ----------------------------

/**
 * Start a full-pass JSON import preview scan as a cancellable job (F33).
 * Resolves with the job id; fetch the preview with {@link getJsonImportPreview}
 * once the `job-finished` event arrives. Nothing is created.
 */
export const jsonImportPreview = (path: string, options?: JsonImportOptions) =>
  invoke<number>("json_import_preview", { path, options });

/** The preview of a finished JSON import scan, by its job id (F33). */
export const getJsonImportPreview = (jobId: number) =>
  invoke<JsonImportPreview | null>("get_json_import_preview", { jobId });

/**
 * Run a JSON / JSON Lines import as a cancellable "derive" job (F33): the NEW
 * document registers under the returned docId when the job finishes, through
 * the same pipeline as every other producer. Invalid input never leaves a
 * partial document.
 */
export const jsonImportApply = (path: string, options?: JsonImportOptions) =>
  invoke<IndexedOpenStart>("json_import_apply", { path, options });

/**
 * Start a scoped JSON / JSON Lines export as a cancellable "export" job (F33).
 * Options validate, the scope resolves and duplicate output paths are rejected
 * BEFORE the job spawns (the invoke rejects), then re-checked inside it.
 */
export const jsonExport = (
  docId: number,
  path: string,
  options: JsonExportOptions,
  scope: ExportScope,
  expectedRevision: number,
) => invoke<number>("json_export", { docId, path, options, scope, expectedRevision });

// ----- project workspaces (F37) ---------------------------------------------
// The ProjectStore is THE persistence boundary: typed, versioned sections are
// written through `project_set_section` and flushed atomically by `project_save`.

/** Create a new (unsaved) project, optionally from a template, replacing any
 * open one. The caller confirms discarding unsaved changes first. */
export const projectNew = (templatePath?: string) =>
  invoke<ProjectMeta>("project_new", { templatePath: templatePath ?? null });

/** The current project (meta + all sections), or null. */
export const projectGet = () => invoke<ProjectStateDto | null>("project_get");

/** Replace one registered section; dirties the project only when it changed. */
export const projectSetSection = (name: string, value: unknown) =>
  invoke<ProjectMeta>("project_set_section", { name, value });

/** Save the project to its existing path (atomic). */
export const projectSave = () => invoke<ProjectMeta>("project_save");

/** Save the project to a new path (atomic) and adopt it. */
export const projectSaveAs = (path: string) => invoke<ProjectMeta>("project_save_as", { path });

/** Export the open project as a reusable template (config only, no sources). */
export const projectSaveTemplate = (path: string) =>
  invoke<void>("project_save_template", { path });

/** Close the project (documents stay open). Caller confirms unsaved changes. */
export const projectClose = () => invoke<void>("project_close");

/** Preview opening a project file: per-source statuses and gating verdicts. */
export const projectOpenPreview = (path: string) =>
  invoke<ProjectOpenPreview>("project_open_preview", { path });

/** Apply a project open with per-source resolutions, replacing any open one. */
export const projectOpenApply = (path: string, resolutions: ResolutionEntry[]) =>
  invoke<ProjectOpenPlan>("project_open_apply", { path, resolutions });

// ----- row bookmarks, tags & notes (F40) -----------------------------------
// Annotations live in a doc_id-keyed registry OUTSIDE the document, so they
// survive reparse/reindex/convert (the id is stable) and never dirty the doc.
// Every mutation is guarded by the store's own `annotationsRevision` and returns
// the fresh view. The front end calls {@link annotationsRematch} after a reload.

/** The annotations panel surface, rematched against the current document. */
export const annotationsView = (docId: number) =>
  invoke<AnnotationsView>("annotations_view", { docId });

/** Re-resolve every annotation against the current document; returns the
 * matched tally plus the ambiguous / orphaned review list. Call after any
 * reparse / reindex / external-change reload. */
export const annotationsRematch = (docId: number) =>
  invoke<RematchReport>("annotations_rematch", { docId });

/** Set (or clear, with `null`) the key columns anchoring NEW annotations, then
 * re-anchor the matched existing ones to the new mechanism. */
export const annotationsSetKeySpec = (
  docId: number,
  keySpec: KeySpec | null,
  expectedAnnotationsRevision: number,
) =>
  invoke<AnnotationsView>("annotations_set_key_spec", {
    docId,
    keySpec,
    expectedAnnotationsRevision,
  });

/** Set (or clear, with `null`) the default author label carried on new notes. */
export const annotationsSetAuthor = (
  docId: number,
  author: string | null,
  expectedAnnotationsRevision: number,
) =>
  invoke<AnnotationsView>("annotations_set_author", {
    docId,
    author,
    expectedAnnotationsRevision,
  });

/** Star / flag / add or remove tags on the row at `displayRow`. Creates the
 * annotation if absent; prunes it if the edit leaves it empty. */
export const annotationsEditRow = (
  docId: number,
  displayRow: number,
  patch: RowMarkPatch,
  expectedAnnotationsRevision: number,
) =>
  invoke<AnnotationsView>("annotations_edit_row", {
    docId,
    displayRow,
    patch,
    expectedAnnotationsRevision,
  });

/** Set (or clear, with `text = null`) the ROW note on `displayRow`. */
export const annotationsSetRowNote = (
  docId: number,
  displayRow: number,
  text: string | null,
  author: string | null,
  expectedAnnotationsRevision: number,
) =>
  invoke<AnnotationsView>("annotations_set_row_note", {
    docId,
    displayRow,
    text,
    author,
    expectedAnnotationsRevision,
  });

/** Set (or clear, with `text = null`) a CELL note on `columnId` of `displayRow`. */
export const annotationsSetCellNote = (
  docId: number,
  displayRow: number,
  columnId: string,
  text: string | null,
  author: string | null,
  expectedAnnotationsRevision: number,
) =>
  invoke<AnnotationsView>("annotations_set_cell_note", {
    docId,
    displayRow,
    columnId,
    text,
    author,
    expectedAnnotationsRevision,
  });

/** Delete one whole annotation entry by its stable handle (e.g. discarding a
 * single orphan from the review list). */
export const annotationsRemoveRow = (
  docId: number,
  handle: number,
  expectedAnnotationsRevision: number,
) =>
  invoke<AnnotationsView>("annotations_remove_row", {
    docId,
    handle,
    expectedAnnotationsRevision,
  });

/** Discard every orphaned annotation (no matching row in the current document). */
export const annotationsDiscardOrphans = (docId: number, expectedAnnotationsRevision: number) =>
  invoke<AnnotationsView>("annotations_discard_orphans", { docId, expectedAnnotationsRevision });

/** Define or update a tag in the per-document namespace. */
export const annotationsDefineTag = (
  docId: number,
  tag: TagDef,
  expectedAnnotationsRevision: number,
) => invoke<AnnotationsView>("annotations_define_tag", { docId, tag, expectedAnnotationsRevision });

/** Remove a tag from the namespace and from every row that carries it. */
export const annotationsRemoveTag = (
  docId: number,
  name: string,
  expectedAnnotationsRevision: number,
) =>
  invoke<AnnotationsView>("annotations_remove_tag", { docId, name, expectedAnnotationsRevision });

/** Filter the grid to the rows matching an annotation-state predicate, via the
 * existing row-filter view. Only MATCHED rows contribute. Guarded by the
 * document revision; returns the fresh document meta. */
export const applyAnnotationFilter = (
  docId: number,
  predicate: AnnotationPredicate,
  expectedRevision: number,
) => invoke<DocumentMeta>("apply_annotation_filter", { docId, predicate, expectedRevision });

/** Preview copying a tag into a column (rows affected, what is skipped as
 * ambiguous / orphaned, a bounded sample). Read-only. */
export const previewTagToColumn = (docId: number, tag: string) =>
  invoke<TagToColumnPreview>("preview_tag_to_column", { docId, tag });

/** Copy a tag into a real column as ONE undoable document operation. Guarded by
 * the document revision; the notes themselves are untouched. */
export const applyTagToColumn = (
  docId: number,
  tag: string,
  target: TagToColumnTarget,
  expectedRevision: number,
) => invoke<DocumentMeta>("apply_tag_to_column", { docId, tag, target, expectedRevision });

/** Export the annotations as versioned JSON or flat CSV (atomic write). An
 * EXPLICIT action — notes never leave through an ordinary data export. */
export const exportAnnotations = (docId: number, path: string, format: AnnotationExportFormat) =>
  invoke<void>("export_annotations", { docId, path, format });

/** The full annotations export envelope — what the front end writes into the
 * project's `annotations` section, or a sidecar. */
export const annotationsGetExport = (docId: number) =>
  invoke<AnnotationsExport>("annotations_get_export", { docId });

/** Hydrate a document's annotation store from an export envelope (from the
 * project section on project open, say). Replaces any current store. */
export const annotationsLoadExport = (docId: number, exportEnvelope: AnnotationsExport) =>
  invoke<AnnotationsView>("annotations_load_export", { docId, export: exportEnvelope });

/** Load a document's annotations from its sidecar file, replacing any current
 * store. An absent sidecar yields an empty store. Used when no project is open. */
export const annotationsLoadSidecar = (docId: number, sourcePath: string) =>
  invoke<AnnotationsView>("annotations_load_sidecar", { docId, sourcePath });

/** Save a document's annotations to its sidecar file (atomic). An empty store
 * deletes the sidecar. */
export const annotationsSaveSidecar = (docId: number, sourcePath: string) =>
  invoke<void>("annotations_save_sidecar", { docId, sourcePath });
