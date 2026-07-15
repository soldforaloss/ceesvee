// Thin, typed wrappers around the Rust command surface. Keeping every `invoke`
// in one place gives the rest of the app a clean, discoverable API and a single
// spot to evolve the contract.

import { invoke } from "@tauri-apps/api/core";
import type {
  CellRect,
  ColumnSummary,
  DocumentMeta,
  ExportOptions,
  FilterGroup,
  FindMatch,
  FindOptions,
  OpenOptions,
  ReplaceResult,
  RowsResponse,
  SelectionStats,
  SortKey,
} from "../types";

export const openFile = (path: string, options?: OpenOptions) =>
  invoke<DocumentMeta>("open_file", { path, options });

export const reparse = (docId: number, options: OpenOptions) =>
  invoke<DocumentMeta>("reparse", { docId, options });

export const newDocument = (rows?: number, cols?: number) =>
  invoke<DocumentMeta>("new_document", { rows, cols });

export const closeDocument = (docId: number) => invoke<void>("close_document", { docId });

export const getMeta = (docId: number) => invoke<DocumentMeta>("get_meta", { docId });

export const listEncodings = () => invoke<string[]>("list_encodings");

export const takePendingFiles = () => invoke<string[]>("take_pending_files");

/** Request cooperative cancellation of a running background job. */
export const cancelJob = (jobId: number) => invoke<boolean>("cancel_job", { jobId });

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

export const save = (docId: number, path: string, options: ExportOptions) =>
  invoke<DocumentMeta>("save", { docId, path, options });
