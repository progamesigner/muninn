//! Content recall.
//!
//! Recall is backed by per-scope indexes plus a single shared-region index. A
//! scope's notes live only in that scope's index, so a query opens only the
//! caller's own-scope index and (policy permitting) the shared index: cross-scope
//! recall is *structurally* impossible, not filtered. Indexes are built eagerly at
//! startup, updated synchronously on the server's own writes, and reconciled
//! against external edits by a stat-diff that a filesystem watcher (and a freshness
//! window) trigger.
//!
//! Indexes are held in memory and nothing is written to disk unless the operator
//! sets `MUNINN_RECALL_INDEX_DIR` under the tantivy backend. With persistence on,
//! each region's index is memory-mapped under that directory and its reconcile
//! manifest is recovered from the index itself on open (see [`persist`] and the
//! tantivy backend), so a restart re-reads only what changed while the server was
//! down instead of the whole vault.
//!
//! The backend is configurable. The default [`SimpleIndex`] supports
//! case-insensitive substring and regex matching; the opt-in tantivy backend (the
//! `recall-tantivy` feature) adds BM25 ranking and frontmatter property filters.
//! Scores from the scope and shared indexes are normalized to 0–1 per index before
//! merging, so the agent-facing score is comparable across the two corpora.

#[cfg(feature = "recall-tantivy")]
mod persist;
mod simple;
#[cfg(feature = "recall-tantivy")]
mod tantivy;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use crate::config::{RecallBackendKind, RecallConfig};
use crate::error::MuninnError;
use crate::path::PhysicalPath;
use crate::policy::Region;
use crate::storage::{Cursor, Storage};

/// The maximum number of snippets returned per hit.
pub(crate) const MAX_SNIPPETS: usize = 3;
/// The maximum length, in bytes, of a single snippet before truncation.
pub(crate) const MAX_SNIPPET_LEN: usize = 200;

// --- public request / response types ---

/// A frontmatter property predicate (honoured by the tantivy backend only).
#[derive(Debug, Clone)]
pub struct PropertyFilter {
    pub key: String,
    pub op: FilterOp,
    pub value: Option<String>,
}

/// The comparison a [`PropertyFilter`] applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Exists,
    Eq,
    Contains,
    Gt,
    Lt,
    Ge,
    Le,
}

/// A parsed recall request (after scope extraction and argument validation).
#[derive(Debug, Clone)]
pub struct RecallQuery {
    /// Full-text query (substring on `simple`, BM25 on tantivy).
    pub text: Option<String>,
    /// Regular-expression query over note content.
    pub regex: Option<String>,
    /// Frontmatter property predicates (tantivy backend only).
    pub filters: Vec<PropertyFilter>,
    /// Clean-path prefix relative to the agents folder, mirroring `list_memory_notes`.
    pub path_prefix: Option<String>,
    /// Page size (already defaulted/capped by the caller).
    pub limit: u64,
    /// Pagination offset, decoded from an opaque cursor.
    pub offset: u64,
    /// Include only notes whose mtime is at or after this instant (half-open:
    /// `modified_after ≤ mtime < modified_before`).
    pub modified_after: Option<SystemTime>,
    /// Include only notes whose mtime is strictly before this instant.
    pub modified_before: Option<SystemTime>,
}

/// One ranked recall hit returned to the agent.
#[derive(Debug, Clone)]
pub struct RecallHit {
    pub path: String,
    pub score: f32,
    pub snippets: Vec<String>,
    /// The note's last modification time (RFC 3339, UTC), sourced from the
    /// index manifest; `None` when the entry vanished between scan and merge.
    pub modified_at: Option<String>,
}

/// A page of recall hits.
#[derive(Debug, Clone)]
pub struct RecallResults {
    pub hits: Vec<RecallHit>,
    pub next_cursor: Option<String>,
    /// `true` when a regex scan hit the byte cap and stopped early.
    pub truncated: bool,
}

// --- backend abstraction ---

/// A raw, un-normalized hit from a single backend index.
pub(crate) struct RawHit {
    pub clean_path: String,
    pub raw_score: f32,
    pub snippets: Vec<String>,
}

/// The outcome of scanning one index.
pub(crate) struct ScanResult {
    pub hits: Vec<RawHit>,
    pub truncated: bool,
}

/// A compiled query handed to a backend index. Some fields are read only by the
/// feature-gated tantivy backend.
#[cfg_attr(not(feature = "recall-tantivy"), allow(dead_code))]
pub(crate) struct CompiledQuery {
    /// The raw full-text query (used for BM25 on the tantivy backend).
    pub raw_text: Option<String>,
    /// Lower-cased substring needle (used for matching on the simple backend).
    pub substring: Option<String>,
    /// Compiled regex, if a `regex` query was supplied.
    pub regex: Option<regex::Regex>,
    /// Frontmatter property predicates (applied by the tantivy backend only).
    pub filters: Vec<PropertyFilter>,
}

/// The source file's stat metadata, handed to the backend alongside the body. A
/// disk-backed backend stores it so the manifest can be recovered when the index is
/// reopened; in-memory backends ignore it.
#[cfg_attr(not(feature = "recall-tantivy"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceMeta {
    pub mtime: SystemTime,
    pub size: u64,
}

/// One document read back out of a persisted index, used to seed a region's
/// reconcile manifest before its first stat-diff.
#[cfg_attr(not(feature = "recall-tantivy"), allow(dead_code))]
pub(crate) struct RecoveredDoc {
    pub clean_path: String,
    pub meta: SourceMeta,
}

/// One backend index over a single region's notes.
pub(crate) trait BackendIndex: Send {
    fn upsert(&mut self, clean_path: &str, body: &str, meta: SourceMeta);
    fn remove(&mut self, clean_path: &str);
    fn query(&self, query: &CompiledQuery, byte_cap: usize) -> ScanResult;
    /// Persist a batch of upserts/removals. A no-op for backends that mutate in
    /// place; the tantivy backend commits and reloads its reader here.
    fn flush(&mut self) {}
}

// --- per-region index state ---

/// Per-file metadata used for stat-diff reconciliation.
struct FileMeta {
    clean_path: String,
    mtime: SystemTime,
    size: u64,
}

/// Which region an index covers.
#[derive(Clone)]
enum IndexRegion {
    /// A per-scope index inside the agents folder, keyed by its rendered scope.
    Scoped(String),
    /// The single shared-region index outside the agents folder.
    Shared,
}

/// One resident in-memory index plus its reconciliation bookkeeping.
struct RegionIndex {
    region: IndexRegion,
    manifest: BTreeMap<PathBuf, FileMeta>,
    backend: Box<dyn BackendIndex>,
    last_reconcile: Option<Instant>,
    last_access: Instant,
}

/// The mutable engine state behind a single lock.
struct EngineState {
    built: bool,
    shared: Option<RegionIndex>,
    scopes: HashMap<String, RegionIndex>,
}

/// The recall engine. Holds the in-memory indexes and serves queries; shared
/// behind the `Toolbox`'s `Arc`. Construction yields `None` when recall is `off`.
pub struct RecallEngine {
    effective: RecallBackendKind,
    storage: std::sync::Arc<Storage>,
    config: RecallConfig,
    state: Mutex<EngineState>,
    /// Set true once the eager build has completed; read by `GET /readyz`.
    ready: AtomicBool,
    /// Set by the filesystem watcher; forces the next query to reconcile.
    dirty: std::sync::Arc<AtomicBool>,
    /// The live watcher; kept alive for the engine's lifetime.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// The persisted-index root, when the operator configured one and the
    /// effective backend can use it.
    #[cfg(feature = "recall-tantivy")]
    persist: Option<persist::PersistRoot>,
    /// How many note bodies have been read and handed to a backend since
    /// construction. Backs [`RecallEngine::ingested_count`].
    ingested: AtomicU64,
}

impl RecallEngine {
    /// Build an engine for the configured backend, or `None` when recall is `off`.
    /// Resolves the effective backend against the `recall-tantivy` feature, logging
    /// a fallback to `simple` when tantivy was requested but is unavailable.
    pub fn new(storage: std::sync::Arc<Storage>, config: RecallConfig) -> Option<RecallEngine> {
        let effective = match config.backend {
            RecallBackendKind::Off => {
                if config.index_dir.is_some() {
                    tracing::warn!(
                        "MUNINN_RECALL_INDEX_DIR is set but recall is disabled \
                         (MUNINN_RECALL_BACKEND=off); ignoring it"
                    );
                }
                return None;
            }
            RecallBackendKind::Simple => RecallBackendKind::Simple,
            RecallBackendKind::Tantivy => {
                #[cfg(feature = "recall-tantivy")]
                {
                    RecallBackendKind::Tantivy
                }
                #[cfg(not(feature = "recall-tantivy"))]
                {
                    tracing::warn!(
                        "MUNINN_RECALL_BACKEND=tantivy but the binary was built without the \
                         'recall-tantivy' feature; falling back to the simple backend"
                    );
                    RecallBackendKind::Simple
                }
            }
        };
        // Persistence is a tantivy-only capability: the simple backend has no
        // on-disk representation, so the setting is ignored with a warning (the
        // same place the feature-fallback warning above is issued).
        if config.index_dir.is_some() && !matches!(effective, RecallBackendKind::Tantivy) {
            tracing::warn!(
                backend = effective.as_str(),
                "MUNINN_RECALL_INDEX_DIR is set but the effective recall backend is not tantivy; \
                 ignoring it and holding indexes in memory"
            );
        }
        #[cfg(feature = "recall-tantivy")]
        let persist = match (&config.index_dir, effective) {
            (Some(dir), RecallBackendKind::Tantivy) => {
                Some(persist::PersistRoot::new(dir, &storage))
            }
            _ => None,
        };
        Some(RecallEngine {
            effective,
            storage,
            config,
            state: Mutex::new(EngineState {
                built: false,
                shared: None,
                scopes: HashMap::new(),
            }),
            ready: AtomicBool::new(false),
            dirty: std::sync::Arc::new(AtomicBool::new(false)),
            watcher: Mutex::new(None),
            #[cfg(feature = "recall-tantivy")]
            persist,
            ingested: AtomicU64::new(0),
        })
    }

    /// The effective backend after feature resolution.
    pub fn effective_backend(&self) -> RecallBackendKind {
        self.effective
    }

    /// Whether the effective backend can apply frontmatter property filters.
    pub fn supports_property_filters(&self) -> bool {
        cfg!(feature = "recall-tantivy") && matches!(self.effective, RecallBackendKind::Tantivy)
    }

    /// `true` once the eager startup build has completed. Backs `GET /readyz`.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// The number of per-scope indexes currently resident in memory. Backs the
    /// eviction-bound tests and benchmarks that verify `max_resident_scopes`.
    pub fn resident_scope_count(&self) -> usize {
        let state = self.state.lock().expect("recall state poisoned");
        state.scopes.len()
    }

    /// How many note bodies have been read and ingested since construction — the
    /// count a persisted index exists to keep near zero across restarts. Backs the
    /// persistence tests and benchmarks.
    pub fn ingested_count(&self) -> u64 {
        self.ingested.load(Ordering::Acquire)
    }

    /// Eagerly build every scope index and the shared index, then mark ready. Safe
    /// to call repeatedly; the build runs once. This is also the block-until-ready
    /// path: a query arriving before the background build finishes takes the lock
    /// and builds inline.
    pub fn warm(&self) {
        let mut state = self.state.lock().expect("recall state poisoned");
        self.ensure_built(&mut state);
    }

    fn ensure_built(&self, state: &mut EngineState) {
        if state.built {
            return;
        }
        // Timed here rather than in `warm()` so the inline build-on-first-query
        // path is measured identically to the background warmup, and so repeated
        // calls after the build stay silent.
        let started = Instant::now();
        tracing::info!(
            backend = self.effective.as_str(),
            "recall index build started"
        );
        // Shared region (absent when the agents folder is the vault root).
        if !self.storage.resolver().agents_dir().as_str().is_empty() {
            let mut idx = self.new_region_index(IndexRegion::Shared);
            self.reconcile(&mut idx);
            state.shared = Some(idx);
        }
        // Every scope directory present on disk.
        let mut scope_dirs = self.storage.list_scope_dirs();
        // Single-tenant (empty scheme): one index under the empty rendered scope.
        if self.storage.resolver().scheme().is_empty() {
            scope_dirs.push(String::new());
        }
        for scope in scope_dirs {
            let mut idx = self.new_region_index(IndexRegion::Scoped(scope.clone()));
            self.reconcile(&mut idx);
            state.scopes.insert(scope, idx);
        }
        state.built = true;
        self.ready.store(true, Ordering::Release);
        tracing::info!(
            backend = self.effective.as_str(),
            scopes = state.scopes.len(),
            elapsed = ?started.elapsed(),
            "recall index ready"
        );
    }

    /// Start the filesystem watcher: any change under the vault root marks the
    /// engine dirty, so the next query reconciles. Idempotent.
    pub fn start_watcher(&self) {
        use notify::{RecursiveMode, Watcher};
        let mut guard = self.watcher.lock().expect("recall watcher poisoned");
        if guard.is_some() {
            return;
        }
        let dirty = self.dirty.clone();
        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<_>| {
            if res.is_ok() {
                dirty.store(true, Ordering::Release);
            }
        }) {
            Ok(w) => w,
            Err(err) => {
                tracing::warn!(%err, "recall filesystem watcher unavailable; relying on the freshness reconcile");
                return;
            }
        };
        let root = self.storage.resolver().vault_root().to_path_buf();
        if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
            tracing::warn!(%err, "recall watcher could not watch the vault root");
            return;
        }
        *guard = Some(watcher);
    }

    /// Incrementally update the index after the server's own write to `physical`.
    /// A no-op when the owning index is not currently resident (it will pick the
    /// change up on its next build).
    pub fn on_write(&self, rendered_scope: &str, region: Region, physical: &PhysicalPath) {
        let mut state = self.state.lock().expect("recall state poisoned");
        if !state.built {
            // Not built yet: the eager build will read the new content.
            return;
        }
        let idx = match region {
            Region::OutsideAgentsFolder => state.shared.as_mut(),
            Region::InsideAgentsFolder => state.scopes.get_mut(rendered_scope),
        };
        if let Some(idx) = idx {
            self.apply_path(idx, physical);
        }
    }

    /// Run a recall query for the caller's scope across the permitted regions.
    pub fn recall(
        &self,
        rendered_scope: &str,
        regions: &[Region],
        query: &RecallQuery,
    ) -> Result<RecallResults, MuninnError> {
        if !query.filters.is_empty() && !self.supports_property_filters() {
            return Err(MuninnError::Unsupported {
                message: "frontmatter property filters require the tantivy backend \
                          (build with --features recall-tantivy and set \
                          MUNINN_RECALL_BACKEND=tantivy)"
                    .to_string(),
            });
        }
        let compiled = compile_query(query)?;
        // A query with no content predicate is answered from the manifests alone
        // (a time-only query); no backend scan runs.
        let has_content =
            compiled.raw_text.is_some() || compiled.regex.is_some() || !compiled.filters.is_empty();
        let time_bounded = query.modified_after.is_some() || query.modified_before.is_some();

        let mut state = self.state.lock().expect("recall state poisoned");
        self.ensure_built(&mut state);
        let force = self.dirty.swap(false, Ordering::AcqRel);

        let include_shared = regions.contains(&Region::OutsideAgentsFolder);
        let include_scope = regions.contains(&Region::InsideAgentsFolder);

        let mut merged: Vec<(f32, RawHit)> = Vec::new();
        let mut truncated = false;
        // clean_path → mtime from the opened manifests, backing the time bounds
        // and each hit's `modified_at` — no filesystem stats on the query path.
        let mut mtimes: HashMap<String, SystemTime> = HashMap::new();

        if include_scope {
            self.ensure_scope_resident(&mut state, rendered_scope);
            if let Some(idx) = state.scopes.get_mut(rendered_scope) {
                self.refresh(idx, force);
                idx.last_access = Instant::now();
                if has_content {
                    let scan = idx
                        .backend
                        .query(&compiled, self.config.regex_scan_byte_cap);
                    truncated |= scan.truncated;
                    push_normalized(&mut merged, scan.hits);
                }
                for meta in idx.manifest.values() {
                    mtimes.insert(meta.clean_path.clone(), meta.mtime);
                }
            }
        }
        if include_shared && let Some(idx) = state.shared.as_mut() {
            self.refresh(idx, force);
            if has_content {
                let scan = idx
                    .backend
                    .query(&compiled, self.config.regex_scan_byte_cap);
                truncated |= scan.truncated;
                push_normalized(&mut merged, scan.hits);
            }
            for meta in idx.manifest.values() {
                mtimes.insert(meta.clean_path.clone(), meta.mtime);
            }
        }
        drop(state);

        self.evict_if_needed();

        if !has_content {
            // Time-only: every manifest entry inside the bounds is a hit, with a
            // uniform score and no snippets (nothing was matched to excerpt).
            for (clean, mtime) in &mtimes {
                if within_time_bounds(query, *mtime) {
                    merged.push((
                        1.0,
                        RawHit {
                            clean_path: clean.clone(),
                            raw_score: 1.0,
                            snippets: Vec::new(),
                        },
                    ));
                }
            }
        } else if time_bounded {
            // Content hits: the time bounds filter the merged set. A hit whose
            // manifest entry vanished mid-query has no provable mtime and is
            // dropped.
            merged.retain(|(_, h)| {
                mtimes
                    .get(&h.clean_path)
                    .is_some_and(|mtime| within_time_bounds(query, *mtime))
            });
        }

        // Path-prefix filter, mirroring list_memory_notes (prefix relative to the
        // agents folder).
        if let Some(prefix) = &query.path_prefix {
            let agents = self.storage.resolver().agents_dir();
            let effective = if agents.as_str().is_empty() {
                prefix.clone()
            } else {
                format!("{agents}/{prefix}")
            };
            let with_sep = format!("{effective}/");
            merged
                .retain(|(_, h)| h.clean_path == effective || h.clean_path.starts_with(&with_sep));
        }

        if has_content {
            // Sort by normalized score (desc), then path (asc) for stable ordering.
            merged.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.clean_path.cmp(&b.1.clean_path))
            });
        } else {
            // Time-only: most recently modified first, then path (asc).
            merged.sort_by(|a, b| {
                mtimes
                    .get(&b.1.clean_path)
                    .cmp(&mtimes.get(&a.1.clean_path))
                    .then_with(|| a.1.clean_path.cmp(&b.1.clean_path))
            });
        }

        let total = merged.len() as u64;
        let start = query.offset.min(total) as usize;
        let end = (query.offset + query.limit).min(total) as usize;
        let next_cursor = if (end as u64) < total {
            Some(Cursor::encode(end as u64))
        } else {
            None
        };

        let hits = merged[start..end]
            .iter()
            .map(|(score, h)| RecallHit {
                path: h.clean_path.clone(),
                score: *score,
                // Snippets are clean by construction: ingestion strips the
                // scope's own suffixes (see `read_for_index`).
                snippets: h.snippets.clone(),
                modified_at: mtimes.get(&h.clean_path).map(|m| format_modified_at(*m)),
            })
            .collect();

        Ok(RecallResults {
            hits,
            next_cursor,
            truncated,
        })
    }

    // --- index construction / reconciliation ---

    /// Open (or create) a region's index. With persistence configured this reopens
    /// the region's on-disk index and seeds the manifest from the metadata stored
    /// in it, so the first reconcile is a stat-diff against real bookkeeping rather
    /// than a full rebuild.
    fn new_region_index(&self, region: IndexRegion) -> RegionIndex {
        // Only the tantivy arm can seed a manifest (persistence is tantivy-only).
        #[cfg_attr(not(feature = "recall-tantivy"), allow(unused_mut))]
        let mut manifest = BTreeMap::new();
        let backend: Box<dyn BackendIndex> = match self.effective {
            RecallBackendKind::Simple => Box::new(simple::SimpleIndex::default()),
            #[cfg(feature = "recall-tantivy")]
            RecallBackendKind::Tantivy => {
                let dir = self.persist.as_ref().and_then(|p| p.region_dir(&region));
                let (index, recovered) = tantivy::TantivyIndex::new(dir);
                manifest = self.seed_manifest(&region, recovered);
                Box::new(index)
            }
            // Without the feature, `new` never resolves the effective backend to
            // Tantivy, so this arm is unreachable.
            #[cfg(not(feature = "recall-tantivy"))]
            RecallBackendKind::Tantivy => Box::new(simple::SimpleIndex::default()),
            RecallBackendKind::Off => unreachable!("engine is None when recall is off"),
        };
        RegionIndex {
            region,
            manifest,
            backend,
            last_reconcile: None,
            last_access: Instant::now(),
        }
    }

    /// Turn documents recovered from a persisted index into a reconcile manifest.
    /// The manifest is keyed by physical path, so each recovered clean path is run
    /// back through the resolver — the same mapping `current_files` applies — and an
    /// entry that no longer resolves is dropped (the file is then simply absent from
    /// the diff's "unchanged" set and gets re-indexed or removed).
    #[cfg(feature = "recall-tantivy")]
    fn seed_manifest(
        &self,
        region: &IndexRegion,
        recovered: Vec<RecoveredDoc>,
    ) -> BTreeMap<PathBuf, FileMeta> {
        let resolver = self.storage.resolver();
        let scope = match region {
            IndexRegion::Shared => "",
            IndexRegion::Scoped(scope) => scope.as_str(),
        };
        let mut manifest = BTreeMap::new();
        for doc in recovered {
            let Ok(vpath) = crate::path::VirtualPath::new(&doc.clean_path) else {
                continue;
            };
            let Ok(physical) = resolver.resolve(scope, &vpath) else {
                continue;
            };
            manifest.insert(
                physical.as_path().to_path_buf(),
                FileMeta {
                    clean_path: doc.clean_path,
                    mtime: doc.meta.mtime,
                    size: doc.meta.size,
                },
            );
        }
        manifest
    }

    /// Reconcile if the index is stale or the engine was marked dirty.
    fn refresh(&self, idx: &mut RegionIndex, force: bool) {
        let stale = match idx.last_reconcile {
            None => true,
            Some(t) => t.elapsed() >= self.config.freshness,
        };
        if force || stale {
            reconcile_with(idx, &self.storage, &self.ingested);
        }
    }

    fn reconcile(&self, idx: &mut RegionIndex) {
        reconcile_with(idx, &self.storage, &self.ingested);
    }

    /// Re-read and upsert a single physical path into `idx` (or remove it if gone).
    fn apply_path(&self, idx: &mut RegionIndex, physical: &PhysicalPath) {
        apply_path_with(idx, physical, &self.storage, &self.ingested);
    }

    /// Build a scope index on demand when it was never built or was evicted.
    fn ensure_scope_resident(&self, state: &mut EngineState, rendered_scope: &str) {
        if state.scopes.contains_key(rendered_scope) {
            return;
        }
        let mut idx = self.new_region_index(IndexRegion::Scoped(rendered_scope.to_string()));
        self.reconcile(&mut idx);
        state.scopes.insert(rendered_scope.to_string(), idx);
    }

    /// Evict the least-recently-accessed scope indexes beyond the resident cap.
    fn evict_if_needed(&self) {
        let cap = self.config.max_resident_scopes.max(1);
        let mut state = self.state.lock().expect("recall state poisoned");
        while state.scopes.len() > cap {
            let victim = state
                .scopes
                .iter()
                .min_by_key(|(_, idx)| idx.last_access)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    // Commit before dropping: a disk-backed index must not lose
                    // uncommitted work, since the next access reopens from disk.
                    if let Some(mut idx) = state.scopes.remove(&k) {
                        idx.backend.flush();
                    }
                }
                None => break,
            }
        }
    }
}

/// Read a note body the way the index's region should see it. A scoped index
/// ingests the agent-facing view — the scope's own link suffixes stripped,
/// identical to the `read_memory_note` transform — so full-text, regex,
/// property filters, and snippets all evaluate against clean content. The
/// shared index ingests verbatim: the cross-scope leak guard guarantees shared
/// files carry no scope suffix. Every ingestion path (warm build, watcher and
/// stat-diff reconcile, eviction rebuild, synchronous own-write) reads through
/// this single funnel.
fn read_for_index(
    idx: &RegionIndex,
    physical: &PhysicalPath,
    storage: &Storage,
) -> Result<String, MuninnError> {
    let body = storage.read(physical)?;
    Ok(match &idx.region {
        IndexRegion::Scoped(scope) => {
            crate::wikilink::strip_links(&body, scope, storage.resolver())
        }
        IndexRegion::Shared => body,
    })
}

/// Gather the current visible files for an index's region as `(physical, clean)`.
fn current_files(idx: &RegionIndex, storage: &Storage) -> Vec<(PhysicalPath, String)> {
    let resolver = storage.resolver();
    let clean_paths = match &idx.region {
        IndexRegion::Shared => storage.list_outside_agents_folder().unwrap_or_default(),
        IndexRegion::Scoped(scope) => storage.list_inside_agents_folder(scope).unwrap_or_default(),
    };
    let scope_for_resolve = match &idx.region {
        IndexRegion::Shared => "",
        IndexRegion::Scoped(scope) => scope.as_str(),
    };
    let mut out = Vec::new();
    for vpath in clean_paths {
        if let Ok(physical) = resolver.resolve(scope_for_resolve, &vpath) {
            out.push((physical, vpath.as_str().to_string()));
        }
    }
    out
}

/// Stat-diff reconcile: upsert new/changed files, drop deleted ones. `ingested`
/// counts the bodies actually read, so a restart over a persisted index can be
/// asserted to read nothing.
fn reconcile_with(idx: &mut RegionIndex, storage: &Storage, ingested: &AtomicU64) {
    let current = current_files(idx, storage);
    let mut seen: BTreeMap<PathBuf, ()> = BTreeMap::new();

    for (physical, clean) in &current {
        let key = physical.as_path().to_path_buf();
        seen.insert(key.clone(), ());
        let meta = std::fs::metadata(physical.as_path());
        let (mtime, size) = match meta {
            Ok(m) => (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()),
            Err(_) => continue,
        };
        let unchanged = idx
            .manifest
            .get(&key)
            .is_some_and(|prev| prev.mtime == mtime && prev.size == size);
        if unchanged {
            continue;
        }
        if let Ok(body) = read_for_index(idx, physical, storage) {
            ingested.fetch_add(1, Ordering::AcqRel);
            idx.backend.upsert(clean, &body, SourceMeta { mtime, size });
            idx.manifest.insert(
                key,
                FileMeta {
                    clean_path: clean.clone(),
                    mtime,
                    size,
                },
            );
        }
    }

    // Drop files that vanished.
    let removed: Vec<PathBuf> = idx
        .manifest
        .keys()
        .filter(|k| !seen.contains_key(*k))
        .cloned()
        .collect();
    for key in removed {
        if let Some(meta) = idx.manifest.remove(&key) {
            idx.backend.remove(&meta.clean_path);
        }
    }

    idx.backend.flush();
    idx.last_reconcile = Some(Instant::now());
}

/// Upsert or remove a single physical path (used by the synchronous own-write path).
fn apply_path_with(
    idx: &mut RegionIndex,
    physical: &PhysicalPath,
    storage: &Storage,
    ingested: &AtomicU64,
) {
    let key = physical.as_path().to_path_buf();
    match std::fs::metadata(physical.as_path()) {
        Ok(m) => {
            let mtime = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let size = m.len();
            // Recompute the clean path for the manifest entry.
            let clean = idx
                .manifest
                .get(&key)
                .map(|meta| meta.clean_path.clone())
                .or_else(|| clean_path_for(idx, physical, storage));
            if let (Some(clean), Ok(body)) = (clean, read_for_index(idx, physical, storage)) {
                ingested.fetch_add(1, Ordering::AcqRel);
                idx.backend
                    .upsert(&clean, &body, SourceMeta { mtime, size });
                idx.manifest.insert(
                    key,
                    FileMeta {
                        clean_path: clean,
                        mtime,
                        size,
                    },
                );
            }
        }
        Err(_) => {
            if let Some(meta) = idx.manifest.remove(&key) {
                idx.backend.remove(&meta.clean_path);
            }
        }
    }
    idx.backend.flush();
}

/// Derive the clean virtual path of a physical file for the given index region.
fn clean_path_for(idx: &RegionIndex, physical: &PhysicalPath, storage: &Storage) -> Option<String> {
    let resolver = storage.resolver();
    match &idx.region {
        IndexRegion::Scoped(scope) => resolver
            .strip_suffix(physical.as_path(), scope)
            .map(|v| v.as_str().to_string()),
        IndexRegion::Shared => {
            let rel = physical
                .as_path()
                .strip_prefix(resolver.vault_root())
                .ok()?;
            camino::Utf8Path::from_path(rel).map(|p| p.as_str().to_string())
        }
    }
}

/// Compile a query for the backends, validating the regex.
fn compile_query(query: &RecallQuery) -> Result<CompiledQuery, MuninnError> {
    let raw_text = query.text.as_ref().filter(|s| !s.is_empty()).cloned();
    let substring = raw_text.as_ref().map(|s| s.to_lowercase());
    let regex = match query.regex.as_ref().filter(|s| !s.is_empty()) {
        Some(pattern) => {
            Some(
                regex::Regex::new(pattern).map_err(|e| MuninnError::InvalidArgument {
                    message: format!("invalid regex: {e}"),
                })?,
            )
        }
        None => None,
    };
    Ok(CompiledQuery {
        raw_text,
        substring,
        regex,
        filters: query.filters.clone(),
    })
}

/// Half-open time-bound check: `modified_after ≤ mtime < modified_before`.
fn within_time_bounds(query: &RecallQuery, mtime: SystemTime) -> bool {
    query.modified_after.is_none_or(|after| mtime >= after)
        && query.modified_before.is_none_or(|before| mtime < before)
}

/// Format a manifest mtime as an RFC 3339 UTC timestamp.
fn format_modified_at(mtime: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(mtime)
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
}

/// Normalize one index's raw scores to 0–1 (by its own max) and append to `merged`.
fn push_normalized(merged: &mut Vec<(f32, RawHit)>, hits: Vec<RawHit>) {
    let max = hits.iter().map(|h| h.raw_score).fold(0.0_f32, f32::max);
    for hit in hits {
        let score = if max > 0.0 { hit.raw_score / max } else { 1.0 };
        merged.push((score, hit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::PathResolver;
    use crate::scheme::Scheme;
    use std::sync::Arc;

    use assert_fs::TempDir;
    use assert_fs::prelude::*;

    const BOTH: &[Region] = &[Region::InsideAgentsFolder, Region::OutsideAgentsFolder];

    /// Build an engine over a two-scope vault with a shared note.
    fn engine() -> (TempDir, RecallEngine) {
        let tmp = TempDir::new().unwrap();
        // jarvis.tony owns a note; jarvis.sam owns a note that also says "borrow".
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("The borrow checker enforces ownership.")
            .unwrap();
        tmp.child("Agents/jarvis.sam/topics/secret.jarvis.sam.md")
            .write_str("Sam's borrow secret lives here.")
            .unwrap();
        // A shared note outside the agents folder.
        tmp.child("Actions/release.md")
            .write_str("The release borrow process is documented.")
            .unwrap();

        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Simple,
            watch_debounce: std::time::Duration::from_millis(0),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: std::time::Duration::from_millis(0),
            index_dir: None,
        };
        let engine = RecallEngine::new(storage, config).unwrap();
        (tmp, engine)
    }

    fn query(text: &str) -> RecallQuery {
        RecallQuery {
            text: Some(text.to_string()),
            regex: None,
            filters: Vec::new(),
            path_prefix: None,
            limit: 100,
            offset: 0,
            modified_after: None,
            modified_before: None,
        }
    }

    fn regex_query(pattern: &str) -> RecallQuery {
        RecallQuery {
            text: None,
            regex: Some(pattern.to_string()),
            filters: Vec::new(),
            path_prefix: None,
            limit: 100,
            offset: 0,
            modified_after: None,
            modified_before: None,
        }
    }

    fn time_query(after: Option<SystemTime>, before: Option<SystemTime>) -> RecallQuery {
        RecallQuery {
            text: None,
            regex: None,
            filters: Vec::new(),
            path_prefix: None,
            limit: 100,
            offset: 0,
            modified_after: after,
            modified_before: before,
        }
    }

    /// An instant `secs` after the Unix epoch.
    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    fn set_mtime(path: &std::path::Path, secs: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t(secs))
            .unwrap();
    }

    #[test]
    fn warm_makes_engine_ready() {
        let (_tmp, engine) = engine();
        assert!(!engine.is_ready());
        engine.warm();
        assert!(engine.is_ready());
    }

    // The build's start/ready log lines are asserted in
    // `tests/recall_build_logs.rs` — a dedicated test binary, because `tracing`
    // caches callsite interest process-wide and the other tests in this module
    // hit the same callsites with no subscriber installed.

    #[test]
    fn resident_scope_count_tracks_the_warm_build() {
        let (_tmp, engine) = engine();
        assert_eq!(engine.resident_scope_count(), 0);
        engine.warm();
        // The fixture vault has two scope directories: jarvis.tony and jarvis.sam.
        assert_eq!(engine.resident_scope_count(), 2);
    }

    #[test]
    fn recall_finds_own_scope_and_shared_but_never_another_scope() {
        let (_tmp, engine) = engine();
        let results = engine
            .recall("jarvis.tony", BOTH, &query("borrow"))
            .unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();

        assert!(paths.contains(&"Agents/topics/rust.md"));
        assert!(paths.contains(&"Actions/release.md"));
        // Structural isolation: sam's note is in a different index and unreachable.
        for hit in &results.hits {
            assert!(!hit.path.contains("sam"), "leaked path: {}", hit.path);
            assert!(!hit.path.contains("secret"), "leaked path: {}", hit.path);
            for snip in &hit.snippets {
                assert!(!snip.contains("Sam"), "leaked snippet: {snip}");
            }
        }
    }

    #[test]
    fn scoped_policy_omits_shared_region() {
        let (_tmp, engine) = engine();
        let results = engine
            .recall(
                "jarvis.tony",
                &[Region::InsideAgentsFolder],
                &query("borrow"),
            )
            .unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.contains(&"Agents/topics/rust.md"));
        assert!(!paths.contains(&"Actions/release.md"));
    }

    #[test]
    fn scores_are_normalized_zero_to_one() {
        let (_tmp, engine) = engine();
        let results = engine
            .recall("jarvis.tony", BOTH, &query("borrow"))
            .unwrap();
        assert!(!results.hits.is_empty());
        for hit in &results.hits {
            assert!(hit.score > 0.0 && hit.score <= 1.0, "score {}", hit.score);
        }
    }

    #[test]
    fn property_filters_are_unsupported_on_simple() {
        let (_tmp, engine) = engine();
        let mut q = query("borrow");
        q.filters.push(PropertyFilter {
            key: "tag".to_string(),
            op: FilterOp::Eq,
            value: Some("rust".to_string()),
        });
        let err = engine.recall("jarvis.tony", BOTH, &q).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::Unsupported);
    }

    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn tantivy_backend_applies_property_filters_end_to_end() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("---\nstatus: published\n---\nThe borrow checker enforces ownership.")
            .unwrap();
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Tantivy,
            watch_debounce: std::time::Duration::from_millis(0),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: std::time::Duration::from_millis(0),
            index_dir: None,
        };
        let engine = RecallEngine::new(storage, config).unwrap();
        assert!(engine.supports_property_filters());

        let mut q = query("borrow");
        q.filters.push(PropertyFilter {
            key: "status".into(),
            op: FilterOp::Eq,
            value: Some("published".into()),
        });
        let hits = engine.recall("jarvis.tony", BOTH, &q).unwrap().hits;
        assert!(hits.iter().any(|h| h.path == "Agents/topics/rust.md"));

        // A non-matching property filter excludes the note.
        let mut q2 = query("borrow");
        q2.filters.push(PropertyFilter {
            key: "status".into(),
            op: FilterOp::Eq,
            value: Some("draft".into()),
        });
        assert!(
            engine
                .recall("jarvis.tony", BOTH, &q2)
                .unwrap()
                .hits
                .is_empty()
        );
    }

    #[test]
    fn time_only_recall_returns_bounded_set_in_recency_order() {
        let (tmp, engine) = engine();
        set_mtime(
            &tmp.path()
                .join("Agents/jarvis.tony/topics/rust.jarvis.tony.md"),
            1_000,
        );
        set_mtime(&tmp.path().join("Actions/release.md"), 3_000);
        // Sam's note is inside the bounds but structurally unreachable.
        set_mtime(
            &tmp.path()
                .join("Agents/jarvis.sam/topics/secret.jarvis.sam.md"),
            5_000,
        );

        let results = engine
            .recall("jarvis.tony", BOTH, &time_query(Some(t(500)), None))
            .unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["Actions/release.md", "Agents/topics/rust.md"]);
        for hit in &results.hits {
            assert_eq!(hit.score, 1.0);
            assert!(hit.snippets.is_empty());
        }
        assert_eq!(
            results.hits[1].modified_at.as_deref(),
            Some("1970-01-01T00:16:40Z")
        );

        // Tightening the lower bound drops the older note.
        let results = engine
            .recall("jarvis.tony", BOTH, &time_query(Some(t(2_000)), None))
            .unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["Actions/release.md"]);
    }

    #[test]
    fn time_bounds_are_half_open() {
        let (tmp, engine) = engine();
        set_mtime(
            &tmp.path()
                .join("Agents/jarvis.tony/topics/rust.jarvis.tony.md"),
            1_000,
        );
        set_mtime(&tmp.path().join("Actions/release.md"), 3_000);

        // mtime == after is included; mtime == before is excluded.
        let results = engine
            .recall(
                "jarvis.tony",
                BOTH,
                &time_query(Some(t(1_000)), Some(t(3_000))),
            )
            .unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["Agents/topics/rust.md"]);
    }

    #[test]
    fn time_bounds_filter_content_hits_and_keep_score_order() {
        let (tmp, engine) = engine();
        // An older note that matches twice, so score order disagrees with recency.
        tmp.child("Agents/jarvis.tony/topics/double.jarvis.tony.md")
            .write_str("borrow and borrow again.")
            .unwrap();
        set_mtime(
            &tmp.path()
                .join("Agents/jarvis.tony/topics/double.jarvis.tony.md"),
            1_000,
        );
        set_mtime(
            &tmp.path()
                .join("Agents/jarvis.tony/topics/rust.jarvis.tony.md"),
            2_000,
        );
        set_mtime(&tmp.path().join("Actions/release.md"), 9_000);

        let mut q = query("borrow");
        q.modified_before = Some(t(5_000));
        let results = engine.recall("jarvis.tony", BOTH, &q).unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
        // Score order (double match first), not recency; the shared note is out
        // of range.
        assert_eq!(
            paths,
            vec!["Agents/topics/double.md", "Agents/topics/rust.md"]
        );
        assert!(!results.hits[0].snippets.is_empty());
        assert!(results.hits[0].modified_at.is_some());
    }

    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn tantivy_time_only_recall_matches_simple_semantics() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/old.jarvis.tony.md")
            .write_str("old note")
            .unwrap();
        tmp.child("Agents/jarvis.tony/topics/new.jarvis.tony.md")
            .write_str("new note")
            .unwrap();
        set_mtime(
            &tmp.path()
                .join("Agents/jarvis.tony/topics/old.jarvis.tony.md"),
            1_000,
        );
        set_mtime(
            &tmp.path()
                .join("Agents/jarvis.tony/topics/new.jarvis.tony.md"),
            3_000,
        );
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Tantivy,
            watch_debounce: std::time::Duration::from_millis(0),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: std::time::Duration::from_millis(0),
            index_dir: None,
        };
        let engine = RecallEngine::new(storage, config).unwrap();

        let results = engine
            .recall(
                "jarvis.tony",
                BOTH,
                &time_query(Some(t(1_000)), Some(t(3_000))),
            )
            .unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["Agents/topics/old.md"]);
        assert_eq!(results.hits[0].score, 1.0);
        assert!(results.hits[0].snippets.is_empty());
    }

    #[test]
    fn own_write_is_reflected_immediately() {
        let (tmp, engine) = engine();
        engine.warm();
        // A new own-scope note, then the synchronous own-write hook.
        let path = tmp.child("Agents/jarvis.tony/topics/async.jarvis.tony.md");
        path.write_str("Futures are polled by the executor.")
            .unwrap();
        let resolver = engine.storage.resolver();
        let vpath = crate::path::VirtualPath::new("Agents/topics/async.md").unwrap();
        let physical = resolver.resolve("jarvis.tony", &vpath).unwrap();
        engine.on_write("jarvis.tony", Region::InsideAgentsFolder, &physical);

        let results = engine
            .recall("jarvis.tony", BOTH, &query("futures"))
            .unwrap();
        let paths: Vec<&str> = results.hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.contains(&"Agents/topics/async.md"));
    }

    #[test]
    fn recall_matches_own_path_end_to_end() {
        let (_tmp, engine) = engine();
        // "rust" appears only in the path Agents/topics/rust.md, not its body.
        let results = engine
            .recall("jarvis.tony", BOTH, &regex_query("rust"))
            .unwrap();
        let hit = results
            .hits
            .iter()
            .find(|h| h.path == "Agents/topics/rust.md")
            .expect("path-only hit");
        assert!(hit.snippets.iter().any(|s| s == "Agents/topics/rust.md"));
    }

    #[test]
    fn path_match_never_leaks_another_scope() {
        let (_tmp, engine) = engine();
        // "secret" matches jarvis.sam's path (Agents/topics/secret.md) and body, but
        // sam lives in a separate index and must never surface for jarvis.tony.
        let results = engine
            .recall("jarvis.tony", BOTH, &regex_query("secret"))
            .unwrap();
        for hit in &results.hits {
            assert!(!hit.path.contains("secret"), "leaked path: {}", hit.path);
        }
    }

    #[test]
    fn scoped_index_ingests_the_clean_read_view() {
        let (tmp, engine) = engine();
        // The stored form carries the scope suffix, as the write path persists it.
        tmp.child("Agents/jarvis.tony/topics/links.jarvis.tony.md")
            .write_str("see [[rust.jarvis.tony]] for ownership")
            .unwrap();

        // A regex for the clean link form matches the stripped indexed content,
        // and the snippet is clean without any query-time repair.
        let results = engine
            .recall("jarvis.tony", BOTH, &regex_query(r"\[\[rust\]\]"))
            .unwrap();
        let hit = results
            .hits
            .iter()
            .find(|h| h.path == "Agents/topics/links.md")
            .expect("clean-form regex hit");
        assert_eq!(hit.snippets, vec!["see [[rust]] for ownership"]);

        // Scope idents occur only in stored link suffixes and are not content.
        let results = engine.recall("jarvis.tony", BOTH, &query("tony")).unwrap();
        assert!(
            results.hits.is_empty(),
            "scope ident matched as content: {:?}",
            results.hits
        );
    }

    #[test]
    fn shared_index_ingests_verbatim() {
        let (tmp, engine) = engine();
        // A shared note (externally written) may contain suffix-looking text;
        // the shared index never strips, so queries match it exactly as stored.
        tmp.child("Actions/plan.md")
            .write_str("tracked in [[plan.jarvis.tony]] for now")
            .unwrap();
        let results = engine
            .recall("jarvis.tony", BOTH, &query("plan.jarvis.tony"))
            .unwrap();
        let hit = results
            .hits
            .iter()
            .find(|h| h.path == "Actions/plan.md")
            .expect("verbatim shared hit");
        assert_eq!(
            hit.snippets,
            vec!["tracked in [[plan.jarvis.tony]] for now"]
        );
    }

    #[test]
    fn every_ingestion_path_strips_identically() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/links.jarvis.tony.md")
            .write_str("see [[rust.jarvis.tony]] for ownership")
            .unwrap();
        tmp.child("Agents/jarvis.sam/topics/other.jarvis.sam.md")
            .write_str("unrelated")
            .unwrap();
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        // A cap of one resident scope (so a sam query evicts tony) and an
        // hour-long freshness (so only the exercised path re-ingests).
        let config = RecallConfig {
            backend: RecallBackendKind::Simple,
            watch_debounce: std::time::Duration::from_secs(3600),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 1,
            freshness: std::time::Duration::from_secs(3600),
            index_dir: None,
        };
        let engine = RecallEngine::new(storage, config).unwrap();

        let tony_snippets = |engine: &RecallEngine| {
            // The scope ident never matches as content on any ingestion path.
            assert!(
                engine
                    .recall("jarvis.tony", BOTH, &query("tony"))
                    .unwrap()
                    .hits
                    .is_empty(),
                "scope ident matched as content"
            );
            engine
                .recall("jarvis.tony", BOTH, &regex_query(r"\[\[rust\]\]"))
                .unwrap()
                .hits
                .iter()
                .find(|h| h.path == "Agents/topics/links.md")
                .expect("clean-form hit")
                .snippets
                .clone()
        };

        // 1. Startup warm build.
        engine.warm();
        let warm = tony_snippets(&engine);
        assert_eq!(warm, vec!["see [[rust]] for ownership"]);

        // 2. The synchronous own-write hook re-ingests the same note.
        let vpath = crate::path::VirtualPath::new("Agents/topics/links.md").unwrap();
        let physical = engine
            .storage
            .resolver()
            .resolve("jarvis.tony", &vpath)
            .unwrap();
        engine.on_write("jarvis.tony", Region::InsideAgentsFolder, &physical);
        let own_write = tony_snippets(&engine);

        // 3. Rebuild after eviction: the sam query makes tony the LRU victim.
        engine
            .recall("jarvis.sam", BOTH, &query("unrelated"))
            .unwrap();
        let rebuilt = tony_snippets(&engine);

        assert_eq!(warm, own_write);
        assert_eq!(warm, rebuilt);
    }

    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn tantivy_index_ingests_the_clean_read_view() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/links.jarvis.tony.md")
            .write_str("see [[rust.jarvis.tony]] for ownership")
            .unwrap();
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Tantivy,
            watch_debounce: std::time::Duration::from_millis(0),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: std::time::Duration::from_millis(0),
            index_dir: None,
        };
        let engine = RecallEngine::new(storage, config).unwrap();

        // The clean link form matches under regex, with a clean snippet.
        let results = engine
            .recall("jarvis.tony", BOTH, &regex_query(r"\[\[rust\]\]"))
            .unwrap();
        let hit = results
            .hits
            .iter()
            .find(|h| h.path == "Agents/topics/links.md")
            .expect("clean-form regex hit");
        assert_eq!(hit.snippets, vec!["see [[rust]] for ownership"]);

        // BM25 never tokenizes scope idents out of stored link suffixes.
        let results = engine.recall("jarvis.tony", BOTH, &query("tony")).unwrap();
        assert!(
            results.hits.is_empty(),
            "scope ident matched as content: {:?}",
            results.hits
        );
    }

    // --- persistent index (MUNINN_RECALL_INDEX_DIR) ---

    /// A tantivy engine over `vault`, optionally persisting under `index_dir`. The
    /// freshness window is an hour and no watcher runs, so the only reconciles are
    /// the ones a test triggers — which makes `ingested_count` an exact count of
    /// the note bodies the engine chose to read.
    #[cfg(feature = "recall-tantivy")]
    fn persisted_engine(
        vault: &std::path::Path,
        index_dir: Option<&std::path::Path>,
        max_resident_scopes: usize,
        scheme: &str,
    ) -> RecallEngine {
        let resolver = PathResolver::new(
            vault.canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse(scheme).unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Tantivy,
            watch_debounce: std::time::Duration::from_secs(3600),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes,
            freshness: std::time::Duration::from_secs(3600),
            index_dir: index_dir.map(|p| p.to_path_buf()),
        };
        RecallEngine::new(storage, config).unwrap()
    }

    /// A two-scope vault with a shared note: five notes in total.
    #[cfg(feature = "recall-tantivy")]
    fn persistence_vault() -> TempDir {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("The borrow checker enforces ownership.")
            .unwrap();
        tmp.child("Agents/jarvis.tony/topics/async.jarvis.tony.md")
            .write_str("Futures are polled by the executor.")
            .unwrap();
        tmp.child("Agents/jarvis.sam/topics/notes.jarvis.sam.md")
            .write_str("Sam keeps ownership notes.")
            .unwrap();
        tmp.child("Actions/release.md")
            .write_str("The release process is documented.")
            .unwrap();
        tmp.child("Actions/roadmap.md")
            .write_str("The roadmap lists the next milestones.")
            .unwrap();
        tmp
    }

    /// The fingerprint directories currently present under an index dir.
    #[cfg(feature = "recall-tantivy")]
    fn fingerprint_dirs(index_dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(index_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Restarting over an unchanged vault must read no note content at all: the
    /// manifest recovered from the persisted index makes every file compare equal
    /// under the stat-diff.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn restart_over_an_unchanged_vault_reindexes_nothing() {
        let vault = persistence_vault();
        let index = TempDir::new().unwrap();

        let cold = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        cold.warm();
        assert_eq!(cold.ingested_count(), 5, "the cold build reads every note");
        let before = cold
            .recall("jarvis.tony", BOTH, &query("ownership"))
            .unwrap();
        drop(cold);

        let warm = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        warm.warm();
        assert_eq!(
            warm.ingested_count(),
            0,
            "a restart over an unchanged vault must not re-read any note"
        );
        let after = warm
            .recall("jarvis.tony", BOTH, &query("ownership"))
            .unwrap();

        let paths =
            |r: &RecallResults| -> Vec<String> { r.hits.iter().map(|h| h.path.clone()).collect() };
        assert_eq!(paths(&before), paths(&after));
        assert!(paths(&after).contains(&"Agents/topics/rust.md".to_string()));
        // Cross-scope isolation survives persistence: sam's note lives in another
        // region directory and is unreachable, though it matches the query.
        for path in paths(&after) {
            assert!(!path.contains("notes"), "leaked path: {path}");
        }
    }

    /// Everything that changed while the server was down is reconciled on the next
    /// start, and only that.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn changes_made_while_down_are_reconciled_and_nothing_else_is_reread() {
        let vault = persistence_vault();
        let index = TempDir::new().unwrap();

        let cold = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        cold.warm();
        drop(cold);

        // While "down": one note added, one edited, one deleted.
        vault
            .child("Agents/jarvis.tony/topics/traits.jarvis.tony.md")
            .write_str("Traits describe shared behaviour.")
            .unwrap();
        let edited = vault
            .path()
            .join("Agents/jarvis.tony/topics/rust.jarvis.tony.md");
        std::fs::write(&edited, "The borrow checker now also mentions lifetimes.").unwrap();
        // An explicit mtime so the change is visible even on a coarse clock.
        set_mtime(&edited, 9_000);
        std::fs::remove_file(
            vault
                .path()
                .join("Agents/jarvis.tony/topics/async.jarvis.tony.md"),
        )
        .unwrap();

        let warm = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        warm.warm();
        assert_eq!(
            warm.ingested_count(),
            2,
            "only the added and the edited note should be read"
        );

        let hits = warm
            .recall("jarvis.tony", BOTH, &query("lifetimes"))
            .unwrap();
        let paths: Vec<&str> = hits.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["Agents/topics/rust.md"], "the edit is visible");

        let hits = warm.recall("jarvis.tony", BOTH, &query("traits")).unwrap();
        assert!(
            hits.hits
                .iter()
                .any(|h| h.path == "Agents/topics/traits.md"),
            "the addition is visible: {:?}",
            hits.hits
        );

        let hits = warm.recall("jarvis.tony", BOTH, &query("futures")).unwrap();
        assert!(
            hits.hits.is_empty(),
            "the deleted note must be gone: {:?}",
            hits.hits
        );
    }

    /// A configuration change that alters what gets ingested lands on a different
    /// fingerprint, so the previous index is neither reused nor left on disk.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn a_changed_scheme_invalidates_and_removes_the_persisted_index() {
        let vault = persistence_vault();
        let index = TempDir::new().unwrap();

        let first = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        first.warm();
        drop(first);
        let before = fingerprint_dirs(index.path());
        assert_eq!(before.len(), 1, "one fingerprint directory: {before:?}");

        // A different scheme shapes the ingested view, so the fingerprint changes.
        let second = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>");
        second.warm();
        let after = fingerprint_dirs(index.path());
        assert_eq!(
            after.len(),
            1,
            "the stale fingerprint must be gone: {after:?}"
        );
        assert_ne!(before, after);
        assert!(
            second.ingested_count() > 0,
            "the new fingerprint must be built from the vault"
        );
        // And the shared region — unaffected by the scheme — still answers.
        let hits = second
            .recall("jarvis.tony", BOTH, &query("release"))
            .unwrap();
        assert!(
            hits.hits.iter().any(|h| h.path == "Actions/release.md"),
            "results must be correct under the new fingerprint: {:?}",
            hits.hits
        );
    }

    /// A damaged index is discarded rather than trusted or fatal.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn a_corrupted_persisted_index_is_wiped_and_rebuilt() {
        let vault = persistence_vault();
        let index = TempDir::new().unwrap();

        let cold = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        cold.warm();
        drop(cold);

        // Truncate every segment file in every region, leaving tantivy's metadata
        // intact so the damage is only discovered while reading documents.
        let mut truncated = 0usize;
        for entry in walk_files(index.path()) {
            let name = entry.file_name().unwrap().to_string_lossy().into_owned();
            if name == "meta.json" || name == ".managed.json" || name == "region.id" {
                continue;
            }
            std::fs::write(&entry, b"").unwrap();
            truncated += 1;
        }
        assert!(truncated > 0, "the build must have written segment files");

        let warm = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        warm.warm();
        assert_eq!(
            warm.ingested_count(),
            5,
            "a corrupted index must be rebuilt from the vault in full"
        );
        let hits = warm
            .recall("jarvis.tony", BOTH, &query("ownership"))
            .unwrap();
        assert!(
            hits.hits.iter().any(|h| h.path == "Agents/topics/rust.md"),
            "recall must be correct after the rebuild: {:?}",
            hits.hits
        );
        // Every rebuilt region directory is still claimed by its own region, so a
        // later start cannot mistake one region's index for another's.
        let markers: Vec<PathBuf> = walk_files(index.path())
            .into_iter()
            .filter(|p| p.file_name().is_some_and(|n| n == "region.id"))
            .collect();
        assert_eq!(
            markers.len(),
            3,
            "expected an identity marker per region (two scopes and shared): {markers:?}"
        );
    }

    /// With persistence on, re-residence after eviction is a reopen plus a
    /// stat-diff, not a rebuild.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn re_resident_scopes_reopen_instead_of_reindexing() {
        let vault = persistence_vault();
        let index = TempDir::new().unwrap();
        // One resident scope, so each cross-scope recall evicts the other.
        let engine = persisted_engine(vault.path(), Some(index.path()), 1, "<agent>.<user>");
        engine.warm();
        let after_build = engine.ingested_count();
        assert_eq!(after_build, 5);

        for _ in 0..3 {
            let tony = engine
                .recall("jarvis.tony", BOTH, &query("ownership"))
                .unwrap();
            assert!(
                tony.hits.iter().any(|h| h.path == "Agents/topics/rust.md"),
                "tony's own note must survive the round trip: {:?}",
                tony.hits
            );
            let sam = engine
                .recall("jarvis.sam", BOTH, &query("ownership"))
                .unwrap();
            assert!(
                sam.hits.iter().any(|h| h.path == "Agents/topics/notes.md"),
                "sam's own note must survive the round trip: {:?}",
                sam.hits
            );
            assert!(engine.resident_scope_count() <= 1);
        }
        assert_eq!(
            engine.ingested_count(),
            after_build,
            "re-residence must reopen the persisted index, not re-index the scope"
        );
    }

    /// Persistence is opt-in: with no index directory the tantivy backend writes
    /// nothing outside the vault, exactly as before this option existed.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn without_an_index_dir_nothing_is_written_to_disk() {
        let vault = persistence_vault();
        let scratch = TempDir::new().unwrap();
        let engine = persisted_engine(vault.path(), None, 256, "<agent>.<user>");
        engine.warm();
        assert!(
            !engine
                .recall("jarvis.tony", BOTH, &query("ownership"))
                .unwrap()
                .hits
                .is_empty()
        );
        assert!(
            walk_files(scratch.path()).is_empty(),
            "an unconfigured engine must not write index files anywhere"
        );
    }

    /// The `simple` backend has no on-disk form, so a configured index directory is
    /// ignored — the engine starts, serves, and leaves the directory empty.
    #[test]
    fn the_simple_backend_ignores_a_configured_index_dir() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("The borrow checker enforces ownership.")
            .unwrap();
        let index = TempDir::new().unwrap();
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Simple,
            watch_debounce: std::time::Duration::from_millis(0),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: std::time::Duration::from_millis(0),
            index_dir: Some(index.path().to_path_buf()),
        };
        let engine = RecallEngine::new(storage, config).unwrap();
        engine.warm();
        assert!(
            !engine
                .recall("jarvis.tony", BOTH, &query("ownership"))
                .unwrap()
                .hits
                .is_empty()
        );
        assert!(
            walk_files(index.path()).is_empty(),
            "the simple backend must leave the index directory untouched"
        );
    }

    /// Every regular file under `root`, recursively.
    fn walk_files(root: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn tantivy_matches_path_end_to_end() {
        let tmp = TempDir::new().unwrap();
        // The date appears only in the path, not the body.
        tmp.child("Agents/jarvis.tony/diary/2026-06-10.jarvis.tony.md")
            .write_str("Nothing dated in the body.")
            .unwrap();
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Tantivy,
            watch_debounce: std::time::Duration::from_millis(0),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: std::time::Duration::from_millis(0),
            index_dir: None,
        };
        let engine = RecallEngine::new(storage, config).unwrap();
        let results = engine
            .recall("jarvis.tony", BOTH, &regex_query("2026-06-10"))
            .unwrap();
        let hit = results
            .hits
            .iter()
            .find(|h| h.path == "Agents/diary/2026-06-10.md")
            .expect("path-only hit");
        assert!(
            hit.snippets
                .iter()
                .any(|s| s == "Agents/diary/2026-06-10.md")
        );
    }
}
