//! The opt-in tantivy recall backend (the `recall-tantivy` feature).
//!
//! Holds one tantivy index per region: in RAM (a `RamDirectory`, writing nothing
//! to disk) by default, or memory-mapped under a region directory when the
//! operator configures persistence. Full-text `query` is BM25-ranked with snippet
//! generation; `regex` and frontmatter property `filters` are applied as a
//! post-filter over the candidate documents (BM25 results when a text query
//! narrows the set, or a bounded full scan otherwise). Frontmatter properties are
//! parsed at index time and stored as JSON for filtering.
//!
//! Persistence carries its own manifest: every document records its source file's
//! mtime and size in the index, so opening a persisted index recovers exactly the
//! stat-diff bookkeeping the engine needs — no sidecar file, and tantivy's atomic
//! commits keep index and manifest impossible to desync. Any failure along the
//! persistent open path (unreadable directory, schema mismatch, held writer lock,
//! unreadable metadata) funnels into one fallback: warn, wipe the index, and start
//! empty, which is just the cold path of a normal build.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, QueryParser};
use tantivy::schema::{FAST, Field, STORED, STRING, Schema, TEXT, TantivyDocument, Value as _};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyError, Term};

use crate::frontmatter;

use super::{
    BackendIndex, CompiledQuery, FilterOp, MAX_SNIPPET_LEN, MAX_SNIPPETS, PropertyFilter, RawHit,
    RecoveredDoc, ScanResult, SourceMeta,
};

/// Writer heap budget; tantivy requires a few MiB minimum.
const WRITER_HEAP: usize = 30_000_000;

/// The schema's field handles, resolved by name so an index opened from disk is
/// read through its own schema rather than through positional assumptions.
struct Fields {
    path: Field,
    path_text: Field,
    body: Field,
    props_json: Field,
    mtime: Field,
    size: Field,
}

/// Build the recall schema. Any change here must bump
/// [`super::persist::INDEX_FORMAT_VERSION`], since persisted indexes carry it.
fn schema() -> Schema {
    let mut builder = Schema::builder();
    // `path`: stored clean virtual path, and the unique key for upsert/delete. Also
    // a fast (columnar) field, so manifest recovery can read every path without
    // touching the document store — see `recover_manifest`.
    builder.add_text_field("path", (STRING | STORED).set_fast(None));
    // `path_text`: the same clean path, tokenized so `query`/`regex` match it
    // like the body (the `path` STRING field stays the exact upsert/delete key).
    builder.add_text_field("path_text", TEXT);
    // `body`: frontmatter-stripped prose, BM25-indexed and stored for snippets.
    builder.add_text_field("body", TEXT | STORED);
    // `props_json`: the serialized frontmatter properties, stored for post-filtering.
    builder.add_text_field("props_json", STORED);
    // `mtime`/`size`: the source file's stat metadata. Never queried — they exist
    // only to rebuild the reconcile manifest when a persisted index is reopened, so
    // they are columnar (`FAST`) rather than stored: recovery then reads three
    // columns instead of decompressing every document's body.
    builder.add_u64_field("mtime", FAST);
    builder.add_u64_field("size", FAST);
    builder.build()
}

impl Fields {
    /// Resolve every field by name; a missing field means a foreign schema.
    fn resolve(schema: &Schema) -> Result<Fields, TantivyError> {
        Ok(Fields {
            path: schema.get_field("path")?,
            path_text: schema.get_field("path_text")?,
            body: schema.get_field("body")?,
            props_json: schema.get_field("props_json")?,
            mtime: schema.get_field("mtime")?,
            size: schema.get_field("size")?,
        })
    }
}

pub(crate) struct TantivyIndex {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    fields: Fields,
}

impl TantivyIndex {
    /// Open a region's index. `None` keeps it in RAM (nothing on disk, no
    /// documents to recover); `Some(dir)` memory-maps it under `dir`, returning the
    /// documents found there so the caller can seed its reconcile manifest.
    pub(crate) fn new(region_dir: Option<PathBuf>) -> (TantivyIndex, Vec<RecoveredDoc>) {
        let Some(dir) = region_dir else {
            return (Self::in_ram(), Vec::new());
        };
        match Self::open_persistent(&dir) {
            Ok(opened) => opened,
            Err(err) => {
                tracing::warn!(
                    dir = %dir.display(),
                    %err,
                    "persisted recall index could not be opened; discarding it and rebuilding from the vault"
                );
                // The one fallback for every persistence failure: wipe, reopen
                // empty, and let the caller's stat-diff index everything.
                let wiped = super::persist::wipe_region_index(&dir)
                    .map_err(TantivyError::from)
                    .and_then(|()| Self::open_persistent(&dir));
                match wiped {
                    Ok((index, _)) => (index, Vec::new()),
                    Err(err) => {
                        tracing::warn!(
                            dir = %dir.display(),
                            %err,
                            "persistent recall index unusable after a rebuild attempt; this region stays in memory"
                        );
                        (Self::in_ram(), Vec::new())
                    }
                }
            }
        }
    }

    /// An index in RAM. Infallible in practice — a `RamDirectory` cannot fail to
    /// open — so the writer/reader construction keeps its `expect`.
    fn in_ram() -> TantivyIndex {
        let schema = schema();
        let index = Index::create_in_ram(schema);
        Self::wrap(index).expect("create the tantivy in-RAM index")
    }

    /// Memory-map an index under `dir`, creating it when absent, and recover the
    /// stored manifest from whatever documents it already holds.
    fn open_persistent(dir: &Path) -> Result<(TantivyIndex, Vec<RecoveredDoc>), TantivyError> {
        std::fs::create_dir_all(dir)?;
        let directory = MmapDirectory::open(dir)?;
        let index = Index::open_or_create(directory, schema())?;
        let opened = Self::wrap(index)?;
        let recovered = opened.recover_manifest()?;
        Ok((opened, recovered))
    }

    /// Attach a writer and a reader to an opened index.
    fn wrap(index: Index) -> Result<TantivyIndex, TantivyError> {
        let fields = Fields::resolve(&index.schema())?;
        let writer = index.writer(WRITER_HEAP)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(TantivyIndex {
            index,
            writer,
            reader,
            fields,
        })
    }

    /// Read `(clean_path, mtime, size)` off every live document, straight from the
    /// columnar fast fields. No note body is decompressed and nothing is
    /// re-tokenized, which is what makes a reopen cheap next to a rebuild — reading
    /// the same three values out of the document store instead costs roughly the
    /// same as re-reading the vault, because the store holds each body alongside
    /// them. A document whose metadata is unreadable is skipped; the caller's
    /// stat-diff then treats that file as changed and re-indexes it.
    fn recover_manifest(&self) -> Result<Vec<RecoveredDoc>, TantivyError> {
        let searcher = self.reader.searcher();
        let mut out = Vec::with_capacity(searcher.num_docs() as usize);
        let mut clean_path = String::new();
        for segment in searcher.segment_readers() {
            let fast = segment.fast_fields();
            // A segment with no documents has no path column at all.
            let Some(paths) = fast.str("path")? else {
                continue;
            };
            let mtimes = fast.u64("mtime")?;
            let sizes = fast.u64("size")?;
            let alive = segment.alive_bitset();
            for doc in 0..segment.max_doc() {
                if alive.is_some_and(|bitset| !bitset.is_alive(doc)) {
                    continue;
                }
                let (Some(ord), Some(mtime), Some(size)) = (
                    paths.term_ords(doc).next(),
                    mtimes.first(doc),
                    sizes.first(doc),
                ) else {
                    continue;
                };
                clean_path.clear();
                if !paths.ord_to_str(ord, &mut clean_path)? {
                    continue;
                }
                out.push(RecoveredDoc {
                    clean_path: clean_path.clone(),
                    meta: SourceMeta {
                        mtime: nanos_to_mtime(mtime),
                        size,
                    },
                });
            }
        }
        Ok(out)
    }

    /// Read the three stored fields off a document.
    fn fields(&self, doc: &TantivyDocument) -> (String, String, serde_json::Value) {
        let path = stored_str(doc, self.fields.path).unwrap_or_default();
        let body = stored_str(doc, self.fields.body).unwrap_or_default();
        let props = stored_str(doc, self.fields.props_json)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        (path, body, props)
    }

    /// Build snippets: the BM25 fragment when available, else matching lines. When
    /// the note matches only on its path (no body fragment or line), the clean path
    /// is emitted as the single snippet, consistent with the simple backend.
    fn snippets(
        &self,
        doc: &TantivyDocument,
        path: &str,
        body: &str,
        compiled: &CompiledQuery,
        generator: Option<&SnippetGenerator>,
    ) -> Vec<String> {
        if let Some(generator) = generator {
            let fragment = generator
                .snippet_from_doc(doc)
                .fragment()
                .trim()
                .to_string();
            if !fragment.is_empty() {
                return vec![truncate(&fragment, MAX_SNIPPET_LEN)];
            }
        }
        let mut out = Vec::new();
        for line in body.lines() {
            if out.len() >= MAX_SNIPPETS {
                break;
            }
            // Only keep lines the query actually matches, so a path-only hit falls
            // through to the path snippet below instead of emitting unrelated body
            // lines (mirrors the simple backend's line selection).
            let matched = match (&compiled.regex, &compiled.substring) {
                (Some(re), _) => re.is_match(line),
                (None, Some(needle)) => line.to_lowercase().contains(needle.as_str()),
                (None, None) => true,
            };
            if matched {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    out.push(truncate(trimmed, MAX_SNIPPET_LEN));
                }
            }
        }
        if out.is_empty() && path_matches(path, compiled) {
            out.push(truncate(path, MAX_SNIPPET_LEN));
        }
        out
    }
}

impl BackendIndex for TantivyIndex {
    fn upsert(&mut self, clean_path: &str, body: &str, meta: SourceMeta) {
        // delete-then-add (the delete carries an earlier opstamp, so the new doc
        // survives the next commit) makes this an upsert keyed by `path`.
        self.writer
            .delete_term(Term::from_field_text(self.fields.path, clean_path));
        let parsed = frontmatter::parse(body);
        let props_json = serde_json::to_string(&parsed.props).unwrap_or_else(|_| "{}".to_string());
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.path, clean_path);
        doc.add_text(self.fields.path_text, clean_path);
        doc.add_text(self.fields.body, &parsed.body);
        doc.add_text(self.fields.props_json, &props_json);
        doc.add_u64(self.fields.mtime, mtime_to_nanos(meta.mtime));
        doc.add_u64(self.fields.size, meta.size);
        let _ = self.writer.add_document(doc);
    }

    fn remove(&mut self, clean_path: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.fields.path, clean_path));
    }

    fn flush(&mut self) {
        if self.writer.commit().is_ok() {
            let _ = self.reader.reload();
        }
    }

    fn query(&self, compiled: &CompiledQuery, byte_cap: usize) -> ScanResult {
        let searcher = self.reader.searcher();
        let mut hits = Vec::new();
        let mut truncated = false;

        if let Some(text) = &compiled.raw_text {
            // BM25 over the narrowed candidate set, then regex/filter post-checks.
            let parser =
                QueryParser::for_index(&self.index, vec![self.fields.body, self.fields.path_text]);
            let query = match parser.parse_query(text).or_else(|_| {
                // Lenient retry as a quoted phrase for inputs with query syntax.
                parser.parse_query(&format!("\"{}\"", text.replace('"', " ")))
            }) {
                Ok(query) => query,
                Err(_) => return ScanResult { hits, truncated },
            };
            let generator = SnippetGenerator::create(&searcher, &*query, self.fields.body).ok();
            let limit = searcher.num_docs().max(1) as usize;
            let top = searcher
                .search(&query, &TopDocs::with_limit(limit).order_by_score())
                .unwrap_or_default();
            for (score, address) in top {
                let Ok(doc) = searcher.doc::<TantivyDocument>(address) else {
                    continue;
                };
                let (path, body, props) = self.fields(&doc);
                if !passes(&path, &body, &props, compiled) {
                    continue;
                }
                let snippets = self.snippets(&doc, &path, &body, compiled, generator.as_ref());
                hits.push(RawHit {
                    clean_path: path,
                    raw_score: score,
                    snippets,
                });
            }
        } else {
            // No text query: a bounded full scan filtered by regex and/or properties.
            let limit = searcher.num_docs().max(1) as usize;
            let all = searcher
                .search(&AllQuery, &TopDocs::with_limit(limit).order_by_score())
                .unwrap_or_default();
            let cap_applies = compiled.regex.is_some();
            let mut scanned = 0usize;
            for (_, address) in all {
                if cap_applies && scanned >= byte_cap {
                    truncated = true;
                    break;
                }
                let Ok(doc) = searcher.doc::<TantivyDocument>(address) else {
                    continue;
                };
                let (path, body, props) = self.fields(&doc);
                scanned = scanned.saturating_add(body.len());
                if !passes(&path, &body, &props, compiled) {
                    continue;
                }
                let raw_score = match &compiled.regex {
                    // Count path and body matches with equal weight.
                    Some(re) => (re.find_iter(&path).count() + re.find_iter(&body).count()) as f32,
                    None => 1.0,
                };
                let snippets = self.snippets(&doc, &path, &body, compiled, None);
                hits.push(RawHit {
                    clean_path: path,
                    raw_score,
                    snippets,
                });
            }
        }

        ScanResult { hits, truncated }
    }
}

/// A document passes when the regex (if any) matches its path or body, and its
/// properties satisfy every filter.
fn passes(path: &str, body: &str, props: &serde_json::Value, compiled: &CompiledQuery) -> bool {
    if let Some(re) = &compiled.regex
        && !re.is_match(body)
        && !re.is_match(path)
    {
        return false;
    }
    compiled.filters.iter().all(|f| eval_filter(props, f))
}

/// Whether the supplied `query`/`regex` matchers match the clean virtual path. Used
/// to decide whether to surface the path as a snippet for a path-only hit; the
/// full-text check mirrors the substring semantics of the simple backend.
fn path_matches(path: &str, compiled: &CompiledQuery) -> bool {
    if let Some(needle) = &compiled.substring
        && path.to_lowercase().contains(needle.as_str())
    {
        return true;
    }
    if let Some(re) = &compiled.regex
        && re.is_match(path)
    {
        return true;
    }
    false
}

/// Evaluate one property predicate against the parsed frontmatter.
fn eval_filter(props: &serde_json::Value, filter: &PropertyFilter) -> bool {
    let value = props.get(&filter.key);
    match filter.op {
        FilterOp::Exists => value.is_some(),
        FilterOp::Eq => value.is_some_and(|v| scalar_eq(v, filter.value.as_deref())),
        FilterOp::Contains => match value {
            Some(serde_json::Value::Array(items)) => {
                items.iter().any(|e| scalar_eq(e, filter.value.as_deref()))
            }
            Some(serde_json::Value::String(s)) => filter
                .value
                .as_deref()
                .is_some_and(|needle| s.contains(needle)),
            _ => false,
        },
        FilterOp::Gt | FilterOp::Lt | FilterOp::Ge | FilterOp::Le => {
            compare(value, filter.value.as_deref(), filter.op)
        }
    }
}

/// Equality between a JSON scalar and the filter's string value.
fn scalar_eq(value: &serde_json::Value, want: Option<&str>) -> bool {
    let Some(want) = want else { return false };
    match value {
        serde_json::Value::String(s) => s == want,
        serde_json::Value::Number(n) => n.to_string() == want,
        serde_json::Value::Bool(b) => b.to_string() == want,
        _ => false,
    }
}

/// Ordered comparison: numeric when both sides parse as numbers, else lexical.
fn compare(value: Option<&serde_json::Value>, want: Option<&str>, op: FilterOp) -> bool {
    let (Some(value), Some(want)) = (value, want) else {
        return false;
    };
    let lhs_str = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => return false,
    };
    let ordering = match (lhs_str.parse::<f64>(), want.parse::<f64>()) {
        (Ok(a), Ok(b)) => a.partial_cmp(&b),
        _ => Some(lhs_str.as_str().cmp(want)),
    };
    match ordering {
        Some(std::cmp::Ordering::Greater) => matches!(op, FilterOp::Gt | FilterOp::Ge),
        Some(std::cmp::Ordering::Less) => matches!(op, FilterOp::Lt | FilterOp::Le),
        Some(std::cmp::Ordering::Equal) => matches!(op, FilterOp::Ge | FilterOp::Le),
        None => false,
    }
}

/// Read a stored text field as a `String`.
fn stored_str(doc: &TantivyDocument, field: Field) -> Option<String> {
    doc.get_first(field)
        .and_then(|value| value.as_str().map(str::to_string))
}

/// Encode a modification time as nanoseconds since the Unix epoch. `SystemTime` on
/// every supported platform is a whole number of nanoseconds, so the round-trip
/// through [`nanos_to_mtime`] reproduces the exact value `fs::metadata` reported —
/// which the reconcile's `==` comparison depends on. A pre-epoch timestamp (only
/// reachable from a deliberately backdated file) collapses to the epoch, and so
/// looks changed on every restart rather than wrongly unchanged.
fn mtime_to_nanos(mtime: SystemTime) -> u64 {
    mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The inverse of [`mtime_to_nanos`].
fn nanos_to_mtime(nanos: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos)
}

/// Truncate to at most `max` bytes on a char boundary, adding an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(
        text: Option<&str>,
        regex: Option<&str>,
        filters: Vec<PropertyFilter>,
    ) -> CompiledQuery {
        CompiledQuery {
            raw_text: text.map(|s| s.to_string()),
            substring: text.map(|s| s.to_lowercase()),
            regex: regex.map(|r| regex::Regex::new(r).unwrap()),
            filters,
        }
    }

    /// An in-RAM index (the default, unpersisted mode).
    fn in_ram() -> TantivyIndex {
        let (idx, recovered) = TantivyIndex::new(None);
        assert!(recovered.is_empty(), "a RAM index has nothing to recover");
        idx
    }

    /// Distinguishable stat metadata for a document.
    fn meta(secs: u64, size: u64) -> SourceMeta {
        SourceMeta {
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            size,
        }
    }

    fn index() -> TantivyIndex {
        let mut idx = in_ram();
        idx.upsert(
            "Agents/topics/rust.md",
            "---\ntags: [rust, systems]\nstatus: published\nweight: 5\n---\nThe borrow checker enforces ownership.",
            meta(1_000, 11),
        );
        idx.upsert(
            "Agents/topics/python.md",
            "---\ntags: [python]\nstatus: draft\nweight: 2\n---\nThe GIL serializes threads.",
            meta(2_000, 22),
        );
        idx.flush();
        idx
    }

    #[test]
    fn bm25_full_text_ranks_and_snippets() {
        let idx = index();
        let scan = idx.query(&compiled(Some("borrow"), None, vec![]), usize::MAX);
        assert_eq!(scan.hits.len(), 1);
        assert_eq!(scan.hits[0].clean_path, "Agents/topics/rust.md");
        assert!(scan.hits[0].raw_score > 0.0);
        assert!(!scan.hits[0].snippets.is_empty());
        // Frontmatter is stripped from the indexed body: a property word is not prose.
        let none = idx.query(&compiled(Some("published"), None, vec![]), usize::MAX);
        assert!(none.hits.is_empty());
    }

    #[test]
    fn property_filter_eq_and_contains() {
        let idx = index();
        let eq = idx.query(
            &compiled(
                None,
                None,
                vec![PropertyFilter {
                    key: "status".into(),
                    op: FilterOp::Eq,
                    value: Some("draft".into()),
                }],
            ),
            usize::MAX,
        );
        assert_eq!(eq.hits.len(), 1);
        assert_eq!(eq.hits[0].clean_path, "Agents/topics/python.md");

        let contains = idx.query(
            &compiled(
                None,
                None,
                vec![PropertyFilter {
                    key: "tags".into(),
                    op: FilterOp::Contains,
                    value: Some("systems".into()),
                }],
            ),
            usize::MAX,
        );
        assert_eq!(contains.hits.len(), 1);
        assert_eq!(contains.hits[0].clean_path, "Agents/topics/rust.md");
    }

    #[test]
    fn property_filter_numeric_comparison() {
        let idx = index();
        let scan = idx.query(
            &compiled(
                None,
                None,
                vec![PropertyFilter {
                    key: "weight".into(),
                    op: FilterOp::Gt,
                    value: Some("3".into()),
                }],
            ),
            usize::MAX,
        );
        assert_eq!(scan.hits.len(), 1);
        assert_eq!(scan.hits[0].clean_path, "Agents/topics/rust.md");
    }

    #[test]
    fn text_plus_filter_compose() {
        let idx = index();
        let scan = idx.query(
            &compiled(
                Some("threads"),
                None,
                vec![PropertyFilter {
                    key: "status".into(),
                    op: FilterOp::Eq,
                    value: Some("published".into()),
                }],
            ),
            usize::MAX,
        );
        // "threads" matches python (draft), but the published filter excludes it.
        assert!(scan.hits.is_empty());
    }

    #[test]
    fn regex_over_candidates() {
        let idx = index();
        let scan = idx.query(&compiled(None, Some(r"\bGIL\b"), vec![]), usize::MAX);
        assert_eq!(scan.hits.len(), 1);
        assert_eq!(scan.hits[0].clean_path, "Agents/topics/python.md");
    }

    #[test]
    fn regex_matches_path_when_body_does_not() {
        let mut idx = in_ram();
        idx.upsert(
            "Agents/diary/2026-06-10.md",
            "Nothing dated in the body.",
            meta(1_000, 26),
        );
        idx.flush();
        let scan = idx.query(&compiled(None, Some(r"2026-06-10"), vec![]), usize::MAX);
        assert_eq!(scan.hits.len(), 1);
        assert_eq!(scan.hits[0].clean_path, "Agents/diary/2026-06-10.md");
        assert_eq!(scan.hits[0].snippets, vec!["Agents/diary/2026-06-10.md"]);
    }

    #[test]
    fn full_text_matches_path_when_body_does_not() {
        let mut idx = in_ram();
        // "kotlin" appears only in the path, not the body.
        idx.upsert(
            "Agents/topics/kotlin.md",
            "Coroutines structure concurrency.",
            meta(1_000, 33),
        );
        idx.flush();
        let scan = idx.query(&compiled(Some("kotlin"), None, vec![]), usize::MAX);
        assert_eq!(scan.hits.len(), 1);
        assert_eq!(scan.hits[0].clean_path, "Agents/topics/kotlin.md");
        assert_eq!(scan.hits[0].snippets, vec!["Agents/topics/kotlin.md"]);
    }

    #[test]
    fn remove_then_flush_drops_doc() {
        let mut idx = index();
        idx.remove("Agents/topics/rust.md");
        idx.flush();
        let scan = idx.query(&compiled(Some("borrow"), None, vec![]), usize::MAX);
        assert!(scan.hits.is_empty());
    }

    /// The whole persistence scheme rests on this: the mtime a stat reports must
    /// survive the trip through the stored field bit-for-bit, or the reconcile's
    /// equality check calls every file changed and each restart re-indexes the
    /// entire vault.
    #[test]
    fn a_real_files_mtime_round_trips_through_the_stored_field() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let file = tmp.path().join("note.md");
        std::fs::write(&file, b"body").unwrap();
        let stat = std::fs::metadata(&file).unwrap().modified().unwrap();
        assert_eq!(nanos_to_mtime(mtime_to_nanos(stat)), stat);
        // Sub-second precision is not silently dropped.
        assert_ne!(mtime_to_nanos(stat) % 1_000_000_000, u64::MAX);
    }

    /// A persisted index reopened in a fresh process hands back exactly the
    /// manifest it was written with — the metadata is committed together with the
    /// documents, so the two cannot desync.
    #[test]
    fn reopening_a_persisted_index_recovers_the_manifest_and_the_documents() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let dir = tmp.path().join("region");
        let file = tmp.path().join("note.md");
        std::fs::write(&file, b"The borrow checker enforces ownership.").unwrap();
        let stat = std::fs::metadata(&file).unwrap();
        let written = SourceMeta {
            mtime: stat.modified().unwrap(),
            size: stat.len(),
        };

        {
            let (mut idx, recovered) = TantivyIndex::new(Some(dir.clone()));
            assert!(recovered.is_empty(), "a fresh directory holds no documents");
            idx.upsert(
                "Agents/topics/rust.md",
                "The borrow checker enforces ownership.",
                written,
            );
            idx.flush();
        }

        let (idx, recovered) = TantivyIndex::new(Some(dir));
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].clean_path, "Agents/topics/rust.md");
        assert_eq!(recovered[0].meta.mtime, written.mtime);
        assert_eq!(recovered[0].meta.size, written.size);
        // The documents themselves came back too, queryable without re-indexing.
        let scan = idx.query(&compiled(Some("borrow"), None, vec![]), usize::MAX);
        assert_eq!(scan.hits.len(), 1);
        assert_eq!(scan.hits[0].clean_path, "Agents/topics/rust.md");
    }
}
