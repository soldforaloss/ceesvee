//! `DerivedDocumentBuilder` (shared by F20–F23): build a NEW document from
//! headers plus a stream of rows, automatically choosing the backing.
//! Rows accumulate in memory until a byte budget is crossed; past it the
//! builder spills everything to a UTF-8 CSV in a guarded temp directory and
//! the finished document opens INDEXED over that file (read-only, exactly
//! like a huge opened file), with the guard deleting the temp on close.
//! Source documents are never mutated — the builder only ever creates.

use std::path::PathBuf;

use crate::document::Document;
use crate::error::AppResult;
use crate::index::{self, IndexDirGuard};
use crate::parse::{ImportInfo, ParsedFile};

/// In-memory cell bytes past which the output spills to disk (matches the
/// open-editable decision threshold).
pub const SPILL_BUDGET: u64 = index::SIZE_DECISION_THRESHOLD;

/// Rough per-cell bookkeeping overhead (String + Vec headers), so the budget
/// approximates real memory, not just text bytes.
const CELL_OVERHEAD: u64 = 32;

pub struct DerivedDocumentBuilder {
    headers: Vec<String>,
    has_header_row: bool,
    cache_root: PathBuf,
    budget: u64,
    rows: Vec<Vec<String>>,
    bytes: u64,
    row_count: usize,
    spill: Option<Spill>,
}

struct Spill {
    guard: IndexDirGuard,
    writer: csv::Writer<std::io::BufWriter<std::fs::File>>,
    path: PathBuf,
}

impl DerivedDocumentBuilder {
    /// `cache_root` is the app's index-cache root (spill directories are
    /// created under it and swept like every other index cache).
    pub fn new(headers: Vec<String>, cache_root: PathBuf, budget: u64) -> DerivedDocumentBuilder {
        DerivedDocumentBuilder {
            headers,
            has_header_row: true,
            cache_root,
            budget,
            rows: Vec::new(),
            bytes: 0,
            row_count: 0,
            spill: None,
        }
    }

    /// Declare whether the supplied names are a real header row (`true`, the
    /// default) or synthetic positional placeholders standing in for a
    /// header-less source (`false`). When `false`, the names are used only as
    /// internal column widths/labels and are NOT emitted as a header record, so
    /// a document derived from a header-less source stays header-less — matching
    /// the CSV export path, which honours
    /// [`crate::tabular::TabularSource::has_header_row`]. Without this, the placeholder `Column N` labels
    /// would leak into the derived document as a genuine header and reappear as
    /// an extra line on the next save/export.
    pub fn with_header_row(mut self, has_header_row: bool) -> DerivedDocumentBuilder {
        self.has_header_row = has_header_row;
        self
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Whether the builder has already spilled to disk.
    pub fn spilled(&self) -> bool {
        self.spill.is_some()
    }

    /// Append one row (padded/truncated to the header width).
    pub fn push_row(&mut self, mut row: Vec<String>) -> AppResult<()> {
        row.resize(self.headers.len(), String::new());
        self.row_count += 1;
        if let Some(spill) = &mut self.spill {
            spill.writer.write_record(&row)?;
            return Ok(());
        }
        self.bytes += row
            .iter()
            .map(|c| c.len() as u64 + CELL_OVERHEAD)
            .sum::<u64>();
        self.rows.push(row);
        if self.bytes > self.budget {
            self.spill_now()?;
        }
        Ok(())
    }

    /// Move everything written so far into a guarded temp CSV.
    fn spill_now(&mut self) -> AppResult<()> {
        let guard = IndexDirGuard::create(&self.cache_root)?;
        let path = guard.dir().join("derived.csv");
        let file = std::fs::File::create(&path)?;
        let mut writer = csv::WriterBuilder::new()
            .delimiter(b',')
            .from_writer(std::io::BufWriter::new(file));
        if self.has_header_row {
            writer.write_record(&self.headers)?;
        }
        for row in self.rows.drain(..) {
            writer.write_record(&row)?;
        }
        self.bytes = 0;
        self.spill = Some(Spill {
            guard,
            writer,
            path,
        });
        Ok(())
    }

    /// Finish the build and produce the document. In-memory outputs become
    /// ordinary editable documents (unsaved, so closing warns); spilled
    /// outputs open INDEXED over the temp file with the guard attached.
    /// `progress` receives byte deltas while the spilled file is indexed.
    pub fn finish(
        self,
        doc_id: u64,
        progress: &mut dyn FnMut(u64) -> AppResult<()>,
    ) -> AppResult<Document> {
        match self.spill {
            None => {
                let n_cols = self.headers.len();
                let mut records = Vec::with_capacity(self.rows.len() + 1);
                if self.has_header_row {
                    records.push(self.headers.clone());
                }
                records.extend(self.rows);
                let parsed = ParsedFile {
                    records,
                    n_cols,
                    delimiter: b',',
                    encoding: encoding_rs::UTF_8,
                    had_bom: false,
                    uses_crlf: false,
                    import: ImportInfo::default(),
                };
                let mut doc = Document::from_parsed(doc_id, None, parsed, self.has_header_row);
                doc.mark_derived_unsaved();
                Ok(doc)
            }
            Some(spill) => {
                let Spill {
                    guard,
                    mut writer,
                    path,
                } = spill;
                writer.flush()?;
                drop(writer);
                let settings = index::IndexSettings {
                    delimiter: Some(b','),
                    encoding: Some(encoding_rs::UTF_8),
                    has_header_row: Some(self.has_header_row),
                    chunk_size: 0,
                };
                let indexed = index::build_index(&path, &self.cache_root, &settings, progress)?;
                let mut doc = Document::from_index(doc_id, None, indexed);
                doc.set_derived_guard(guard);
                Ok(doc)
            }
        }
    }
}

/// Make `base` unique among `existing` by suffixing " (2)", " (3)", ….
pub fn unique_column_name(existing: &[String], base: &str) -> String {
    if !existing.iter().any(|h| h == base) {
        return base.to_string();
    }
    for i in 2.. {
        let candidate = format!("{base} ({i})");
        if !existing.iter().any(|h| h == &candidate) {
            return candidate;
        }
    }
    unreachable!("counter is unbounded")
}

impl std::fmt::Debug for DerivedDocumentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DerivedDocumentBuilder")
            .field("headers", &self.headers.len())
            .field("rows", &self.row_count)
            .field("spilled", &self.spilled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder(budget: u64) -> (tempfile::TempDir, DerivedDocumentBuilder) {
        let dir = tempfile::tempdir().unwrap();
        let b = DerivedDocumentBuilder::new(
            vec!["a".into(), "b".into()],
            dir.path().to_path_buf(),
            budget,
        );
        (dir, b)
    }

    #[test]
    fn small_outputs_stay_in_memory_and_are_editable_but_unsaved() {
        let (_dir, mut b) = builder(SPILL_BUDGET);
        b.push_row(vec!["1".into(), "x".into()]).unwrap();
        b.push_row(vec!["2".into()]).unwrap(); // short rows pad
        assert!(!b.spilled());
        let doc = b.finish(1, &mut |_| Ok(())).unwrap();
        assert!(doc.is_editable());
        assert_eq!(doc.headers(), &["a", "b"]);
        assert_eq!(doc.n_rows(), 2);
        assert_eq!(doc.rows()[1], vec!["2".to_string(), String::new()]);
        assert!(doc.is_dirty(), "derived documents start unsaved");
        assert!(doc.meta().path.is_none());
    }

    #[test]
    fn crossing_the_budget_spills_to_an_indexed_document() {
        // A tiny budget forces the spill after the first pushes.
        let (_dir, mut b) = builder(64);
        for i in 0..50 {
            b.push_row(vec![format!("row {i}"), "some text payload".into()])
                .unwrap();
        }
        assert!(b.spilled());
        assert_eq!(b.row_count(), 50);
        let doc = b.finish(2, &mut |_| Ok(())).unwrap();
        assert!(!doc.is_editable(), "spilled outputs open indexed");
        assert_eq!(doc.n_rows(), 50);
        assert_eq!(doc.headers(), &["a", "b"]);
        // Values round-trip through the CSV spill + index.
        let rows = doc.fetch_rows(&[0, 49]).unwrap();
        assert_eq!(rows[0][0], "row 0");
        assert_eq!(rows[1][0], "row 49");
    }

    #[test]
    fn spilled_values_with_quotes_and_newlines_round_trip() {
        let (_dir, mut b) = builder(1);
        b.push_row(vec!["he said \"hi\"".into(), "two\nlines".into()])
            .unwrap();
        b.push_row(vec!["comma, inside".into(), "plain".into()])
            .unwrap();
        let doc = b.finish(3, &mut |_| Ok(())).unwrap();
        let rows = doc.fetch_rows(&[0, 1]).unwrap();
        assert_eq!(rows[0][0], "he said \"hi\"");
        assert_eq!(rows[0][1], "two\nlines");
        assert_eq!(rows[1][0], "comma, inside");
    }

    #[test]
    fn headerless_in_memory_output_omits_the_synthetic_header() {
        // `with_header_row(false)` marks the supplied names as placeholders for
        // a header-less source: they must NOT be written as a real header row.
        let (_dir, b) = builder(SPILL_BUDGET);
        let mut b = b.with_header_row(false);
        b.push_row(vec!["1".into(), "x".into()]).unwrap();
        b.push_row(vec!["2".into(), "y".into()]).unwrap();
        assert!(!b.spilled());
        let doc = b.finish(1, &mut |_| Ok(())).unwrap();
        assert!(!doc.has_header_row(), "header-less provenance is preserved");
        assert_eq!(doc.n_rows(), 2, "no row was consumed as a header");
        assert_eq!(doc.rows()[0], vec!["1".to_string(), "x".to_string()]);
        // Labels fall back to synthetic placeholders (never the input names).
        assert_eq!(doc.headers(), &["Column 1", "Column 2"]);
    }

    #[test]
    fn headerless_spilled_output_omits_the_synthetic_header() {
        // The same must hold once the output spills to an indexed CSV: the
        // spill file gets no header line and the index opens header-less.
        let (_dir, b) = builder(1); // a tiny budget forces the spill immediately
        let mut b = b.with_header_row(false);
        for i in 0..30 {
            b.push_row(vec![format!("{i}"), format!("v{i}")]).unwrap();
        }
        assert!(b.spilled());
        let doc = b.finish(2, &mut |_| Ok(())).unwrap();
        assert!(!doc.has_header_row(), "spilled output stays header-less");
        assert_eq!(doc.n_rows(), 30, "the spill wrote no synthetic header line");
        let rows = doc.fetch_rows(&[0, 29]).unwrap();
        assert_eq!(rows[0], vec!["0".to_string(), "v0".to_string()]);
        assert_eq!(rows[1], vec!["29".to_string(), "v29".to_string()]);
    }

    #[test]
    fn unique_names_avoid_collisions() {
        let existing = vec!["a".to_string(), "source_file".to_string()];
        assert_eq!(unique_column_name(&existing, "b"), "b");
        assert_eq!(
            unique_column_name(&existing, "source_file"),
            "source_file (2)"
        );
    }
}
