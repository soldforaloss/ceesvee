// Every CEESVEE command, registered once into the shared registry (F11).
// Commands read and drive the Zustand store directly; components render
// state, the registry owns behaviour.

import { parseCellRef } from "./cellRef";
import { registry, type AppCommand } from "./commands";
import { IS_MAC } from "./shortcuts";
import { checkForUpdates } from "./updater";
import { useStore, type ModalName } from "../store/useStore";

const NO_DOC = "No document is open";
const READ_ONLY = "The document is read-only (indexed) — convert it to editable first";

function state() {
  return useStore.getState();
}

function needsDoc(): string | null {
  return state().activeId == null ? NO_DOC : null;
}

function needsEditable(): string | null {
  const s = state();
  if (s.activeId == null) return NO_DOC;
  const meta = s.tabs.find((t) => t.id === s.activeId);
  return meta?.backing === "indexedReadOnly" ? READ_ONLY : null;
}

/** Annotation row-ops act on the selection; they never touch source data, so a
 * read-only (indexed) document is fine — only a non-empty selection is needed. */
function needsRows(): string | null {
  const s = state();
  if (s.activeId == null) return NO_DOC;
  return s.selectedRows.length === 0 ? "Select one or more rows first" : null;
}

function activeMeta() {
  const s = state();
  return s.tabs.find((t) => t.id === s.activeId) ?? null;
}

function openModal(modal: ModalName): void {
  state().setModal(modal);
}

function staticCommands(): AppCommand[] {
  return [
    // ----- File ------------------------------------------------------------
    {
      id: "file.new",
      title: "New document",
      keywords: ["create", "blank"],
      category: "File",
      defaultShortcut: "mod+n",
      allowInEditable: true,
      run: () => void state().newDoc(),
    },
    {
      id: "file.open",
      title: "Open file…",
      keywords: ["browse", "load", "csv"],
      category: "File",
      defaultShortcut: "mod+o",
      allowInEditable: true,
      run: () => void state().openDialog(),
    },
    {
      id: "file.openJson",
      title: "Open JSON…",
      keywords: ["json", "jsonl", "ndjson", "import", "nested", "records"],
      category: "File",
      allowInEditable: true,
      run: () => void state().openJsonDialog(),
    },
    {
      id: "file.openColumnar",
      title: "Open Parquet/Arrow…",
      keywords: ["parquet", "arrow", "feather", "ipc", "columnar", "import", "typed"],
      category: "File",
      allowInEditable: true,
      run: () => void state().openColumnarDialog(),
    },
    {
      id: "file.openExcel",
      title: "Open Excel…",
      keywords: ["xlsx", "excel", "workbook", "spreadsheet", "sheet", "import", "table"],
      category: "File",
      allowInEditable: true,
      run: () => void state().openExcelDialog(),
    },
    {
      id: "file.openDatabase",
      title: "Open database…",
      keywords: ["sqlite", "db", "database", "table", "sql", "browse", "import"],
      category: "File",
      allowInEditable: true,
      run: () => void state().openDatabaseDialog(),
    },
    {
      id: "file.exportDatabase",
      title: "Export to database…",
      keywords: ["sqlite", "db", "database", "table", "sql", "write", "create", "append"],
      category: "Export",
      unavailableReason: needsDoc,
      run: () => void state().openDbExport(),
    },
    {
      id: "file.save",
      title: "Save",
      keywords: ["write", "persist"],
      category: "File",
      defaultShortcut: "mod+s",
      allowInEditable: true,
      unavailableReason: needsEditable,
      run: () => void state().saveActive(false),
    },
    {
      id: "file.saveAs",
      title: "Save As…",
      category: "File",
      defaultShortcut: "mod+shift+s",
      allowInEditable: true,
      unavailableReason: needsEditable,
      run: () => void state().saveActive(true),
    },
    {
      id: "file.export",
      title: "Export…",
      keywords: ["scoped", "split", "csv", "download"],
      category: "Export",
      defaultShortcut: "mod+e",
      unavailableReason: needsDoc,
      run: () => openModal("export"),
    },
    {
      id: "file.exportJson",
      title: "Export as JSON…",
      keywords: ["json", "jsonl", "ndjson", "objects", "arrays", "nested", "rebuild"],
      category: "Export",
      unavailableReason: needsDoc,
      run: () => openModal("jsonExport"),
    },
    {
      id: "file.exportColumnar",
      title: "Export as Parquet/Arrow…",
      keywords: ["parquet", "arrow", "feather", "ipc", "columnar", "typed", "snappy", "zstd"],
      category: "Export",
      unavailableReason: needsDoc,
      run: () => openModal("columnarExport"),
    },
    {
      id: "file.exportExcel",
      title: "Export to Excel…",
      keywords: ["xlsx", "excel", "workbook", "spreadsheet", "sheets", "tabs", "styling"],
      category: "Export",
      unavailableReason: needsDoc,
      run: () => openModal("excelExport"),
    },
    {
      id: "file.closeTab",
      title: "Close tab",
      category: "Tabs",
      defaultShortcut: "mod+w",
      allowInEditable: true,
      unavailableReason: needsDoc,
      run: () => {
        const id = state().activeId;
        if (id != null) void state().closeTab(id);
      },
    },
    {
      id: "file.convertToEditable",
      title: "Convert to editable",
      keywords: ["indexed", "read-only", "materialize"],
      category: "File",
      unavailableReason: () => {
        const meta = activeMeta();
        if (!meta) return NO_DOC;
        return meta.backing === "indexedReadOnly" ? null : "The document is already editable";
      },
      run: () => void state().convertActiveToEditable(false),
    },

    // ----- Project (F37) ---------------------------------------------------
    {
      id: "project.new",
      title: "New project",
      keywords: ["workspace", "ceesveeproj", "project"],
      category: "Project",
      allowInEditable: true,
      run: () => void state().projectNew(),
    },
    {
      id: "project.newFromTemplate",
      title: "New project from template…",
      keywords: ["workspace", "template", "scaffold", "repeatable"],
      category: "Project",
      allowInEditable: true,
      run: () => void state().projectNewFromTemplate(),
    },
    {
      id: "project.open",
      title: "Open project…",
      keywords: ["workspace", "ceesveeproj", "load project"],
      category: "Project",
      allowInEditable: true,
      run: () => void state().projectPickAndOpen(),
    },
    {
      id: "project.save",
      title: "Save project",
      keywords: ["workspace", "persist project"],
      category: "Project",
      allowInEditable: true,
      unavailableReason: () => (state().project ? null : "No project is open"),
      run: () => void state().projectSave(false),
    },
    {
      id: "project.saveAs",
      title: "Save project as…",
      keywords: ["workspace", "copy project"],
      category: "Project",
      allowInEditable: true,
      unavailableReason: () => (state().project ? null : "No project is open"),
      run: () => void state().projectSave(true),
    },
    {
      id: "project.saveTemplate",
      title: "Save project as template…",
      keywords: ["workspace", "template", "reusable", "configuration"],
      category: "Project",
      allowInEditable: true,
      unavailableReason: () => (state().project ? null : "No project is open"),
      run: () => void state().projectSaveTemplate(),
    },
    {
      id: "project.close",
      title: "Close project",
      keywords: ["workspace", "end project"],
      category: "Project",
      allowInEditable: true,
      unavailableReason: () => (state().project ? null : "No project is open"),
      run: () => state().requestCloseProject(),
    },

    // ----- Edit ------------------------------------------------------------
    {
      id: "edit.undo",
      title: "Undo",
      category: "Edit",
      defaultShortcut: "mod+z",
      unavailableReason: () => {
        const meta = activeMeta();
        if (!meta) return NO_DOC;
        return meta.canUndo ? null : "Nothing to undo";
      },
      run: () => void state().undo(),
    },
    {
      id: "edit.redo",
      title: "Redo",
      category: "Edit",
      defaultShortcut: "mod+y",
      unavailableReason: () => {
        const meta = activeMeta();
        if (!meta) return NO_DOC;
        return meta.canRedo ? null : "Nothing to redo";
      },
      run: () => void state().redo(),
    },
    {
      // Shortcut alias only: keeps the long-standing Ctrl/Cmd+Shift+Z redo
      // chord working alongside mod+y. Hidden from the palette so Redo is
      // listed once; the binding stays independently rebindable.
      id: "edit.redoAlt",
      title: "Redo",
      category: "Edit",
      defaultShortcut: "mod+shift+z",
      hidden: true,
      unavailableReason: () => {
        const meta = activeMeta();
        if (!meta) return NO_DOC;
        return meta.canRedo ? null : "Nothing to redo";
      },
      run: () => void state().redo(),
    },
    {
      id: "edit.editCell",
      title: "Edit cell (multiline)…",
      keywords: ["multiline", "raw", "inspect", "newline", "escaped", "invisible"],
      category: "Edit",
      defaultShortcut: "f2",
      extraShortcuts: ["mod+enter"],
      unavailableReason: () => {
        const reason = needsDoc();
        if (reason) return reason;
        return state().selectionRect ? null : "No cell is selected";
      },
      run: () => {
        const rect = state().selectionRect;
        // The rect is display-space; the editor targets the PHYSICAL column
        // (F12 layouts can hide/reorder columns).
        if (rect) state().openCellEditor(rect.y, state().displayColToPhysical(rect.x));
      },
    },
    {
      id: "edit.find",
      title: "Find & replace",
      keywords: ["search", "regex", "replace"],
      category: "Edit",
      defaultShortcut: "mod+f",
      allowInEditable: true,
      unavailableReason: needsDoc,
      run: () => state().setFindOpen(true),
    },
    {
      id: "edit.copyAs",
      title: "Copy As…",
      keywords: ["clipboard", "json", "markdown", "sql", "tsv", "serialize"],
      category: "Edit",
      defaultShortcut: "mod+shift+c",
      unavailableReason: needsDoc,
      run: () => openModal("copyAs"),
    },
    {
      id: "edit.pasteSpecial",
      title: "Paste Special…",
      keywords: ["clipboard", "transpose", "skip blanks", "insert rows", "structured"],
      category: "Edit",
      defaultShortcut: "mod+shift+v",
      unavailableReason: needsEditable,
      run: () => openModal("pasteSpecial"),
    },
    {
      id: "edit.insertRow",
      title: "Insert row",
      keywords: ["add", "new row"],
      category: "Edit",
      unavailableReason: needsEditable,
      run: () => {
        const s = state();
        const meta = activeMeta();
        const at = s.selectedRows.length ? Math.max(...s.selectedRows) + 1 : (meta?.rowCount ?? 0);
        void s.insertRows(at, 1);
      },
    },
    {
      id: "edit.deleteRows",
      title: "Delete selected rows",
      keywords: ["remove"],
      category: "Edit",
      unavailableReason: () => {
        const editable = needsEditable();
        if (editable) return editable;
        return state().selectedRows.length ? null : "No rows are selected";
      },
      run: () => {
        const s = state();
        if (s.selectedRows.length) void s.deleteRows(s.selectedRows);
      },
    },
    {
      id: "edit.addColumn",
      title: "Add column",
      keywords: ["insert", "new column"],
      category: "Edit",
      unavailableReason: needsEditable,
      run: () => {
        const meta = activeMeta();
        if (meta) void state().insertColumn(meta.colCount, `Column ${meta.colCount + 1}`);
      },
    },

    // ----- Data ------------------------------------------------------------
    {
      id: "data.sort",
      title: "Sort…",
      keywords: ["order", "ascending", "descending", "view sort", "non-destructive"],
      category: "Data",
      // The dialog offers a non-destructive VIEW sort (F12), which works on
      // read-only documents too; only the destructive apply needs editable.
      unavailableReason: needsDoc,
      run: () => openModal("sort"),
    },
    {
      id: "data.filter",
      title: "Filter rows…",
      keywords: ["query", "where", "conditions"],
      category: "Data",
      defaultShortcut: "mod+shift+f",
      unavailableReason: needsDoc,
      run: () => openModal("filter"),
    },
    {
      id: "data.clearFilter",
      title: "Clear filter",
      keywords: ["remove filter", "show all rows"],
      category: "Data",
      unavailableReason: () => {
        const meta = activeMeta();
        if (!meta) return NO_DOC;
        return meta.filtered ? null : "No filter is active";
      },
      run: () => void state().clearFilter(),
    },
    {
      id: "data.transform",
      title: "Clean data…",
      keywords: ["transform", "trim", "case", "split", "merge", "normalize"],
      category: "Data",
      unavailableReason: needsEditable,
      run: () => openModal("transform"),
    },
    {
      id: "data.dedup",
      title: "Find duplicates…",
      keywords: ["deduplicate", "unique", "duplicate rows"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("dedup"),
    },
    {
      id: "data.cluster",
      title: "Cluster values…",
      keywords: ["fuzzy", "normalize", "variants", "misspelling", "merge values", "openrefine"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("cluster"),
    },
    {
      id: "data.semantic",
      title: "Semantic types…",
      keywords: ["detect", "email", "url", "uuid", "ip", "phone", "column types", "validate"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("semantic"),
    },
    {
      id: "data.schema",
      title: "Edit schema…",
      keywords: [
        "logical type",
        "typed columns",
        "integer",
        "decimal",
        "date",
        "strict",
        "nullable",
      ],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => state().openSchemaDialog(),
    },
    {
      id: "data.inferSchema",
      title: "Infer schema",
      keywords: ["detect types", "guess", "logical schema", "auto"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => void state().inferAndApplySchema(),
    },
    {
      id: "data.schemaIo",
      title: "Import/Export schema…",
      keywords: ["schema json", "versioned", "load schema", "save schema", "typed columns"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => state().openSchemaDialog(),
    },
    {
      id: "data.dictionary",
      title: "Data dictionary…",
      keywords: [
        "document columns",
        "descriptions",
        "metadata",
        "glossary",
        "owner",
        "sensitivity",
        "unit",
        "meaning",
        "catalog",
      ],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => state().openDictionaryDialog(),
    },
    {
      id: "data.dictionaryImport",
      title: "Import/Export data dictionary…",
      keywords: ["dictionary json", "markdown", "csv documentation", "merge metadata", "versioned"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => state().openDictionaryDialog(),
    },
    // ----- Annotate (F40) --------------------------------------------------
    {
      id: "annotate.panel",
      title: "Annotations panel",
      keywords: ["bookmarks", "tags", "notes", "flags", "stars", "review", "orphans"],
      category: "Annotate",
      unavailableReason: needsDoc,
      run: () => {
        const s = state();
        s.setAnnotationsPanelOpen(!s.annotationsPanelOpen);
      },
    },
    {
      id: "annotate.star",
      title: "Star selected rows",
      keywords: ["bookmark", "mark", "favourite", "favorite"],
      category: "Annotate",
      unavailableReason: needsRows,
      run: () => void state().applyRowMarks(state().selectedRows, { star: true }),
    },
    {
      id: "annotate.unstar",
      title: "Unstar selected rows",
      keywords: ["bookmark", "clear", "remove"],
      category: "Annotate",
      unavailableReason: needsRows,
      run: () => void state().applyRowMarks(state().selectedRows, { star: false }),
    },
    {
      id: "annotate.flag",
      title: "Flag selected rows",
      keywords: ["mark", "attention", "review"],
      category: "Annotate",
      unavailableReason: needsRows,
      run: () => void state().applyRowMarks(state().selectedRows, { flag: true }),
    },
    {
      id: "annotate.unflag",
      title: "Unflag selected rows",
      keywords: ["clear", "remove"],
      category: "Annotate",
      unavailableReason: needsRows,
      run: () => void state().applyRowMarks(state().selectedRows, { flag: false }),
    },
    {
      id: "annotate.tag",
      title: "Tag selected rows…",
      keywords: ["label", "categorise", "categorize", "tags"],
      category: "Annotate",
      unavailableReason: needsRows,
      run: () => state().openTagPicker(state().selectedRows),
    },
    {
      id: "annotate.filterStarred",
      title: "Filter: starred rows",
      keywords: ["bookmarks", "show only", "annotation"],
      category: "Annotate",
      unavailableReason: needsDoc,
      run: () => void state().applyAnnotationFilter({ type: "starred" }),
    },
    {
      id: "annotate.filterFlagged",
      title: "Filter: flagged rows",
      keywords: ["show only", "annotation"],
      category: "Annotate",
      unavailableReason: needsDoc,
      run: () => void state().applyAnnotationFilter({ type: "flagged" }),
    },
    {
      id: "annotate.filterAnnotated",
      title: "Filter: annotated rows",
      keywords: ["show only", "has note", "any annotation"],
      category: "Annotate",
      unavailableReason: needsDoc,
      run: () => void state().applyAnnotationFilter({ type: "anyAnnotation" }),
    },
    {
      id: "annotate.export",
      title: "Export annotations…",
      keywords: ["json", "csv", "notes", "tags", "backup"],
      category: "Annotate",
      unavailableReason: needsDoc,
      run: () => openModal("annotationExport"),
    },
    {
      id: "file.follow",
      title: "Open in follow mode…",
      keywords: ["tail", "watch", "log", "live", "growing", "monitor"],
      category: "File",
      unavailableReason: () => null,
      run: () => {
        void (async () => {
          const { open } = await import("@tauri-apps/plugin-dialog");
          const chosen = await open({
            filters: [
              { name: "Delimited text", extensions: ["csv", "tsv", "tab", "txt", "psv", "dat"] },
            ],
          });
          if (typeof chosen === "string") {
            await state().startFollowFile(chosen);
          }
        })();
      },
    },
    {
      id: "file.dialect",
      title: "Advanced import…",
      keywords: ["dialect", "preamble", "comment", "quote", "escape", "header rows", "skip"],
      category: "File",
      unavailableReason: needsEditable,
      run: () => openModal("dialect"),
    },
    {
      id: "file.recovery",
      title: "Recover unsaved work…",
      keywords: ["crash", "journal", "restore", "recovery"],
      category: "File",
      unavailableReason: () => null,
      run: () => openModal("recovery"),
    },
    {
      id: "view.changes",
      title: "Changes since save",
      keywords: ["inspector", "revert", "history", "dirty", "diff", "unsaved"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => {
        const s = state();
        s.setChangesOpen(!s.changesOpen);
      },
    },
    {
      id: "data.pii",
      title: "Find personal data…",
      keywords: ["pii", "redact", "mask", "sensitive", "gdpr", "anonymize", "pseudonymize"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("pii"),
    },
    {
      id: "data.recipes",
      title: "Batch process files…",
      keywords: ["recipe", "folder", "automation", "pipeline", "bulk", "many files"],
      category: "Data",
      unavailableReason: () => null,
      run: () => openModal("recipes"),
    },
    {
      id: "data.append",
      title: "Append files…",
      keywords: ["concatenate", "combine", "merge files", "stack", "union"],
      category: "Data",
      unavailableReason: () => null,
      run: () => openModal("append"),
    },
    {
      id: "data.outlier",
      title: "Find outliers…",
      keywords: ["anomaly", "iqr", "mad", "z-score", "suspicious", "statistics"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("outlier"),
    },
    {
      id: "data.repair",
      title: "Repair missing values…",
      keywords: ["fill", "blank", "null", "interpolate", "mean", "median", "mode", "ffill"],
      category: "Data",
      unavailableReason: needsEditable,
      run: () => openModal("repair"),
    },
    {
      id: "data.crossval",
      title: "Validate across columns…",
      keywords: ["rules", "cross column", "relationship", "consistency", "sum", "conditional"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("crossval"),
    },
    {
      id: "data.compare",
      title: "Compare…",
      keywords: ["diff", "changes", "versus"],
      category: "Data",
      unavailableReason: () => {
        const reason = needsDoc();
        if (reason) return reason;
        return state().tabs.length >= 2 ? null : "Comparing needs a second open document";
      },
      run: () => openModal("compare"),
    },
    {
      id: "data.reshape",
      title: "Pivot / unpivot / transpose…",
      keywords: ["reshape", "wide", "long", "melt", "crosstab", "rotate"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("reshape"),
    },
    {
      id: "data.groupBy",
      title: "Group by…",
      keywords: ["aggregate", "summarize", "sum", "count", "pivot table", "rollup"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("groupBy"),
    },
    {
      id: "data.sample",
      title: "Sample rows…",
      keywords: [
        "subset",
        "random",
        "reservoir",
        "stratified",
        "systematic",
        "hash",
        "seed",
        "downsample",
      ],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => state().openSamplingDialog("sampling"),
    },
    {
      id: "data.partition",
      title: "Partition dataset…",
      keywords: [
        "split",
        "train",
        "validation",
        "test",
        "holdout",
        "fold",
        "weighted",
        "group-preserving",
      ],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => state().openSamplingDialog("partitioning"),
    },
    {
      id: "data.join",
      title: "Join…",
      keywords: ["lookup", "merge", "vlookup", "relational", "inner", "outer"],
      category: "Data",
      unavailableReason: () => {
        const reason = needsDoc();
        if (reason) return reason;
        return state().tabs.length >= 2 ? null : "Joining needs a second open document";
      },
      run: () => openModal("join"),
    },
    {
      id: "data.summaries",
      title: "Column summaries",
      keywords: ["statistics", "types", "overview"],
      category: "Data",
      unavailableReason: needsDoc,
      run: () => openModal("summaries"),
    },
    {
      id: "data.profiles",
      title: "File profiles…",
      keywords: ["validation", "rules", "schema"],
      category: "Data",
      run: () => openModal("profiles"),
    },

    // ----- View ------------------------------------------------------------
    {
      id: "view.palette",
      title: "Command palette",
      keywords: ["commands", "actions", "search"],
      category: "View",
      defaultShortcut: "mod+k",
      allowInEditable: true,
      run: () => state().setPaletteOpen(true),
    },
    {
      id: "view.shortcuts",
      title: "Keyboard shortcuts…",
      keywords: ["keybindings", "hotkeys", "customize"],
      category: "View",
      run: () => openModal("shortcuts"),
    },
    {
      id: "view.diagnostics",
      title: "Toggle diagnostics panel",
      keywords: ["issues", "data quality", "fidelity"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => state().setDiagnosticsOpen(!state().diagnosticsOpen),
    },
    {
      id: "view.explorer",
      title: "Toggle column explorer",
      keywords: ["profile", "histogram", "distribution"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => state().setExplorerOpen(!state().explorer.open),
    },
    {
      id: "view.facets",
      title: "Toggle facets",
      keywords: [
        "facet",
        "faceted search",
        "explore",
        "cross-filter",
        "dimensions",
        "drill down",
        "slice",
      ],
      category: "View",
      unavailableReason: needsDoc,
      run: () => state().setFacetsOpen(!state().facets.open),
    },
    {
      id: "view.facetsToFilter",
      title: "Convert facets to filter",
      keywords: ["facet", "filter", "convert", "build filter"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => void state().convertFacetsToFilter(),
    },
    {
      id: "view.recordForm",
      title: "Toggle record form",
      keywords: ["record", "form", "single", "row", "wide", "field", "edit one"],
      category: "View",
      defaultShortcut: "mod+shift+r",
      unavailableReason: needsDoc,
      run: () => state().setRecordFormOpen(!state().recordFormOpen),
    },
    {
      id: "view.theme",
      title: "Toggle theme",
      keywords: ["dark", "light", "appearance"],
      category: "View",
      allowInEditable: true,
      run: () => state().setTheme(state().theme === "dark" ? "light" : "dark"),
    },
    {
      id: "view.manageViews",
      title: "Named views…",
      keywords: ["view", "layout", "save view", "column layout", "perspective"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => openModal("views"),
    },
    {
      id: "view.highlight",
      title: "Conditional highlighting…",
      keywords: [
        "highlight",
        "rules",
        "colour",
        "color",
        "decorate",
        "flag cells",
        "conditional formatting",
        "emphasis",
      ],
      category: "View",
      unavailableReason: needsDoc,
      run: () => openModal("highlight"),
    },
    {
      id: "view.saveViewAs",
      title: "Save current view as…",
      keywords: ["named view", "layout", "snapshot"],
      category: "View",
      unavailableReason: needsDoc,
      argPlaceholder: "View name",
      run: () => state().openPaletteForArg("view.saveViewAs"),
      runWith: (arg: string) => {
        if (!arg.trim()) return "Enter a name for the view";
        void state().saveCurrentViewAs(arg);
        return null;
      },
    },
    {
      id: "view.reset",
      title: "Reset view",
      keywords: ["clear layout", "unhide", "unsort", "default view"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => void state().resetView(),
    },
    {
      id: "view.autoFitAll",
      title: "Auto-fit all column widths",
      keywords: ["resize", "fit", "widths"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => state().requestAutoFit("all"),
    },
    {
      id: "view.autoFitSelected",
      title: "Auto-fit selected columns",
      keywords: ["resize", "fit", "widths"],
      category: "View",
      unavailableReason: () => {
        const gate = needsDoc();
        if (gate) return gate;
        return state().selectedCols.length === 0 ? "No columns are selected" : null;
      },
      run: () => state().requestAutoFit(state().selectedCols),
    },
    {
      id: "view.toggleWrap",
      title: "Toggle wrap text",
      keywords: ["wrap", "multiline", "row height"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => state().setWrapText(!state().wrapText),
    },
    {
      id: "view.hideSelectedColumns",
      title: "Hide selected columns",
      keywords: ["hide", "column", "layout"],
      category: "View",
      unavailableReason: () => {
        const gate = needsDoc();
        if (gate) return gate;
        return state().selectedCols.length === 0 ? "No columns are selected" : null;
      },
      run: () => {
        const s = state();
        for (const col of s.selectedCols) s.setColumnHidden(col, true);
      },
    },
    {
      id: "view.unhideAll",
      title: "Unhide all columns",
      keywords: ["show", "hidden", "columns"],
      category: "View",
      unavailableReason: needsDoc,
      run: () => state().unhideAllColumns(),
    },

    // ----- Navigate ---------------------------------------------------------
    {
      id: "nav.goToRow",
      title: "Go to row…",
      keywords: ["jump", "line"],
      category: "Navigate",
      defaultShortcut: "mod+g",
      unavailableReason: needsDoc,
      argPlaceholder: "Row number (1-based)",
      run: () => state().openPaletteForArg("nav.goToRow"),
      runWith: (arg) => {
        const ref = parseCellRef(arg);
        if (!ref) return "Enter a row number, e.g. 120";
        void state().jumpToCell(ref.row, 0);
        return null;
      },
    },
    {
      id: "nav.goToCell",
      title: "Go to cell…",
      keywords: ["jump", "reference", "a1"],
      category: "Navigate",
      unavailableReason: needsDoc,
      argPlaceholder: "Cell (C42 or row,column)",
      run: () => state().openPaletteForArg("nav.goToCell"),
      runWith: (arg) => {
        const ref = parseCellRef(arg);
        if (!ref) return "Enter a cell like C42 or 42,3";
        void state().jumpToCell(ref.row, ref.col);
        return null;
      },
    },
    {
      id: "nav.nextTab",
      title: "Next tab",
      category: "Tabs",
      // Physical Ctrl+Tab everywhere: on Windows/Linux `bindingFromEvent`
      // reports the primary modifier as "mod", so the default must use the
      // form key events actually emit; macOS keeps the literal "ctrl" chord
      // (Cmd+Tab belongs to the OS app switcher).
      defaultShortcut: IS_MAC ? "ctrl+tab" : "mod+tab",
      allowInEditable: true,
      unavailableReason: () => (state().tabs.length > 1 ? null : "Only one tab is open"),
      run: () => cycleTab(1),
    },
    {
      id: "nav.prevTab",
      title: "Previous tab",
      category: "Tabs",
      defaultShortcut: IS_MAC ? "ctrl+shift+tab" : "mod+shift+tab",
      allowInEditable: true,
      unavailableReason: () => (state().tabs.length > 1 ? null : "Only one tab is open"),
      run: () => cycleTab(-1),
    },

    // ----- Help ------------------------------------------------------------
    {
      id: "help.checkUpdates",
      title: "Check for updates",
      keywords: ["upgrade", "version", "release"],
      category: "Help",
      allowInEditable: true,
      run: () => void checkForUpdates({ silent: false }),
    },
  ];
}

function cycleTab(delta: number): void {
  const s = state();
  if (s.tabs.length < 2 || s.activeId == null) return;
  const index = s.tabs.findIndex((t) => t.id === s.activeId);
  const next = s.tabs[(index + delta + s.tabs.length) % s.tabs.length];
  s.setActive(next.id);
}

/** Dynamic entries: open tabs, recent files and named views (F12). */
function dynamicCommands(): AppCommand[] {
  const s = state();
  const tabs: AppCommand[] = s.tabs
    .filter((t) => t.id !== s.activeId)
    .map((t) => ({
      id: `dynamic.tab.${t.id}`,
      title: `Switch to tab: ${t.fileName}`,
      keywords: ["tab", "document"],
      category: "Tabs" as const,
      run: () => s.setActive(t.id),
    }));
  const views: AppCommand[] =
    s.activeId != null
      ? s.viewsForActive().views.map((view) => ({
          id: `dynamic.view.${view.id}`,
          title: `Apply view: ${view.name}`,
          keywords: ["named view", "layout", view.name],
          category: "View" as const,
          run: () => void s.applyNamedView(view),
        }))
      : [];
  const recents: AppCommand[] = s.recent.slice(0, 10).map((path, i) => ({
    id: `dynamic.recent.${i}`,
    title: `Open recent: ${path.split(/[\\/]/).pop() ?? path}`,
    keywords: ["recent", path],
    category: "File" as const,
    run: () => void s.openPath(path),
  }));
  return [...tabs, ...views, ...recents];
}

let registered = false;

/** Populate the shared registry (idempotent; App calls it once on mount). */
export function registerAppCommands(): void {
  if (registered) return;
  registered = true;
  registry.register(staticCommands());
  registry.addProvider(dynamicCommands);
}
