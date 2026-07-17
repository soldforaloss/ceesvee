import { useEffect, useMemo, useState } from "react";

import {
  LOGICAL_TYPES,
  LOGICAL_TYPE_LABELS,
  displayFormatOptions,
  isNumericType,
  isTemporalType,
} from "../lib/schema";
import { useActiveMeta, useStore } from "../store/useStore";
import type {
  ColumnSchema,
  ColumnStateCounts,
  ConvertPreview,
  InvalidSampleReport,
  LogicalType,
  ValidationMode,
} from "../types";
import { Modal } from "./Modal";

/**
 * Explicit schemas and typed columns (F31): a searchable, per-column editor
 * for the logical schema. Assigning a type never rewrites cell text; display
 * formatting is presentation only; schema edits never dirty the document.
 * From here the user can infer a schema, import/export versioned JSON, inspect
 * a column's five cell states and invalid samples, and run the canonical
 * conversion as ONE previewed, undoable operation.
 */
export function SchemaDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const schemaInfo = useStore((s) => s.schemaInfo);
  const schemaConvert = useStore((s) => s.schemaConvert);
  const schemaScan = useStore((s) => s.schemaScan);
  const focusColumn = useStore((s) => s.schemaDialogColumn);
  const loadSchema = useStore((s) => s.loadSchema);
  const setColumnSchema = useStore((s) => s.setColumnSchema);
  const removeColumnSchema = useStore((s) => s.removeColumnSchema);
  const inferAndApply = useStore((s) => s.inferAndApplySchema);
  const importSchema = useStore((s) => s.importSchemaFromFile);
  const exportSchema = useStore((s) => s.exportSchemaToFile);
  const runInvalidSamples = useStore((s) => s.runSchemaInvalidSamples);
  const runConvertPreview = useStore((s) => s.runSchemaConvertPreview);
  const cancelScan = useStore((s) => s.cancelSchemaScan);
  const applyConversion = useStore((s) => s.applyColumnConversion);
  const cancelConversion = useStore((s) => s.cancelColumnConversion);

  const colCount = meta?.colCount ?? 0;
  const [selected, setSelected] = useState<number>(() =>
    focusColumn != null && focusColumn < colCount ? focusColumn : 0,
  );
  const [search, setSearch] = useState("");
  const [draft, setDraft] = useState<ColumnSchema | null>(null);
  const [tokenInput, setTokenInput] = useState("");
  const [formatInput, setFormatInput] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  const [invalidReport, setInvalidReport] = useState<InvalidSampleReport | null>(null);
  const [preview, setPreview] = useState<ConvertPreview | null>(null);

  const schemaColumns = schemaInfo?.schema.columns ?? null;
  const readOnly = meta?.backing === "indexedReadOnly";
  // A cancellable scan (infer / invalid-values / preview) is in flight when it
  // targets nothing (whole-document infer) or this selected column.
  const scanning = schemaScan !== null;

  useEffect(() => {
    void loadSchema();
  }, [loadSchema]);

  const selectedId = meta ? meta.columnIds[selected] : undefined;
  const storedSelected = selectedId ? schemaColumns?.[selectedId] : undefined;
  const storedKey = storedSelected ? JSON.stringify(storedSelected) : "";

  // Seed the editable draft when the selected column — or its stored schema —
  // changes. Typing does not touch the stored schema, so the draft survives
  // edits until the user applies or switches columns.
  useEffect(() => {
    if (!meta || selected >= meta.colCount) {
      setDraft(null);
      return;
    }
    const id = meta.columnIds[selected];
    const stored = id ? schemaColumns?.[id] : undefined;
    setDraft(
      stored
        ? { ...stored, nullTokens: [...stored.nullTokens] }
        : {
            columnId: id ?? "",
            name: meta.headers[selected] ?? "",
            logicalType: "text",
            nullable: true,
            nullTokens: [],
            validationMode: "advisory",
          },
    );
    setTokenInput("");
    setFormatInput("");
    setInvalidReport(null);
    setPreview(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected, selectedId, storedKey, meta?.id]);

  const filtered = useMemo(() => {
    if (!meta) return [] as number[];
    const q = search.trim().toLowerCase();
    const all = Array.from({ length: meta.colCount }, (_, i) => i);
    if (!q) return all;
    return all.filter((i) => (meta.headers[i] || `Column ${i + 1}`).toLowerCase().includes(q));
  }, [meta, search]);

  if (!meta) return null;

  const isDirty = draft !== null && JSON.stringify(draft) !== storedKey;
  const declared = storedSelected !== undefined;
  const converting = schemaConvert !== null && schemaConvert.columnId === selectedId;

  const patchDraft = (patch: Partial<ColumnSchema>) =>
    setDraft((d) => (d ? { ...d, ...patch } : d));

  const apply = async () => {
    if (!draft) return;
    setWorking(true);
    setNotice(null);
    const ok = await setColumnSchema(draft);
    setWorking(false);
    if (ok) setNotice(`Schema for "${draft.name}" saved.`);
  };

  const remove = async () => {
    if (!selectedId) return;
    setWorking(true);
    await removeColumnSchema(selectedId);
    setWorking(false);
    setNotice(null);
  };

  const runInfer = async () => {
    setWorking(true);
    setNotice(null);
    const ok = await inferAndApply();
    setWorking(false);
    if (ok) setNotice("Inferred a schema for every column from the data.");
  };

  const runImport = async () => {
    setNotice(null);
    const msg = await importSchema();
    if (msg) setNotice(msg);
  };

  const scanInvalid = async () => {
    if (!selectedId || !declared) return;
    setInvalidReport(null);
    // Runs as a cancellable job in the store; failures surface as a global
    // error, cancellation returns null silently.
    const report = await runInvalidSamples(selectedId, 500);
    if (report) setInvalidReport(report);
  };

  const runPreview = async () => {
    if (!selectedId || !declared) return;
    setPreview(null);
    const p = await runConvertPreview(selectedId, 20);
    if (p) setPreview(p);
  };

  const runConvert = async () => {
    if (!selectedId || !preview) return;
    // Hand back BOTH revisions the preview was computed against so a schema
    // edit (or a data edit) since then is rejected before any cell changes.
    const ok = await applyConversion(selectedId, preview.revision, preview.schemaRevision);
    if (ok) {
      setPreview(null);
      setInvalidReport(null);
      setNotice("Column converted to canonical form (one undo step).");
    }
  };

  const lt = draft?.logicalType ?? "text";
  const formatOptions = displayFormatOptions(lt);

  return (
    <Modal
      title="Schema — typed columns"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-xs text-zinc-400">
            Schema edits are metadata — they never change cell text or mark the document dirty.
          </span>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={() => void runInfer()}
            disabled={working || scanning}
            className={btnPrimary}
          >
            Infer schema
          </button>
          {scanning && schemaScan.kind === "infer" && (
            <span className="flex items-center gap-2 text-[11px] text-zinc-500">
              Inferring… {schemaScan.processed.toLocaleString()}
              {schemaScan.total != null && ` / ${schemaScan.total.toLocaleString()}`}
              <button onClick={() => void cancelScan()} className={btnDanger}>
                Cancel
              </button>
            </span>
          )}
          <button
            onClick={() => void runImport()}
            disabled={working || scanning}
            className={btnOutline}
          >
            Import…
          </button>
          <button
            onClick={() => void exportSchema()}
            disabled={
              working || scanning || !schemaColumns || Object.keys(schemaColumns).length === 0
            }
            className={btnOutline}
          >
            Export…
          </button>
          {readOnly && (
            <span className="rounded-full bg-amber-100 px-2 py-0.5 text-[11px] text-amber-800 dark:bg-amber-500/15 dark:text-amber-300">
              Read-only — types can be declared, but conversion is disabled
            </span>
          )}
        </div>
        {notice && (
          <p className="rounded bg-emerald-50 px-2 py-1.5 text-xs text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-300">
            {notice}
          </p>
        )}

        <div className="flex gap-3">
          {/* ----- column list ----- */}
          <div className="flex w-52 shrink-0 flex-col">
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Search columns…"
              className={`${inputCls} mb-2`}
            />
            <div className="max-h-[52vh] overflow-y-auto rounded border border-zinc-200 dark:border-zinc-800">
              {filtered.map((i) => {
                const id = meta.columnIds[i];
                const s = id ? schemaColumns?.[id] : undefined;
                return (
                  <button
                    key={i}
                    onClick={() => setSelected(i)}
                    className={`flex w-full items-center gap-1.5 px-2 py-1.5 text-left text-xs ${
                      i === selected
                        ? "bg-violet-100 dark:bg-violet-500/20"
                        : "hover:bg-zinc-100 dark:hover:bg-zinc-800"
                    }`}
                  >
                    <span className="min-w-0 flex-1 truncate" title={meta.headers[i]}>
                      {meta.headers[i] || `Column ${i + 1}`}
                    </span>
                    {s ? (
                      <span className="shrink-0 rounded bg-violet-200 px-1 py-0.5 text-[10px] text-violet-800 dark:bg-violet-500/25 dark:text-violet-200">
                        {LOGICAL_TYPE_LABELS[s.logicalType]}
                      </span>
                    ) : (
                      <span className="shrink-0 text-[10px] text-zinc-400">—</span>
                    )}
                  </button>
                );
              })}
              {filtered.length === 0 && (
                <p className="px-2 py-3 text-center text-xs text-zinc-400">No columns match.</p>
              )}
            </div>
          </div>

          {/* ----- editor ----- */}
          {draft && (
            <div className="min-w-0 flex-1 space-y-2.5">
              <div className="flex items-center justify-between">
                <h3 className="truncate font-medium" title={draft.name}>
                  {draft.name || `Column ${selected + 1}`}
                </h3>
                {declared && (
                  <button
                    onClick={() => void remove()}
                    disabled={working || scanning}
                    className={btnDanger}
                  >
                    Remove schema
                  </button>
                )}
              </div>

              <div className="grid grid-cols-2 gap-2.5">
                <label className={fieldLabel}>
                  Logical type
                  <select
                    value={draft.logicalType}
                    onChange={(e) => patchDraft({ logicalType: e.target.value as LogicalType })}
                    className={selectCls}
                  >
                    {LOGICAL_TYPES.map((t) => (
                      <option key={t} value={t} className="dark:bg-zinc-800">
                        {LOGICAL_TYPE_LABELS[t]}
                      </option>
                    ))}
                  </select>
                </label>

                <label className={fieldLabel}>
                  Edit validation
                  <select
                    value={draft.validationMode}
                    onChange={(e) =>
                      patchDraft({ validationMode: e.target.value as ValidationMode })
                    }
                    className={selectCls}
                  >
                    <option value="advisory" className="dark:bg-zinc-800">
                      Advisory (warn, record issue)
                    </option>
                    <option value="strict" className="dark:bg-zinc-800">
                      Strict (reject invalid edits)
                    </option>
                  </select>
                </label>
              </div>

              <label className="flex items-center gap-2 text-xs text-zinc-600 dark:text-zinc-300">
                <input
                  type="checkbox"
                  checked={draft.nullable}
                  onChange={(e) => patchDraft({ nullable: e.target.checked })}
                />
                Nullable (blanks and null tokens are allowed)
              </label>

              {/* null tokens */}
              <div>
                <span className={fieldLabelText}>Null tokens (mean “no value”)</span>
                <div className="mt-1 flex flex-wrap items-center gap-1">
                  {draft.nullTokens.map((tok, i) => (
                    <span key={i} className={chip}>
                      {tok === "" ? "(empty string)" : tok}
                      <button
                        onClick={() =>
                          patchDraft({ nullTokens: draft.nullTokens.filter((_, j) => j !== i) })
                        }
                        className="ml-1 text-zinc-400 hover:text-red-500"
                        aria-label="Remove token"
                      >
                        ×
                      </button>
                    </span>
                  ))}
                  <input
                    value={tokenInput}
                    onChange={(e) => setTokenInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        if (!draft.nullTokens.includes(tokenInput))
                          patchDraft({ nullTokens: [...draft.nullTokens, tokenInput] });
                        setTokenInput("");
                      }
                    }}
                    placeholder="add token, Enter"
                    className={`${inputCls} w-36`}
                  />
                  <button
                    onClick={() => {
                      if (draft.nullTokens.includes("")) return;
                      patchDraft({ nullTokens: [...draft.nullTokens, ""] });
                    }}
                    className={chipBtn}
                    title="Treat the empty string as an explicit null token"
                  >
                    + empty string
                  </button>
                </div>
              </div>

              {/* type-specific parsing options */}
              {isNumericType(lt) && (
                <label className={fieldLabel}>
                  Locale (number separators)
                  <input
                    value={draft.locale ?? ""}
                    onChange={(e) => patchDraft({ locale: e.target.value || undefined })}
                    placeholder="e.g. de-DE, fr-FR (blank = 1,234.5)"
                    className={inputCls}
                  />
                </label>
              )}

              {lt === "datetime" && (
                <label className={fieldLabel}>
                  Time zone (for naive datetimes)
                  <input
                    value={draft.timeZone ?? ""}
                    onChange={(e) => patchDraft({ timeZone: e.target.value || undefined })}
                    placeholder="IANA zone, e.g. Europe/Berlin"
                    className={inputCls}
                  />
                </label>
              )}

              {isTemporalType(lt) && (
                <div>
                  <span className={fieldLabelText}>
                    Input formats (strftime; blank = built-in + RFC 3339)
                  </span>
                  <div className="mt-1 flex flex-wrap items-center gap-1">
                    {(draft.inputFormats ?? []).map((fmt, i) => (
                      <span key={i} className={chip}>
                        <code>{fmt}</code>
                        <button
                          onClick={() =>
                            patchDraft({
                              inputFormats: (draft.inputFormats ?? []).filter((_, j) => j !== i),
                            })
                          }
                          className="ml-1 text-zinc-400 hover:text-red-500"
                          aria-label="Remove format"
                        >
                          ×
                        </button>
                      </span>
                    ))}
                    <input
                      value={formatInput}
                      onChange={(e) => setFormatInput(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          const f = formatInput.trim();
                          if (f) {
                            patchDraft({ inputFormats: [...(draft.inputFormats ?? []), f] });
                            setFormatInput("");
                          }
                        }
                      }}
                      placeholder="%Y-%m-%d, Enter"
                      className={`${inputCls} w-40`}
                    />
                  </div>
                </div>
              )}

              {formatOptions.length > 0 && (
                <label className={fieldLabel}>
                  Display format (presentation only)
                  <input
                    list="ceesvee-schema-formats"
                    value={draft.displayFormat ?? ""}
                    onChange={(e) => patchDraft({ displayFormat: e.target.value || undefined })}
                    placeholder="blank = raw text"
                    className={inputCls}
                  />
                  <datalist id="ceesvee-schema-formats">
                    {formatOptions.map((o) => (
                      <option key={o.value} value={o.value}>
                        {o.label}
                      </option>
                    ))}
                  </datalist>
                </label>
              )}

              <div className="flex items-center gap-2 pt-1">
                <button
                  onClick={() => void apply()}
                  disabled={working || scanning || !isDirty}
                  className={btnPrimary}
                >
                  {declared ? "Apply changes" : "Declare column"}
                </button>
                {isDirty && <span className="text-xs text-amber-500">Unsaved changes</span>}
              </div>

              {/* ----- five-state scan + invalid samples ----- */}
              <div className="mt-1 border-t border-zinc-200 pt-2.5 dark:border-zinc-800">
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => void scanInvalid()}
                    disabled={working || scanning || !declared || isDirty}
                    title={
                      !declared
                        ? "Declare a type first"
                        : isDirty
                          ? "Apply changes first"
                          : undefined
                    }
                    className={btnOutline}
                  >
                    Scan values
                  </button>
                  {!readOnly && (
                    <button
                      onClick={() => void runPreview()}
                      disabled={working || scanning || !declared || isDirty || converting}
                      title={
                        !declared
                          ? "Declare a type first"
                          : isDirty
                            ? "Apply changes first"
                            : undefined
                      }
                      className={btnOutline}
                    >
                      Preview conversion…
                    </button>
                  )}
                  {scanning && schemaScan.kind !== "infer" && (
                    <span className="flex items-center gap-2 text-[11px] text-zinc-500">
                      {schemaScan.kind === "preview" ? "Previewing…" : "Scanning…"}{" "}
                      {schemaScan.processed.toLocaleString()}
                      {schemaScan.total != null && ` / ${schemaScan.total.toLocaleString()}`}
                      <button onClick={() => void cancelScan()} className={btnDanger}>
                        Cancel
                      </button>
                    </span>
                  )}
                </div>

                {invalidReport && (
                  <div className="mt-2 space-y-1.5 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
                    <StateCounts counts={invalidReport.counts} />
                    {invalidReport.scannedRows < invalidReport.totalRows && (
                      <p className="text-[11px] text-sky-600 dark:text-sky-300">
                        Sampled the first {invalidReport.scannedRows.toLocaleString()} of{" "}
                        {invalidReport.totalRows.toLocaleString()} rows.
                      </p>
                    )}
                    {invalidReport.samples.length > 0 ? (
                      <ul className="max-h-40 space-y-0.5 overflow-y-auto font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                        {invalidReport.samples.map((s, i) => (
                          <li key={i} className="truncate">
                            row {s.row + 1}: “{s.value}” — {s.reason}
                          </li>
                        ))}
                      </ul>
                    ) : (
                      <p className="text-[11px] text-emerald-600 dark:text-emerald-300">
                        No invalid values under the declared type.
                      </p>
                    )}
                  </div>
                )}

                {preview && (
                  <div className="mt-2 space-y-1.5 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
                    <StateCounts counts={preview.counts} />
                    <p>
                      {preview.changed.toLocaleString()} valid cell
                      {preview.changed === 1 ? "" : "s"} would be rewritten to canonical form;{" "}
                      {preview.counts.invalid.toLocaleString()} invalid cell
                      {preview.counts.invalid === 1 ? "" : "s"} keep their text.
                    </p>
                    {preview.samples.length > 0 && (
                      <ul className="max-h-32 space-y-0.5 overflow-y-auto font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                        {preview.samples.map((s, i) => (
                          <li key={i} className="truncate">
                            {s.before} → {s.after}
                          </li>
                        ))}
                      </ul>
                    )}
                    <div className="flex items-center gap-2 pt-0.5">
                      {converting ? (
                        <>
                          <span className="text-[11px] text-zinc-500">
                            Converting… {schemaConvert.processed.toLocaleString()}
                            {schemaConvert.total != null &&
                              ` / ${schemaConvert.total.toLocaleString()}`}
                          </span>
                          <button onClick={() => void cancelConversion()} className={btnDanger}>
                            Cancel
                          </button>
                        </>
                      ) : (
                        <button
                          onClick={() => void runConvert()}
                          disabled={preview.changed === 0}
                          className={btnPrimary}
                        >
                          Convert (one undo step)
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      </div>
    </Modal>
  );
}

function StateCounts({ counts }: { counts: ColumnStateCounts }) {
  const pill = (label: string, n: number, cls: string) => (
    <span className={`rounded px-1.5 py-0.5 text-[11px] ${cls}`}>
      {label} {n.toLocaleString()}
    </span>
  );
  return (
    <div className="flex flex-wrap gap-1">
      {pill(
        "valid",
        counts.valid,
        "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300",
      )}
      {pill(
        "invalid",
        counts.invalid,
        "bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300",
      )}
      {pill("empty", counts.empty, "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300")}
      {pill(
        "null token",
        counts.nullToken,
        "bg-sky-100 text-sky-700 dark:bg-sky-500/15 dark:text-sky-300",
      )}
      {pill(
        "missing",
        counts.missing,
        "bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300",
      )}
    </div>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary =
  "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40";
const btnOutline =
  "rounded border border-zinc-200 px-2.5 py-1 text-xs hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
const btnDanger =
  "rounded border border-red-200 px-2 py-1 text-xs text-red-600 hover:bg-red-50 disabled:opacity-40 dark:border-red-900/60 dark:text-red-400 dark:hover:bg-red-500/10";
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
const chip =
  "inline-flex items-center rounded bg-zinc-100 px-1.5 py-0.5 text-[11px] text-zinc-700 dark:bg-zinc-800 dark:text-zinc-200";
const inputCls =
  "w-full rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const selectCls =
  "w-full rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const fieldLabelText = "text-xs font-medium text-zinc-500 dark:text-zinc-400";
const fieldLabel = `flex flex-col gap-1 ${fieldLabelText}`;
