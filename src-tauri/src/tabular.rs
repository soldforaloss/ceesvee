//! Shared tabular source/sink contracts (F31-cycle infrastructure).
//!
//! A [`TabularSource`] is anything that can describe a logical schema and
//! serve bounded, windowed row reads: today the two document backings
//! (in-memory editable and F10 indexed read-only) plus an owned in-memory
//! table; later Parquet/Arrow indexed reads, JSONL streaming, Excel range
//! imports, SQLite table reads, SQL result materialization and streaming
//! sampling (F48). A [`TabularSink`] consumes a schema plus streamed row
//! batches and commits through the F03 atomic-save pipeline, so partial
//! output is never visible at the destination.
//!
//! Cells are `Option<String>`: `None` means the field is MISSING from the
//! record (a short JSONL object, an absent Excel cell, SQL `NULL`), while
//! `Some("")` is a present-but-empty value. Document-backed sources always
//! produce `Some` — the grid is rectangular by construction (import-time
//! padding is recorded in [`crate::parse::ImportInfo`], not in the cells) —
//! but the distinction is part of the contract so future sources can carry
//! it and sinks can decide how to narrow it (CSV writes missing as empty).
//!
//! ## Contract scope and access model
//!
//! **Text values.** Cells are text (`Option<String>`); this contract version
//! deliberately carries no binary or typed-blob slot, and the reused
//! [`ColumnSchema`] (F31) has no binary logical type. A source over a column
//! that holds raw bytes — a SQLite `BLOB`, a Parquet/Arrow `Binary` / `List`
//! / `Struct` — is therefore OUT OF SCOPE until the contract grows a canonical
//! carrier: either a `LogicalType::Binary` plus one agreed text encoding
//! (e.g. base64) or a typed cell slot, so independently written sources cannot
//! diverge on an ad hoc convention. Text-representable sources (CSV, JSONL,
//! most SQL columns, Excel ranges) fit as-is.
//!
//! **Sequential streaming.** The streaming helpers ([`copy`],
//! [`crate::row_identity::build_key_index`], the F20 append reader) only ever
//! request monotonically increasing, contiguous windows — offset `0`, then
//! `0 + len`, and so on. A source that supports random access MAY answer any
//! `offset` (the document and in-memory sources here do); a genuinely
//! single-pass, unseekable source (JSONL off a pipe, an F48 reservoir sample)
//! need only serve forward contiguous reads and still composes with every
//! helper, rather than being forced to buffer or rescan to fake an
//! arbitrary-offset read.
//!
//! **Fixed schema.** [`TabularSource::columns`] is the source's COMPLETE
//! logical schema, stable for the source's lifetime: [`copy`] reads it exactly
//! once, up front, before any row. A schema-on-read source whose records carry
//! heterogeneous key sets (JSONL field drift) must commit to its full column
//! set before serving reads — by sniffing a bounded prefix and unioning, or by
//! a declared projection — never by growing the schema mid-stream.

use std::io::Write;

use csv::{QuoteStyle, Terminator, WriterBuilder};
use encoding_rs::Encoding;

use crate::document::Document;
use crate::dto::{ExportOptions, FileFingerprint};
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::schema::ColumnSchema;
use crate::{encoding, export, save, util};

/// Rows per window used by the streaming helpers ([`copy`], the F20 append
/// reader, the row-identity resolver). Matches the indexed backing's block
/// size, so one window is one contiguous index read.
pub const DEFAULT_WINDOW: usize = 4096;

/// Cooperative-cancellation check interval inside a window read: a cancel is
/// observed within this many rows. Finer than the app's existing full-scan
/// granularity (schema inference checks per 4096-row chunk), so streaming
/// consumers (append, [`copy`]) stop promptly without paying a per-row check
/// on the hot path. Shared with consumers that push a full window through
/// their own row loop (the F20 append writer) so cancellation is honoured at
/// the same granularity mid-batch, not only between window reads.
pub(crate) const CANCEL_CHECK_EVERY: usize = 1024;

/// One owned row: `None` = missing field, `Some("")` = present but empty.
pub type TabularRow = Vec<Option<String>>;

/// One column of a source's logical schema.
#[derive(Debug, Clone, PartialEq)]
pub struct TabularColumn {
    /// Physical column name (header text, possibly synthetic).
    pub name: String,
    /// Stable column ID (F12) when the source has one. Key specs
    /// ([`crate::row_identity::KeySpec`]) address columns by this, so a
    /// non-document source that wants to take part in key matching must
    /// synthesise a stable id: the physical column name when names are unique,
    /// otherwise a positional `col-{index}`. `None` means the column cannot be
    /// named in a key spec.
    pub id: Option<String>,
    /// Declared logical schema (F31), when one exists.
    pub schema: Option<ColumnSchema>,
}

/// How well a source knows its row count up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowCountHint {
    /// The count is exact (documents know their length).
    Exact(u64),
    /// A cheap extrapolation (e.g. file-size based); reads may return more
    /// or fewer rows.
    Estimate(u64),
    /// Nothing useful is known (unbounded streams).
    Unknown,
}

/// Content identity of a source, for change detection: capture one, compare
/// later for equality — inequality means the content may have changed and
/// derived results (key indexes, patches, samples) must be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentFingerprint {
    /// Identity of the backing file (size + mtime) — the same fingerprint
    /// the app's external-change detection uses ([`util::stat_fingerprint`]).
    File(FileFingerprint),
    /// Monotonic document revision, for editable documents whose in-memory
    /// content can be ahead of any file: every content change bumps it.
    Revision { doc_id: u64, revision: u64 },
    /// No identity available; treat every read as potentially changed.
    Unknown,
}

/// A readable table: logical schema, row-count hint, bounded window reads,
/// cooperative cancellation and a content fingerprint.
pub trait TabularSource {
    /// The logical columns, in read order.
    fn columns(&self) -> Vec<TabularColumn>;

    /// Whether [`TabularSource::columns`] carries real header text (`true`) or
    /// synthetic positional placeholders (`false`, e.g. a document opened
    /// without a header row exposes `Column 1`, `Column 2`, ...). A sink uses
    /// this to decide whether emitting a header record is meaningful: a
    /// header-less source must not have its placeholder names written out as a
    /// header row, which would shift every data row. Sources whose column
    /// names are always caller-supplied default to `true`.
    fn has_header_row(&self) -> bool {
        true
    }

    /// How many data rows the source has, as well as it knows.
    fn row_count(&self) -> RowCountHint;

    /// Read up to `limit` rows starting at absolute row `offset` (owned).
    /// An offset at or past the end yields an empty batch; `limit == 0`
    /// yields an empty batch; a window crossing the end yields the partial
    /// tail. Cancellation is observed cooperatively through `ctx`.
    ///
    /// The streaming helpers only request monotonically increasing, contiguous
    /// windows (see the module's access-model note): a random-access source may
    /// serve any `offset`, but a sequential-only source need only support
    /// forward contiguous reads.
    fn read_rows(
        &self,
        offset: u64,
        limit: usize,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<TabularRow>>;

    /// The source's current content identity (see [`ContentFingerprint`]).
    fn fingerprint(&self) -> ContentFingerprint;
}

/// A writable table: declare the schema once, stream row batches, then
/// commit atomically. Dropping a sink without [`TabularSink::finish`] —
/// after an error or a cancellation — must clean up all partial output and
/// leave any existing destination untouched.
pub trait TabularSink {
    /// Declare the output schema. Must be called exactly once, before any
    /// batch. `has_header_row` reports whether `columns` carries real header
    /// text (see [`TabularSource::has_header_row`]); a sink that would
    /// otherwise emit a header record must skip it when this is `false` so a
    /// header-less source never gains a synthetic header row.
    fn begin(&mut self, columns: &[TabularColumn], has_header_row: bool) -> AppResult<()>;

    /// Append a batch of rows. Rows shorter than the schema are padded as
    /// missing; wider rows are rejected (never silently truncated).
    fn write_rows(&mut self, rows: &[TabularRow], ctx: Option<&JobCtx>) -> AppResult<()>;

    /// Commit: make the output visible atomically. Returns bytes written.
    fn finish(&mut self) -> AppResult<u64>;
}

/// Stream everything in `source` into `sink` in bounded windows, with
/// progress + cancellation through `ctx`. Returns the sink's byte count.
pub fn copy(
    source: &dyn TabularSource,
    sink: &mut dyn TabularSink,
    ctx: Option<&JobCtx>,
) -> AppResult<u64> {
    sink.begin(&source.columns(), source.has_header_row())?;
    if let (Some(ctx), RowCountHint::Exact(n)) = (ctx, source.row_count()) {
        ctx.set_total(n);
    }
    let mut offset = 0u64;
    loop {
        let rows = source.read_rows(offset, DEFAULT_WINDOW, ctx)?;
        if rows.is_empty() {
            break;
        }
        sink.write_rows(&rows, ctx)?;
        offset += rows.len() as u64;
        if let Some(ctx) = ctx {
            ctx.advance(rows.len() as u64)?;
        }
        if rows.len() < DEFAULT_WINDOW {
            break;
        }
    }
    sink.finish()
}

// ----- sources ---------------------------------------------------------------------

/// [`TabularSource`] over an open [`Document`]. Covers BOTH storage
/// backings: the document already unifies in-memory editable rows and the
/// F10 record index behind [`Document::visit_rows`], so one adapter serves
/// both — only the fingerprint branches (editable content is identified by
/// its revision, indexed content by its backing file).
pub struct DocumentSource<'a> {
    doc: &'a Document,
}

impl<'a> DocumentSource<'a> {
    pub fn new(doc: &'a Document) -> DocumentSource<'a> {
        DocumentSource { doc }
    }
}

impl TabularSource for DocumentSource<'_> {
    fn columns(&self) -> Vec<TabularColumn> {
        let ids = self.doc.column_ids();
        self.doc
            .headers()
            .iter()
            .enumerate()
            .map(|(i, name)| TabularColumn {
                name: name.clone(),
                id: ids.get(i).cloned(),
                schema: self.doc.column_schema_at(i).cloned(),
            })
            .collect()
    }

    fn has_header_row(&self) -> bool {
        // Header-less documents expose synthetic `Column N` names; surface that
        // provenance so a sink suppresses them exactly like the export path.
        self.doc.has_header_row()
    }

    fn row_count(&self) -> RowCountHint {
        RowCountHint::Exact(self.doc.n_rows() as u64)
    }

    fn read_rows(
        &self,
        offset: u64,
        limit: usize,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<TabularRow>> {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let n = self.doc.n_rows();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(n);
        let end = start.saturating_add(limit).min(n);
        let mut out: Vec<TabularRow> = Vec::with_capacity(end - start);
        self.doc.visit_rows(start..end, &mut |i, row| {
            if let Some(ctx) = ctx {
                if (i - start) % CANCEL_CHECK_EVERY == 0 {
                    ctx.check()?;
                }
            }
            out.push(row.iter().map(|c| Some(c.clone())).collect());
            Ok(true)
        })?;
        Ok(out)
    }

    fn fingerprint(&self) -> ContentFingerprint {
        if self.doc.is_editable() {
            // In-memory content can be ahead of any saved file; the revision
            // moves on every content change (and never on pure view changes
            // for the worse — false positives are safe, misses are not).
            ContentFingerprint::Revision {
                doc_id: self.doc.id,
                revision: self.doc.revision(),
            }
        } else {
            // Indexed content IS the backing file; reuse the same identity
            // external-change detection stores. Derived spilled documents
            // have no recorded fingerprint — fall back to the revision.
            match self.doc.fingerprint() {
                Some(fp) => ContentFingerprint::File(fp),
                None => ContentFingerprint::Revision {
                    doc_id: self.doc.id,
                    revision: self.doc.revision(),
                },
            }
        }
    }
}

/// An owned in-memory table. The reference implementation of the
/// missing-vs-empty contract (its cells CAN be `None`), and a convenient shim
/// for SMALL, already-materialized results (a bounded sample, a modest SQL
/// result). It holds every row in memory with no size guard, so a large query
/// result must instead stream through a purpose-built, cursor-backed
/// [`TabularSource`] rather than being fully materialized here.
#[derive(Debug, Clone, Default)]
pub struct MemSource {
    columns: Vec<TabularColumn>,
    rows: Vec<TabularRow>,
}

impl MemSource {
    pub fn new(columns: Vec<TabularColumn>, rows: Vec<TabularRow>) -> MemSource {
        MemSource { columns, rows }
    }
}

impl TabularSource for MemSource {
    fn columns(&self) -> Vec<TabularColumn> {
        self.columns.clone()
    }

    fn row_count(&self) -> RowCountHint {
        RowCountHint::Exact(self.rows.len() as u64)
    }

    fn read_rows(
        &self,
        offset: u64,
        limit: usize,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<TabularRow>> {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        let n = self.rows.len();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(n);
        let end = start.saturating_add(limit).min(n);
        Ok(self.rows[start..end].to_vec())
    }

    fn fingerprint(&self) -> ContentFingerprint {
        ContentFingerprint::Unknown
    }
}

// ----- the CSV sink ----------------------------------------------------------------

/// [`TabularSink`] producing delimited output with the app's export options
/// (delimiter, quote style, line ending, target encoding, BOM, headers),
/// through the F03 atomic pipeline: everything streams into a staging file
/// and only [`TabularSink::finish`] swaps it into place. Dropping the sink
/// earlier removes the staging file; an existing destination is untouched.
///
/// Missing cells (`None`) are written as empty fields — CSV cannot express
/// the distinction, and this sink narrows it explicitly.
pub struct CsvSink {
    delimiter: u8,
    quote_style: QuoteStyle,
    terminator: Terminator,
    target: &'static Encoding,
    bom: bool,
    include_headers: bool,
    writer: csv::Writer<Vec<u8>>,
    /// Rough bytes fed to the writer since the last drain (the csv writer
    /// hides its inner buffer); drained past [`export::FLUSH_THRESHOLD`].
    estimated: usize,
    /// `Some` until [`TabularSink::finish`] commits.
    out: Option<save::AtomicWriter>,
    bytes: u64,
    n_cols: Option<usize>,
}

impl CsvSink {
    /// Open the staging file next to `dest`. Nothing is visible at `dest`
    /// until [`TabularSink::finish`].
    pub fn create(dest: &std::path::Path, options: &ExportOptions) -> AppResult<CsvSink> {
        let delimiter = util::delimiter_to_byte(&options.delimiter);
        let quote_style = export::quote_style_of(options);
        let terminator = export::terminator_of(options);
        let out = save::AtomicWriter::create(dest, options.backup)?;
        Ok(CsvSink {
            delimiter,
            quote_style,
            terminator,
            target: encoding::from_name(&options.encoding),
            bom: options.bom,
            include_headers: options.include_headers,
            writer: Self::make_writer(delimiter, quote_style, terminator, Vec::new()),
            estimated: 0,
            out: Some(out),
            bytes: 0,
            n_cols: None,
        })
    }

    fn make_writer(
        delimiter: u8,
        quote_style: QuoteStyle,
        terminator: Terminator,
        buf: Vec<u8>,
    ) -> csv::Writer<Vec<u8>> {
        WriterBuilder::new()
            .delimiter(delimiter)
            .quote_style(quote_style)
            .terminator(terminator)
            .from_writer(buf)
    }

    /// Flush the csv writer and transcode its buffer into the staging file,
    /// reusing the (cleared) allocation.
    fn drain(&mut self, ctx: Option<&JobCtx>) -> AppResult<()> {
        self.writer.flush()?;
        let rotated = std::mem::replace(
            &mut self.writer,
            Self::make_writer(
                self.delimiter,
                self.quote_style,
                self.terminator,
                Vec::new(),
            ),
        );
        let mut buf = rotated
            .into_inner()
            .map_err(|e| AppError::Other(e.to_string()))?;
        let out = self
            .out
            .as_mut()
            .ok_or_else(|| AppError::invalid("the sink is already finished"))?;
        self.bytes += export::transcode_chunk(&mut buf, self.target, out.file_mut(), ctx)?;
        self.writer = Self::make_writer(self.delimiter, self.quote_style, self.terminator, buf);
        self.estimated = 0;
        Ok(())
    }
}

impl TabularSink for CsvSink {
    fn begin(&mut self, columns: &[TabularColumn], has_header_row: bool) -> AppResult<()> {
        if self.n_cols.is_some() {
            return Err(AppError::invalid("the sink schema was already declared"));
        }
        if columns.is_empty() {
            return Err(AppError::invalid("the output needs at least one column"));
        }
        let out = self
            .out
            .as_mut()
            .ok_or_else(|| AppError::invalid("the sink is already finished"))?;
        if self.bom {
            let bom = encoding::bom_for(self.target);
            out.file_mut().write_all(bom)?;
            self.bytes += bom.len() as u64;
        }
        // Mirror the export pipeline: emit a header record only when the caller
        // asked for headers AND the source's column names are real header text.
        // A header-less source carries synthetic `Column N` placeholders; those
        // must never be written as a header row (it would shift every data row).
        if self.include_headers && has_header_row {
            self.writer
                .write_record(columns.iter().map(|c| c.name.as_bytes()))?;
        }
        self.n_cols = Some(columns.len());
        Ok(())
    }

    fn write_rows(&mut self, rows: &[TabularRow], ctx: Option<&JobCtx>) -> AppResult<()> {
        let n_cols = self
            .n_cols
            .ok_or_else(|| AppError::invalid("declare the sink schema (begin) before rows"))?;
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        for row in rows {
            if row.len() > n_cols {
                return Err(AppError::invalid(format!(
                    "a row has {} cells but the schema declares {n_cols} columns",
                    row.len()
                )));
            }
            // Short rows pad as missing; missing narrows to an empty field.
            self.writer.write_record((0..n_cols).map(|i| {
                row.get(i)
                    .and_then(Option::as_deref)
                    .unwrap_or("")
                    .as_bytes()
            }))?;
            self.estimated += row
                .iter()
                .map(|c| c.as_deref().map_or(0, str::len))
                .sum::<usize>()
                + n_cols
                + 2;
            if self.estimated >= export::FLUSH_THRESHOLD {
                self.drain(ctx)?;
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> AppResult<u64> {
        if self.n_cols.is_none() {
            return Err(AppError::invalid(
                "declare the sink schema (begin) before finishing",
            ));
        }
        self.drain(None)?;
        let out = self
            .out
            .take()
            .ok_or_else(|| AppError::invalid("the sink is already finished"))?;
        out.commit()?;
        Ok(self.bytes)
    }
}

impl std::fmt::Debug for CsvSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CsvSink")
            .field("bytes", &self.bytes)
            .field("n_cols", &self.n_cols)
            .field("committed", &self.out.is_none())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derived::DerivedDocumentBuilder;
    use crate::dto::BackupPolicy;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};
    use crate::schema::{ColumnSchema, LogicalType};

    fn doc(csv: &str, has_header: bool) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, has_header)
    }

    /// An INDEXED document (spilled through the derived builder) plus the
    /// tempdir keeping its backing file alive.
    fn indexed_doc(rows: usize) -> (tempfile::TempDir, Document) {
        let dir = tempfile::tempdir().unwrap();
        let mut b = DerivedDocumentBuilder::new(
            vec!["id".into(), "name".into()],
            dir.path().to_path_buf(),
            1, // spill immediately
        );
        for i in 0..rows {
            b.push_row(vec![i.to_string(), format!("name-{i}")])
                .unwrap();
        }
        assert!(b.spilled());
        let doc = b.finish(7, &mut |_| Ok(())).unwrap();
        assert!(!doc.is_editable());
        (dir, doc)
    }

    fn options() -> ExportOptions {
        ExportOptions {
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            quote_style: "minimal".into(),
            line_ending: "lf".into(),
            bom: false,
            include_headers: true,
            backup: BackupPolicy::None,
        }
    }

    fn col(name: &str) -> TabularColumn {
        TabularColumn {
            name: name.into(),
            id: Some(format!("c-{name}")),
            schema: None,
        }
    }

    fn cancelled_ctx(registry: &JobRegistry) -> JobCtx {
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        ctx
    }

    // ----- windowed reads -----------------------------------------------------

    #[test]
    fn editable_document_windows_at_boundaries() {
        let d = doc("h\n0\n1\n2\n3\n4\n", true);
        let s = DocumentSource::new(&d);
        assert_eq!(s.row_count(), RowCountHint::Exact(5));

        // Interior window.
        let rows = s.read_rows(1, 2, None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec![Some("1".to_string())]);
        assert_eq!(rows[1], vec![Some("2".to_string())]);

        // Window crossing the end returns the partial tail.
        let rows = s.read_rows(3, 10, None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], vec![Some("4".to_string())]);

        // Offset exactly at / past the end and zero limit are empty.
        assert!(s.read_rows(5, 10, None).unwrap().is_empty());
        assert!(s.read_rows(9999, 10, None).unwrap().is_empty());
        assert!(s.read_rows(u64::MAX, 10, None).unwrap().is_empty());
        assert!(s.read_rows(0, 0, None).unwrap().is_empty());
    }

    #[test]
    fn indexed_document_windows_at_boundaries() {
        let (_dir, d) = indexed_doc(10);
        let s = DocumentSource::new(&d);
        assert_eq!(s.row_count(), RowCountHint::Exact(10));

        let rows = s.read_rows(8, 5, None).unwrap();
        assert_eq!(rows.len(), 2, "window past the end clamps");
        assert_eq!(rows[0][0], Some("8".to_string()));
        assert_eq!(rows[1][1], Some("name-9".to_string()));

        assert!(s.read_rows(10, 4, None).unwrap().is_empty());
        assert!(s.read_rows(0, 0, None).unwrap().is_empty());
    }

    #[test]
    fn document_cells_are_always_present() {
        // Ragged imports are padded at parse time; the padding is a recorded
        // import fact, not a per-cell one, so document sources yield `Some`.
        let d = doc("a,b,c\n1,2\n", true);
        let s = DocumentSource::new(&d);
        let rows = s.read_rows(0, 10, None).unwrap();
        assert_eq!(
            rows[0],
            vec![
                Some("1".to_string()),
                Some("2".to_string()),
                Some(String::new())
            ]
        );
    }

    #[test]
    fn mem_source_preserves_missing_vs_empty() {
        let s = MemSource::new(
            vec![col("a"), col("b")],
            vec![
                vec![Some("x".into()), None],
                vec![Some(String::new()), Some("y".into())],
            ],
        );
        let rows = s.read_rows(0, 10, None).unwrap();
        assert_eq!(rows[0][1], None, "missing stays missing");
        assert_eq!(rows[1][0], Some(String::new()), "empty stays empty");
        assert_ne!(rows[0][1], rows[1][0], "missing and empty are distinct");

        // Boundaries behave like every other source.
        assert!(s.read_rows(2, 5, None).unwrap().is_empty());
        assert!(s.read_rows(0, 0, None).unwrap().is_empty());
    }

    #[test]
    fn columns_carry_ids_and_declared_schemas() {
        let mut d = doc("id,amount\n1,2.50\n", true);
        let ids = d.column_ids().to_vec();
        d.set_column_schema(ColumnSchema::new(
            ids[1].clone(),
            "amount",
            LogicalType::Decimal,
        ));
        let s = DocumentSource::new(&d);
        let cols = s.columns();
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[0].id.as_deref(), Some(ids[0].as_str()));
        assert!(cols[0].schema.is_none());
        assert_eq!(
            cols[1].schema.as_ref().map(|s| s.logical_type),
            Some(LogicalType::Decimal)
        );
    }

    // ----- cancellation -------------------------------------------------------

    #[test]
    fn cancellation_fails_a_read() {
        let registry = JobRegistry::default();
        let ctx = cancelled_ctx(&registry);
        let d = doc("h\n1\n2\n", true);
        let s = DocumentSource::new(&d);
        assert!(matches!(
            s.read_rows(0, 10, Some(&ctx)),
            Err(AppError::Cancelled)
        ));
    }

    #[test]
    fn cancellation_mid_write_cleans_partial_output() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        std::fs::write(&dest, b"precious").unwrap();

        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        {
            let mut sink = CsvSink::create(&dest, &options()).unwrap();
            sink.begin(&[col("a")], true).unwrap();
            sink.write_rows(&[vec![Some("1".into())]], Some(&ctx))
                .unwrap();
            registry.cancel(ctx.id);
            assert!(matches!(
                sink.write_rows(&[vec![Some("2".into())]], Some(&ctx)),
                Err(AppError::Cancelled)
            ));
            // The sink drops here without finish() — the cancellation path.
        }
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"precious",
            "destination untouched after cancellation"
        );
        let stray = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".ceesvee-save-"));
        assert!(!stray, "staging file cleaned up");
    }

    // ----- fingerprints -------------------------------------------------------

    #[test]
    fn editable_fingerprint_is_stable_and_tracks_content_changes() {
        let mut d = doc("h\n1\n2\n", true);
        let before = DocumentSource::new(&d).fingerprint();
        assert_eq!(
            before,
            DocumentSource::new(&d).fingerprint(),
            "stable across reads"
        );
        assert!(matches!(before, ContentFingerprint::Revision { .. }));

        d.set_cell(0, 0, "changed".into()).unwrap();
        let after = DocumentSource::new(&d).fingerprint();
        assert_ne!(before, after, "an edit must change the fingerprint");
    }

    #[test]
    fn indexed_fingerprint_reuses_the_external_change_identity() {
        let (_dir, mut d) = indexed_doc(3);
        // Derived documents have no recorded file identity: revision fallback.
        assert!(matches!(
            DocumentSource::new(&d).fingerprint(),
            ContentFingerprint::Revision { .. }
        ));

        // Documents opened from a file store the stat fingerprint; the source
        // must surface exactly that value.
        let fp = FileFingerprint {
            size: 42,
            modified_at_ms: 1234,
        };
        d.set_fingerprint(Some(fp));
        assert_eq!(
            DocumentSource::new(&d).fingerprint(),
            ContentFingerprint::File(fp)
        );
    }

    // ----- the CSV sink -------------------------------------------------------

    #[test]
    fn sink_streams_and_commits_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let mut sink = CsvSink::create(&dest, &options()).unwrap();
        sink.begin(&[col("a"), col("b")], true).unwrap();
        assert!(!dest.exists(), "nothing visible before finish");
        sink.write_rows(
            &[
                vec![Some("1".into()), Some("x, quoted".into())],
                vec![Some("2".into()), None], // missing narrows to empty
                vec![Some("3".into())],       // short row pads
            ],
            None,
        )
        .unwrap();
        assert!(!dest.exists(), "still nothing visible before finish");
        let bytes = sink.finish().unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), bytes);
        let text = String::from_utf8(std::fs::read(&dest).unwrap()).unwrap();
        assert_eq!(text, "a,b\n1,\"x, quoted\"\n2,\n3,\n");
    }

    #[test]
    fn sink_drop_without_finish_is_a_crash_sim() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        {
            let mut sink = CsvSink::create(&dest, &options()).unwrap();
            sink.begin(&[col("a")], true).unwrap();
            // Enough data to force several 64 KiB drains into the staging
            // file, then "crash" (drop) before the commit.
            let batch: Vec<TabularRow> = (0..40_000)
                .map(|i| vec![Some(format!("row-{i}-payload"))])
                .collect();
            sink.write_rows(&batch, None).unwrap();
        }
        assert!(!dest.exists(), "no partial file visible after a crash");
        let stray = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .count();
        assert_eq!(stray, 0, "no staging leftovers");
    }

    #[test]
    fn sink_enforces_the_declared_schema() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let mut sink = CsvSink::create(&dest, &options()).unwrap();
        assert!(
            sink.write_rows(&[vec![Some("1".into())]], None).is_err(),
            "rows before begin are rejected"
        );
        sink.begin(&[col("a")], true).unwrap();
        assert!(
            sink.begin(&[col("a")], true).is_err(),
            "double begin is rejected"
        );
        assert!(
            sink.write_rows(&[vec![Some("1".into()), Some("2".into())]], None)
                .is_err(),
            "wider rows are rejected, never truncated"
        );
    }

    #[test]
    fn sink_honours_export_options() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let mut opts = options();
        opts.delimiter = ";".into();
        opts.line_ending = "crlf".into();
        opts.bom = true;
        opts.include_headers = false;
        let mut sink = CsvSink::create(&dest, &opts).unwrap();
        sink.begin(&[col("a"), col("b")], true).unwrap();
        sink.write_rows(&[vec![Some("1".into()), Some("2".into())]], None)
            .unwrap();
        sink.finish().unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(&bytes[0..3], &[0xEF, 0xBB, 0xBF], "UTF-8 BOM");
        assert_eq!(&bytes[3..], b"1;2\r\n");
    }

    #[test]
    fn sink_suppresses_headers_without_provenance() {
        // include_headers is on, but the schema carries no real header text
        // (has_header_row = false): the synthetic names must NOT be written as
        // a header record, matching the export pipeline that skips headers
        // unless the document actually has one.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let mut sink = CsvSink::create(&dest, &options()).unwrap();
        assert!(options().include_headers, "guard: headers requested");
        sink.begin(&[col("Column 1"), col("Column 2")], false)
            .unwrap();
        sink.write_rows(&[vec![Some("1".into()), Some("2".into())]], None)
            .unwrap();
        sink.finish().unwrap();
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"1,2\n",
            "no synthetic header record"
        );
    }

    #[test]
    fn sink_transcodes_and_fails_on_unmappable() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let mut opts = options();
        opts.encoding = "windows-1252".into();
        opts.include_headers = false;
        let mut sink = CsvSink::create(&dest, &opts).unwrap();
        sink.begin(&[col("a")], true).unwrap();
        sink.write_rows(&[vec![Some("café".into())]], None).unwrap();
        sink.finish().unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"caf\xE9\n");

        let dest2 = dir.path().join("out2.csv");
        let mut sink = CsvSink::create(&dest2, &opts).unwrap();
        sink.begin(&[col("a")], true).unwrap();
        sink.write_rows(&[vec![Some("a → b".into())]], None)
            .unwrap();
        assert!(sink.finish().is_err(), "unmappable characters fail");
        assert!(!dest2.exists(), "failed commit leaves nothing behind");
    }

    // ----- end-to-end ---------------------------------------------------------

    #[test]
    fn copy_round_trips_a_document_through_the_sink() {
        let d = doc("name,note\nAda,\"hello, world\"\nBob,two\nlines here", true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("copy.csv");
        let mut sink = CsvSink::create(&dest, &options()).unwrap();
        let bytes = copy(&DocumentSource::new(&d), &mut sink, None).unwrap();
        assert!(bytes > 0);

        let reparsed = parse(&std::fs::read(&dest).unwrap(), &ParseSettings::default()).unwrap();
        assert_eq!(reparsed.records[0], vec!["name", "note"]);
        assert_eq!(reparsed.records[1], vec!["Ada", "hello, world"]);
        assert_eq!(reparsed.records[2], vec!["Bob", "two"]);
    }

    #[test]
    fn copy_omits_synthetic_headers_for_a_header_less_document() {
        // A document opened without a header row exposes synthetic `Column N`
        // names. Copying it through the sink with include_headers on must not
        // write those as a header record, which would prepend a bogus row and
        // shift every data row down.
        let d = doc("1,2\n3,4\n", false);
        assert!(!d.has_header_row());
        assert_eq!(d.headers(), &["Column 1", "Column 2"]);

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("copy.csv");
        let mut sink = CsvSink::create(&dest, &options()).unwrap();
        assert!(options().include_headers, "guard: headers requested");
        copy(&DocumentSource::new(&d), &mut sink, None).unwrap();

        let text = String::from_utf8(std::fs::read(&dest).unwrap()).unwrap();
        assert_eq!(text, "1,2\n3,4\n", "data rows only, no synthetic header");
    }

    #[test]
    fn copy_streams_an_indexed_document_across_windows() {
        // More rows than one DEFAULT_WINDOW, read through the record index.
        let (_dir, d) = indexed_doc(DEFAULT_WINDOW + 50);
        let out_dir = tempfile::tempdir().unwrap();
        let dest = out_dir.path().join("copy.csv");
        let mut sink = CsvSink::create(&dest, &options()).unwrap();
        copy(&DocumentSource::new(&d), &mut sink, None).unwrap();

        let reparsed = parse(&std::fs::read(&dest).unwrap(), &ParseSettings::default()).unwrap();
        assert_eq!(reparsed.records.len(), DEFAULT_WINDOW + 51, "header + rows");
        let last = &reparsed.records[DEFAULT_WINDOW + 50];
        assert_eq!(last[0], (DEFAULT_WINDOW + 49).to_string());
    }

    #[test]
    fn copy_observes_cancellation() {
        let registry = JobRegistry::default();
        let ctx = cancelled_ctx(&registry);
        let d = doc("h\n1\n", true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let mut sink = CsvSink::create(&dest, &options()).unwrap();
        let result = copy(&DocumentSource::new(&d), &mut sink, Some(&ctx));
        assert!(matches!(result, Err(AppError::Cancelled)));
        drop(sink);
        assert!(!dest.exists(), "cancelled copy leaves no output");
    }
}
