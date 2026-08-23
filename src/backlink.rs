//! A maintained, per-rendered-scope reverse link index for backlink discovery.
//!
//! `collect_backlinks` (`read_memory_note(backlinks:true)`) and
//! `rename_memory_note` Phase 1 historically discovered a note's referrers by
//! scanning every visible note's content — O(vault) per query. This module keeps
//! the same information incrementally: for each rendered scope, a map from each
//! visible target note (clean path, `.md` stripped) to the visible notes whose
//! links resolve to it under that scope's visible set, using the same
//! resolution rules as the forward link transform ([`crate::wikilink`]).
//! Backlink discovery is then an index lookup proportional to the result set.
//!
//! Resolution is per visible set: a shared referrer's `[[rust]]` resolves to a
//! different target under different scopes (own-scope preferred). The index is
//! therefore per rendered scope — each scope's [`ScopeBacklinks`] indexes the
//! scope's own notes and the shared notes as that scope sees them, so a shared
//! write updates every resident scope.
//!
//! ## Incremental maintenance
//!
//! A backlink edge is a function of the whole visible set, not only the
//! referrer's content: adding or removing a note can re-point *other* notes'
//! links (own-scope-preferred tie-break; dangling links resolving once their
//! target appears). Those shifts are exactly keyed by basename — resolution
//! matches candidates by basename, so a membership change to a note with
//! basename `b` can only alter the resolution of links whose target basename is
//! `b`. Each scope therefore keeps, alongside the reverse map, a per-referrer
//! record of its resolved out-edges and the raw basenames it links, plus an
//! inverted basename → referrers map:
//!
//! - A content-only write recomputes the one note's out-edges — O(its links).
//! - A membership change (create/delete/rename, server-side or external)
//!   additionally recomputes the out-edges of referrers linking `b` —
//!   O(affected referrers), still independent of vault size.
//!
//! ## Lifecycle (mirrors the recall engine)
//!
//! Every scope's index is built eagerly at startup; the engine is updated
//! synchronously on the server's own write paths and reconciled against
//! external edits by a stat-diff that a filesystem watcher (and a freshness
//! window) trigger. Resident scopes are bounded by the recall engine's
//! `max_resident_scopes`; the least-recently-accessed scope is evicted after
//! queries. When `MUNINN_RECALL_INDEX_DIR` is set (and the `recall-tantivy`
//! feature is built), each scope's index is serialized under the recall
//! fingerprint layer and reopened on restart — a stat-diff reconcile then
//! replaces a full re-scan. Nothing is written to disk otherwise.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::config::RecallConfig;
use crate::error::MuninnError;
use crate::path::{PathResolver, PhysicalPath, VirtualPath};
use crate::policy::Region;
use crate::storage::{LinkIndex, Storage};
use crate::wikilink;

/// The reverse map of one rendered scope: each visible target note (clean path,
/// `.md` stripped) to the clean virtual paths (extension retained) of the
/// visible notes whose links resolve to that target under the scope.
#[derive(Debug, Default)]
pub struct BacklinkIndex {
    referrers: HashMap<String, BTreeSet<String>>,
}

impl BacklinkIndex {
    /// Record that `referrer` links to `target`.
    pub(crate) fn add_edge(&mut self, target: &str, referrer: &str) {
        self.referrers
            .entry(target.to_string())
            .or_default()
            .insert(referrer.to_string());
    }

    /// Remove the edge `referrer` → `target`, dropping the target's entry when
    /// its referrer set empties.
    pub(crate) fn remove_edge(&mut self, target: &str, referrer: &str) {
        if let Some(set) = self.referrers.get_mut(target) {
            set.remove(referrer);
        }
        if self.referrers.get(target).is_some_and(|set| set.is_empty()) {
            self.referrers.remove(target);
        }
    }

    /// The referrers of `target`, ascending (BTreeSet order — the same order
    /// the scan-based `collect_backlinks` returned).
    pub(crate) fn referrers_of(&self, target: &str) -> Vec<String> {
        self.referrers
            .get(target)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// One referrer's contribution to a scope's index: its resolved out-edge
/// targets and the raw basenames it links (the shift-detection keys).
#[derive(Debug, Default)]
struct ReferrerEdges {
    /// Resolved targets (clean paths, `.md` stripped).
    targets: BTreeSet<String>,
    /// The last-segment basename of every link target the note carries,
    /// computed with `resolve_target`'s own cleaning (strip `.md`, last
    /// segment). Dangling targets count toward basenames: they may resolve
    /// after a membership change.
    basenames: BTreeSet<String>,
}

/// Per-file stat metadata for the stat-diff reconcile.
struct FileMeta {
    /// The note's clean virtual path (extension retained).
    vpath: String,
    mtime: SystemTime,
    size: u64,
}

/// One rendered scope's reverse index plus its reconcile bookkeeping. Indexes
/// the scope's own notes and the shared notes, resolved under the scope's
/// visible set.
struct ScopeBacklinks {
    /// The rendered scope this index serves ("" in single-tenant).
    scope: String,
    /// target clean path → referrers (the query-facing map).
    reverse: BacklinkIndex,
    /// referrer vpath → its edges (drives removal and shift detection).
    referrers: HashMap<String, ReferrerEdges>,
    /// raw link-target basename → referrers carrying such a link.
    by_basename: HashMap<String, BTreeSet<String>>,
    /// The scope's forward visible set, for resolving out-edges.
    forward: LinkIndex,
    /// physical path → last-processed stat, for the stat-diff reconcile.
    manifest: BTreeMap<PathBuf, FileMeta>,
    last_reconcile: Option<Instant>,
    last_access: Instant,
}

impl ScopeBacklinks {
    fn new(scope: &str) -> ScopeBacklinks {
        ScopeBacklinks {
            scope: scope.to_string(),
            reverse: BacklinkIndex::default(),
            referrers: HashMap::new(),
            by_basename: HashMap::new(),
            forward: LinkIndex::default(),
            manifest: BTreeMap::new(),
            last_reconcile: None,
            last_access: Instant::now(),
        }
    }

    /// Replace one referrer's edges (`Some`) or drop them entirely (`None`),
    /// keeping the reverse map and the basename inversion consistent.
    fn apply_referrer(&mut self, referrer: &str, edges: Option<ReferrerEdges>) {
        if let Some(old) = self.referrers.remove(referrer) {
            for target in &old.targets {
                self.reverse.remove_edge(target, referrer);
            }
            for base in &old.basenames {
                if let Some(set) = self.by_basename.get_mut(base) {
                    set.remove(referrer);
                }
                if self.by_basename.get(base).is_some_and(|set| set.is_empty()) {
                    self.by_basename.remove(base);
                }
            }
        }
        if let Some(edges) = edges {
            for target in &edges.targets {
                self.reverse.add_edge(target, referrer);
            }
            for base in &edges.basenames {
                self.by_basename
                    .entry(base.clone())
                    .or_default()
                    .insert(referrer.to_string());
            }
            self.referrers.insert(referrer.to_string(), edges);
        }
    }
}

/// The resolved out-edge targets of `content` (its stored, on-disk form) under
/// `rendered_scope`'s visible set: every visible note at least one of the
/// content's links resolves to. This is the exact inverse of the forward link
/// transform — computed with the same collector pass and
/// [`wikilink::resolve_target`] as [`wikilink::references_to`], so for any
/// target `t`, `compute_out_edges(...).contains(t)` agrees with
/// `references_to(content, t, ...)`. Dangling links produce no targets.
///
/// Exercised by this module's unit tests; the engine computes through
/// [`compute_link_edges`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn compute_out_edges(
    content: &str,
    rendered_scope: &str,
    resolver: &PathResolver,
    index: &LinkIndex,
) -> BTreeSet<String> {
    compute_link_edges(content, rendered_scope, resolver, index).targets
}

/// [`compute_out_edges`] plus the raw target basenames the content links
/// (shift-detection keys — see the module docs).
fn compute_link_edges(
    content: &str,
    rendered_scope: &str,
    resolver: &PathResolver,
    index: &LinkIndex,
) -> ReferrerEdges {
    let mut edges = ReferrerEdges::default();
    // Collector mode: the callback never rewrites and never errors.
    let _ = wikilink::rewrite_links(content, |kind, target| {
        let stripped = wikilink::strip_target(kind, target, rendered_scope, resolver);
        let clean = stripped.as_deref().unwrap_or(target);
        edges.basenames.insert(link_target_basename(clean));
        if let Some(entry) = wikilink::resolve_target(index, kind, clean) {
            edges.targets.insert(entry.clean_path.clone());
        }
        Ok::<_, MuninnError>(None)
    });
    edges
}

/// The shift-detection key of a link target or note path: its last segment with
/// a literal `.md` stripped — exactly the basename [`wikilink::resolve_target`]
/// matches candidates by.
fn link_target_basename(target: &str) -> String {
    let clean = target.strip_suffix(".md").unwrap_or(target);
    clean
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(clean)
        .to_string()
}

/// The mutable engine state behind a single lock.
struct EngineState {
    built: bool,
    scopes: HashMap<String, ScopeBacklinks>,
}

/// The backlink engine. Holds the per-scope reverse indexes and serves backlink
/// queries; shared behind the `Toolbox`'s `Arc`.
pub struct BacklinkEngine {
    storage: Arc<Storage>,
    /// The regions indexed for every scope — the server's visible regions,
    /// fixed at construction from the active policy.
    regions: Vec<Region>,
    /// Reused from the recall configuration: the resident-scope bound and the
    /// reconcile freshness window.
    max_resident_scopes: usize,
    freshness: Duration,
    state: Mutex<EngineState>,
    /// Set true once the eager build has completed.
    ready: AtomicBool,
    /// Set by the filesystem watcher; forces the next query to reconcile.
    dirty: Arc<AtomicBool>,
    /// The live watcher; kept alive for the engine's lifetime.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// How many note bodies have been read for edge computation since
    /// construction. Backs [`BacklinkEngine::ingested_count`].
    ingested: AtomicU64,
    /// The persisted-index root, when the operator configured one and the build
    /// carries the `recall-tantivy` feature (whose fingerprint layer is reused).
    #[cfg(feature = "recall-tantivy")]
    persist: Option<crate::recall::persist::PersistRoot>,
}

impl BacklinkEngine {
    /// Build an engine indexing `regions` for every scope. Tuning knobs
    /// (`max_resident_scopes`, `freshness`, and — with the `recall-tantivy`
    /// feature — `index_dir`) are reused from the recall configuration.
    pub fn new(
        storage: Arc<Storage>,
        regions: Vec<Region>,
        config: &RecallConfig,
    ) -> BacklinkEngine {
        #[cfg(feature = "recall-tantivy")]
        let persist = config
            .index_dir
            .as_ref()
            .map(|dir| crate::recall::persist::PersistRoot::new(dir, &storage));
        BacklinkEngine {
            storage,
            regions,
            max_resident_scopes: config.max_resident_scopes,
            freshness: config.freshness,
            state: Mutex::new(EngineState {
                built: false,
                scopes: HashMap::new(),
            }),
            ready: AtomicBool::new(false),
            dirty: Arc::new(AtomicBool::new(false)),
            watcher: Mutex::new(None),
            ingested: AtomicU64::new(0),
            #[cfg(feature = "recall-tantivy")]
            persist,
        }
    }

    /// `true` once the eager startup build has completed.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// The number of per-scope indexes currently resident in memory. Backs the
    /// eviction-bound tests.
    pub fn resident_scope_count(&self) -> usize {
        let state = self.state.lock().expect("backlink state poisoned");
        state.scopes.len()
    }

    /// How many note bodies have been read for edge computation since
    /// construction — the count a persisted index exists to keep near zero
    /// across restarts, and the seam proving queries never scan the vault.
    pub fn ingested_count(&self) -> u64 {
        self.ingested.load(Ordering::Acquire)
    }

    /// Eagerly build every scope's index, then mark ready. Safe to call
    /// repeatedly; the build runs once. This is also the block-until-ready
    /// path: a query arriving before the background build finishes takes the
    /// lock and builds inline.
    pub fn warm(&self) {
        let mut state = self.state.lock().expect("backlink state poisoned");
        self.ensure_built(&mut state);
    }

    fn ensure_built(&self, state: &mut EngineState) {
        if state.built {
            return;
        }
        let started = Instant::now();
        tracing::info!("backlink index build started");
        let mut scope_dirs = self.storage.list_scope_dirs();
        // Single-tenant (empty scheme): one index under the empty rendered
        // scope.
        if self.storage.resolver().scheme().is_empty() {
            scope_dirs.push(String::new());
        }
        for scope in scope_dirs {
            let sb = self.build_scope(&scope);
            state.scopes.insert(scope, sb);
        }
        state.built = true;
        self.ready.store(true, Ordering::Release);
        tracing::info!(
            scopes = state.scopes.len(),
            elapsed = ?started.elapsed(),
            "backlink index ready"
        );
    }

    /// Start the filesystem watcher: any change under the vault root marks the
    /// engine dirty, so the next query reconciles. Idempotent.
    pub fn start_watcher(&self) {
        use notify::{RecursiveMode, Watcher};
        let mut guard = self.watcher.lock().expect("backlink watcher poisoned");
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
                tracing::warn!(%err, "backlink filesystem watcher unavailable; relying on the freshness reconcile");
                return;
            }
        };
        let root = self.storage.resolver().vault_root().to_path_buf();
        if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
            tracing::warn!(%err, "backlink watcher could not watch the vault root");
            return;
        }
        *guard = Some(watcher);
    }

    /// The clean virtual paths (extension retained, ascending) of every visible
    /// note whose links resolve to `target_clean` under `rendered_scope` — the
    /// same set the scan-based `collect_backlinks` computed, from the
    /// maintained index.
    pub fn backlinks(&self, rendered_scope: &str, target_clean: &str) -> Vec<String> {
        let mut state = self.state.lock().expect("backlink state poisoned");
        self.ensure_built(&mut state);
        let force = self.dirty.swap(false, Ordering::AcqRel);
        self.ensure_scope_resident(&mut state, rendered_scope);
        let out = match state.scopes.get_mut(rendered_scope) {
            Some(sb) => {
                self.refresh(sb, force);
                sb.last_access = Instant::now();
                sb.reverse.referrers_of(target_clean)
            }
            None => Vec::new(),
        };
        drop(state);
        self.evict_if_needed();
        out
    }

    /// Incrementally update the index after the server's own write to
    /// `physical`. A no-op when the engine is not built yet (the eager build
    /// will read the new content) or the affected scopes are not resident
    /// (their next build or reconcile picks the change up). A write to the
    /// shared region updates every resident scope: a shared note's edges differ
    /// per scope. A missing file is treated as a delete.
    pub fn on_write(&self, rendered_scope: &str, region: Region, physical: &PhysicalPath) {
        let mut state = self.state.lock().expect("backlink state poisoned");
        if !state.built {
            return;
        }
        match region {
            Region::InsideAgentsFolder => {
                if let Some(sb) = state.scopes.get_mut(rendered_scope) {
                    self.apply_path(sb, Region::InsideAgentsFolder, physical);
                }
            }
            Region::OutsideAgentsFolder => {
                for sb in state.scopes.values_mut() {
                    self.apply_path(sb, Region::OutsideAgentsFolder, physical);
                }
            }
        }
    }

    /// Incrementally update the index after the server's own delete of
    /// `physical`. Same affected-scope rules as [`BacklinkEngine::on_write`].
    pub fn on_delete(&self, rendered_scope: &str, region: Region, physical: &PhysicalPath) {
        // The stat check inside apply_path treats the now-missing file as a
        // delete; both hooks share the one code path, exactly like recall's.
        self.on_write(rendered_scope, region, physical);
    }

    // --- index construction / reconciliation ---

    /// Build one scope's index: reopen its persisted snapshot when configured
    /// and valid, reconcile against the vault (a stat-diff; a fresh index reads
    /// every visible note), then persist the result.
    fn build_scope(&self, scope: &str) -> ScopeBacklinks {
        let mut sb = ScopeBacklinks::new(scope);
        #[cfg(feature = "recall-tantivy")]
        self.load_persisted(&mut sb);
        self.reconcile_scope(&mut sb);
        #[cfg(feature = "recall-tantivy")]
        self.persist_scope(&sb);
        sb
    }

    /// Build a scope index on demand when it was never built or was evicted.
    fn ensure_scope_resident(&self, state: &mut EngineState, rendered_scope: &str) {
        if state.scopes.contains_key(rendered_scope) {
            return;
        }
        let sb = self.build_scope(rendered_scope);
        state.scopes.insert(rendered_scope.to_string(), sb);
    }

    /// Reconcile if the index is stale or the engine was marked dirty.
    fn refresh(&self, sb: &mut ScopeBacklinks, force: bool) {
        let stale = match sb.last_reconcile {
            None => true,
            Some(t) => t.elapsed() >= self.freshness,
        };
        if force || stale {
            self.reconcile_scope(sb);
            #[cfg(feature = "recall-tantivy")]
            self.persist_scope(sb);
        }
    }

    /// Evict the least-recently-accessed scope indexes beyond the resident cap,
    /// persisting each victim first so a later re-residence reopens the
    /// snapshot instead of recomputing from the vault.
    fn evict_if_needed(&self) {
        let cap = self.max_resident_scopes.max(1);
        let mut state = self.state.lock().expect("backlink state poisoned");
        while state.scopes.len() > cap {
            let victim = state
                .scopes
                .iter()
                .min_by_key(|(_, sb)| sb.last_access)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    if let Some(sb) = state.scopes.remove(&k) {
                        #[cfg(feature = "recall-tantivy")]
                        self.persist_scope(&sb);
                        #[cfg(not(feature = "recall-tantivy"))]
                        drop(sb);
                    }
                }
                None => break,
            }
        }
    }
}

impl BacklinkEngine {
    /// Stat-diff reconcile of one scope against the vault: recompute the edges
    /// of new and changed files, drop the edges of vanished ones, and route the
    /// basenames of membership changes through the shift recompute so other
    /// notes' re-pointed links converge within the reconcile window.
    fn reconcile_scope(&self, sb: &mut ScopeBacklinks) {
        let resolver = self.storage.resolver();
        let current = self
            .storage
            .list_visible(&sb.scope, &self.regions)
            .unwrap_or_default();

        // The current visible set as the scope's fresh forward index (paths
        // only — no content reads).
        let mut forward = LinkIndex::default();
        let mut current_entries: Vec<(PhysicalPath, String)> = Vec::with_capacity(current.len());
        for vpath in &current {
            let Ok(physical) = resolver.resolve(&sb.scope, vpath) else {
                continue;
            };
            forward.insert(vpath.as_str(), resolver.detect_region(vpath));
            current_entries.push((physical, vpath.as_str().to_string()));
        }
        forward.sort();

        // Membership diff against the previously indexed visible set; each
        // added/removed basename keys the links whose resolution can shift.
        let mut shift_basenames: BTreeSet<String> = BTreeSet::new();
        {
            let old: BTreeSet<&str> = sb
                .forward
                .all_entries()
                .map(|e| e.clean_path.as_str())
                .collect();
            let new: BTreeSet<&str> = forward
                .all_entries()
                .map(|e| e.clean_path.as_str())
                .collect();
            for path in new.difference(&old) {
                shift_basenames.insert(link_target_basename(path));
            }
            for path in old.difference(&new) {
                shift_basenames.insert(link_target_basename(path));
            }
        }
        sb.forward = forward;

        let mut seen: BTreeMap<PathBuf, ()> = BTreeMap::new();
        let mut processed: HashSet<String> = HashSet::new();
        for (physical, vpath) in &current_entries {
            let key = physical.as_path().to_path_buf();
            seen.insert(key.clone(), ());
            let Ok(meta) = std::fs::metadata(physical.as_path()) else {
                continue;
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let size = meta.len();
            let unchanged = sb
                .manifest
                .get(&key)
                .is_some_and(|prev| prev.mtime == mtime && prev.size == size);
            if unchanged {
                continue;
            }
            if let Ok(body) = self.storage.read(physical) {
                self.ingested.fetch_add(1, Ordering::AcqRel);
                let edges = compute_link_edges(&body, &sb.scope, resolver, &sb.forward);
                sb.apply_referrer(vpath, Some(edges));
                sb.manifest.insert(
                    key,
                    FileMeta {
                        vpath: vpath.clone(),
                        mtime,
                        size,
                    },
                );
                processed.insert(vpath.clone());
            }
            // A read failure keeps any previous edges, mirroring recall's
            // retention of the last good ingestion.
        }

        // Drop files that vanished.
        let removed: Vec<PathBuf> = sb
            .manifest
            .keys()
            .filter(|k| !seen.contains_key(*k))
            .cloned()
            .collect();
        for key in removed {
            if let Some(meta) = sb.manifest.remove(&key) {
                sb.apply_referrer(&meta.vpath, None);
            }
        }

        // Membership shifts: recompute the not-just-processed referrers whose
        // links named an added/removed basename — their targets may resolve
        // differently now.
        self.recompute_shifts(sb, &shift_basenames, &processed);

        sb.last_reconcile = Some(Instant::now());
    }

    /// Recompute the edges of every referrer linking one of `basenames`,
    /// skipping referrers already recomputed from fresh content in this pass.
    /// A referrer that cannot be read keeps its previous edges (as the
    /// reconcile's read-failure path does).
    fn recompute_shifts(
        &self,
        sb: &mut ScopeBacklinks,
        basenames: &BTreeSet<String>,
        skip: &HashSet<String>,
    ) {
        let resolver = self.storage.resolver();
        let mut affected: BTreeSet<String> = BTreeSet::new();
        for base in basenames {
            if let Some(set) = sb.by_basename.get(base) {
                for referrer in set {
                    if !skip.contains(referrer) {
                        affected.insert(referrer.clone());
                    }
                }
            }
        }
        for referrer in affected {
            let Ok(vpath) = VirtualPath::new(&referrer) else {
                continue;
            };
            let Ok(physical) = resolver.resolve(&sb.scope, &vpath) else {
                continue;
            };
            if let Ok(body) = self.storage.read(&physical) {
                self.ingested.fetch_add(1, Ordering::AcqRel);
                let edges = compute_link_edges(&body, &sb.scope, resolver, &sb.forward);
                sb.apply_referrer(&referrer, Some(edges));
            }
        }
    }

    /// Upsert or remove a single physical path in one scope's index (the
    /// synchronous own-write/delete path). A membership change additionally
    /// recomputes the referrers linking the note's basename: adding or removing
    /// a note can re-point their existing links.
    fn apply_path(&self, sb: &mut ScopeBacklinks, region: Region, physical: &PhysicalPath) {
        let resolver = self.storage.resolver();
        let key = physical.as_path().to_path_buf();
        let vpath: Option<String> = match region {
            Region::InsideAgentsFolder => resolver
                .strip_suffix(physical.as_path(), &sb.scope)
                .map(|v| v.as_str().to_string()),
            Region::OutsideAgentsFolder => physical
                .as_path()
                .strip_prefix(resolver.vault_root())
                .ok()
                .and_then(camino::Utf8Path::from_path)
                .map(|p| p.as_str().to_string()),
        };
        let Some(vpath) = vpath else {
            // Not part of this scope's region (e.g. another scope's file).
            return;
        };
        let clean = vpath.strip_suffix(".md").unwrap_or(&vpath).to_string();
        match std::fs::metadata(physical.as_path()) {
            Ok(meta) => {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let size = meta.len();
                let is_new = !sb.forward.contains(&clean);
                if is_new {
                    sb.forward.insert(&vpath, region);
                }
                if let Ok(body) = self.storage.read(physical) {
                    self.ingested.fetch_add(1, Ordering::AcqRel);
                    let edges = compute_link_edges(&body, &sb.scope, resolver, &sb.forward);
                    sb.apply_referrer(&vpath, Some(edges));
                    sb.manifest.insert(
                        key,
                        FileMeta {
                            vpath: vpath.clone(),
                            mtime,
                            size,
                        },
                    );
                }
                // A new note can re-point existing links carrying its basename
                // (own-scope-preferred tie-break, dangling-link activation).
                if is_new {
                    let skip = HashSet::from([vpath.clone()]);
                    self.recompute_shifts(
                        sb,
                        &BTreeSet::from([link_target_basename(&vpath)]),
                        &skip,
                    );
                }
            }
            Err(_) => {
                let was_indexed = sb.forward.contains(&clean);
                sb.forward.remove(&vpath);
                sb.apply_referrer(&vpath, None);
                sb.manifest.remove(&key);
                // A removal can re-point the links that used to resolve to it.
                if was_indexed {
                    let skip = HashSet::from([vpath.clone()]);
                    self.recompute_shifts(
                        sb,
                        &BTreeSet::from([link_target_basename(&vpath)]),
                        &skip,
                    );
                }
            }
        }
    }
}

// --- persistence (MUNINN_RECALL_INDEX_DIR, recall-tantivy builds) ---

/// The backlink snapshot format version. Bump on any change to the serialized
/// layout; snapshots carrying another version are discarded and rebuilt.
#[cfg(feature = "recall-tantivy")]
const SNAPSHOT_VERSION: u32 = 1;

/// The snapshot file inside a scope's persisted directory.
#[cfg(feature = "recall-tantivy")]
const SNAPSHOT_FILE: &str = "index.json";

/// The serialized form of one scope's reverse index —
/// `<fingerprint>/backlinks/scope-<hash>/index.json`. The fingerprint layer and
/// the identity-checked directory come from the recall persistence layout.
#[cfg(feature = "recall-tantivy")]
#[derive(serde::Serialize, serde::Deserialize)]
struct ScopeSnapshot {
    version: u32,
    scope: String,
    notes: Vec<NoteRecord>,
}

#[cfg(feature = "recall-tantivy")]
#[derive(serde::Serialize, serde::Deserialize)]
struct NoteRecord {
    /// The note's clean virtual path (extension retained).
    vpath: String,
    mtime_secs: u64,
    mtime_nanos: u32,
    size: u64,
    /// Resolved out-edge targets (clean paths, `.md` stripped).
    targets: Vec<String>,
    /// Raw link-target basenames (shift-detection keys).
    basenames: Vec<String>,
}

#[cfg(feature = "recall-tantivy")]
impl BacklinkEngine {
    /// Seed a scope's index from its persisted snapshot. A missing snapshot
    /// leaves the scope cold; a corrupt or foreign one is discarded and rebuilt
    /// from the vault (the reconcile that follows re-reads everything).
    fn load_persisted(&self, sb: &mut ScopeBacklinks) {
        let Some(persist) = &self.persist else {
            return;
        };
        let Some(dir) = persist.backlink_scope_dir(&sb.scope) else {
            return;
        };
        let Ok(bytes) = std::fs::read(dir.join(SNAPSHOT_FILE)) else {
            return; // no snapshot yet: cold build
        };
        match serde_json::from_slice::<ScopeSnapshot>(&bytes) {
            Ok(snap) if snap.version == SNAPSHOT_VERSION && snap.scope == sb.scope => {
                let resolver = self.storage.resolver();
                for rec in snap.notes {
                    let Ok(vpath) = VirtualPath::new(&rec.vpath) else {
                        continue;
                    };
                    let Ok(physical) = resolver.resolve(&sb.scope, &vpath) else {
                        continue;
                    };
                    sb.forward
                        .insert(&rec.vpath, resolver.detect_region(&vpath));
                    sb.apply_referrer(
                        &rec.vpath,
                        Some(ReferrerEdges {
                            targets: rec.targets.into_iter().collect(),
                            basenames: rec.basenames.into_iter().collect(),
                        }),
                    );
                    sb.manifest.insert(
                        physical.as_path().to_path_buf(),
                        FileMeta {
                            vpath: rec.vpath,
                            mtime: SystemTime::UNIX_EPOCH
                                + Duration::new(rec.mtime_secs, rec.mtime_nanos),
                            size: rec.size,
                        },
                    );
                }
                sb.forward.sort();
            }
            _ => {
                tracing::warn!(
                    dir = %dir.display(),
                    "discarding a corrupt persisted backlink index and rebuilding from the vault"
                );
                let _ = crate::recall::persist::wipe_region_index(&dir);
            }
        }
    }

    /// Serialize a scope's index under the recall fingerprint layer. Snapshots
    /// refresh at build, reconcile, and eviction boundaries — never on the
    /// write path, so writes stay proportional to the changed note; the startup
    /// stat-diff reconcile closes any gap left by writes since the last
    /// snapshot.
    fn persist_scope(&self, sb: &ScopeBacklinks) {
        let Some(persist) = &self.persist else {
            return;
        };
        let Some(dir) = persist.backlink_scope_dir(&sb.scope) else {
            return;
        };
        let mut notes = Vec::with_capacity(sb.manifest.len());
        for meta in sb.manifest.values() {
            let Some(edges) = sb.referrers.get(&meta.vpath) else {
                continue;
            };
            let (mtime_secs, mtime_nanos) = meta
                .mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| (d.as_secs(), d.subsec_nanos()))
                .unwrap_or((0, 0));
            notes.push(NoteRecord {
                vpath: meta.vpath.clone(),
                mtime_secs,
                mtime_nanos,
                size: meta.size,
                targets: edges.targets.iter().cloned().collect(),
                basenames: edges.basenames.iter().cloned().collect(),
            });
        }
        let snap = ScopeSnapshot {
            version: SNAPSHOT_VERSION,
            scope: sb.scope.clone(),
            notes,
        };
        let Ok(bytes) = serde_json::to_vec(&snap) else {
            return;
        };
        // Temp file + rename, so a crash mid-write cannot leave a torn snapshot.
        let tmp = dir.join(format!("{SNAPSHOT_FILE}.tmp"));
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(SNAPSHOT_FILE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RecallBackendKind;
    use crate::scheme::Scheme;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;

    const BOTH: &[Region] = &[Region::InsideAgentsFolder, Region::OutsideAgentsFolder];

    fn bare_index(entries: &[(&str, Region)]) -> LinkIndex {
        let mut idx = LinkIndex::default();
        for (path, region) in entries {
            idx.insert(path, *region);
        }
        idx.sort();
        idx
    }

    fn test_resolver(root: &std::path::Path) -> PathResolver {
        PathResolver::new(
            root.canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        )
    }

    fn engine_over(tmp: &TempDir, freshness: Duration, max_resident: usize) -> BacklinkEngine {
        let storage = Arc::new(Storage::new(test_resolver(tmp.path()), true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Simple,
            watch_debounce: Duration::ZERO,
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: max_resident,
            freshness,
            index_dir: None,
        };
        BacklinkEngine::new(storage, BOTH.to_vec(), &config)
    }

    /// A two-scope fixture vault with shared notes, exercising own-scope and
    /// shared referrers, ambiguous basenames, markdown forms, and dangles.
    fn fixture_vault() -> TempDir {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("The Rust note.")
            .unwrap();
        tmp.child("Agents/jarvis.tony/notes/memo.jarvis.tony.md")
            .write_str("see [[rust.jarvis.tony]] and [[Lang/rust]]")
            .unwrap();
        tmp.child("Agents/jarvis.tony/notes/md.jarvis.tony.md")
            .write_str("[doc](Agents/jarvis.tony/topics/rust.jarvis.tony.md)")
            .unwrap();
        tmp.child("Agents/jarvis.tony/notes/dangle.jarvis.tony.md")
            .write_str("[[ghost]]")
            .unwrap();
        tmp.child("Agents/jarvis.tony/notes/plain.jarvis.tony.md")
            .write_str("unrelated")
            .unwrap();
        tmp.child("Agents/jarvis.sam/notes/smemo.jarvis.sam.md")
            .write_str("[[rust]]")
            .unwrap();
        tmp.child("Lang/rust.md")
            .write_str("The shared rust note.")
            .unwrap();
        tmp.child("Actions/release.md")
            .write_str("tracks [[rust]]")
            .unwrap();
        tmp
    }

    /// An instant `secs` after the Unix epoch.
    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn set_mtime(path: &std::path::Path, secs: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t(secs))
            .unwrap();
    }

    // --- 1.1: reverse index data model ---

    #[test]
    fn backlink_index_edge_insert_remove_lookup() {
        let mut idx = BacklinkIndex::default();
        assert!(idx.referrers_of("Agents/topics/rust").is_empty());

        idx.add_edge("Agents/topics/rust", "Agents/notes/memo.md");
        idx.add_edge("Agents/topics/rust", "Actions/release.md");
        // A duplicate insert is idempotent (a referrer lists once).
        idx.add_edge("Agents/topics/rust", "Agents/notes/memo.md");
        idx.add_edge("Lang/rust", "Agents/notes/memo.md");
        assert_eq!(
            idx.referrers_of("Agents/topics/rust"),
            vec!["Actions/release.md", "Agents/notes/memo.md"]
        );

        idx.remove_edge("Agents/topics/rust", "Actions/release.md");
        assert_eq!(
            idx.referrers_of("Agents/topics/rust"),
            vec!["Agents/notes/memo.md"]
        );
        // Removing the last referrer drops the target entry entirely.
        idx.remove_edge("Agents/topics/rust", "Agents/notes/memo.md");
        assert!(idx.referrers_of("Agents/topics/rust").is_empty());
        // Removing an unknown edge is a no-op.
        idx.remove_edge("Agents/topics/rust", "Agents/notes/memo.md");
        assert_eq!(idx.referrers_of("Lang/rust"), vec!["Agents/notes/memo.md"]);
    }

    #[test]
    fn apply_referrer_replaces_and_removes_edges() {
        let edges = |targets: &[&str], bases: &[&str]| ReferrerEdges {
            targets: targets.iter().map(|s| s.to_string()).collect(),
            basenames: bases.iter().map(|s| s.to_string()).collect(),
        };
        let mut sb = ScopeBacklinks::new("jarvis.tony");
        sb.apply_referrer(
            "Agents/notes/memo.md",
            Some(edges(&["Agents/topics/rust"], &["rust"])),
        );
        sb.apply_referrer(
            "Actions/release.md",
            Some(edges(&["Agents/topics/rust"], &["rust"])),
        );
        assert_eq!(
            sb.reverse.referrers_of("Agents/topics/rust"),
            vec!["Actions/release.md", "Agents/notes/memo.md"]
        );
        assert_eq!(
            sb.by_basename["rust"].iter().cloned().collect::<Vec<_>>(),
            vec!["Actions/release.md", "Agents/notes/memo.md"]
        );

        // Replacement drops the old target and keeps the basename inversion.
        sb.apply_referrer(
            "Agents/notes/memo.md",
            Some(edges(&["Lang/rust"], &["rust"])),
        );
        assert_eq!(
            sb.reverse.referrers_of("Agents/topics/rust"),
            vec!["Actions/release.md"]
        );
        assert_eq!(
            sb.reverse.referrers_of("Lang/rust"),
            vec!["Agents/notes/memo.md"]
        );

        // Removal cleans up both maps; emptied basename entries disappear.
        sb.apply_referrer("Agents/notes/memo.md", None);
        assert!(sb.reverse.referrers_of("Lang/rust").is_empty());
        assert_eq!(
            sb.by_basename["rust"].iter().cloned().collect::<Vec<_>>(),
            vec!["Actions/release.md"]
        );
        sb.apply_referrer("Actions/release.md", None);
        assert!(!sb.by_basename.contains_key("rust"));
        assert!(sb.referrers.is_empty());
    }

    // --- 1.2: compute_out_edges mirrors references_to ---

    /// For every scenario content, the out-edge set must contain exactly the
    /// targets `references_to` confirms, for every candidate target.
    fn assert_edges_match(
        content: &str,
        idx: &LinkIndex,
        resolver: &PathResolver,
        scope: &str,
        candidates: &[&str],
    ) {
        let edges = compute_out_edges(content, scope, resolver, idx);
        for target in candidates {
            assert_eq!(
                edges.contains(*target),
                crate::wikilink::references_to(content, target, scope, resolver, idx),
                "target {target} in {content:?}"
            );
        }
    }

    #[test]
    fn compute_out_edges_matches_references_to_on_every_link_form() {
        let tmp = TempDir::new().unwrap();
        let r = test_resolver(tmp.path());
        let idx = bare_index(&[
            ("Agents/topics/rust.md", Region::InsideAgentsFolder),
            ("Actions/release.md", Region::OutsideAgentsFolder),
        ]);
        let candidates = [
            "Agents/topics/rust",
            "Actions/release",
            "Lang/rust",
            "Agents/topics/other",
        ];
        for content in [
            "see [[rust.jarvis.tony]]",
            "see [[rust]]",
            "see [[rust.jarvis.tony|the Rust note]] and [[rust.jarvis.tony#install]]",
            "embed ![[rust.jarvis.tony]]",
            "[doc](Agents/jarvis.tony/topics/rust.jarvis.tony.md)",
            "shared markdown [r](Actions/release.md)",
            "[[rust.md]] and ![[rust.md#install]] and [[rust.md|alias]]",
            // A frontmatter-style property line: the byte scan finds links
            // anywhere in the file, properties included.
            "---\nrelated: \"[[rust.jarvis.tony]]\"\n---\nno body links",
            "no links here",
            "[[ghost]] and [g](ghost.md)",
        ] {
            assert_edges_match(content, &idx, &r, "jarvis.tony", &candidates);
        }
        assert!(
            compute_out_edges("see [[rust.jarvis.tony]]", "jarvis.tony", &r, &idx)
                .contains("Agents/topics/rust")
        );
        assert!(compute_out_edges("no links here", "jarvis.tony", &r, &idx).is_empty());
    }

    #[test]
    fn compute_out_edges_follows_forward_tie_break() {
        let tmp = TempDir::new().unwrap();
        let r = test_resolver(tmp.path());
        let idx = bare_index(&[
            ("Agents/topics/rust.md", Region::InsideAgentsFolder),
            ("Lang/rust.md", Region::OutsideAgentsFolder),
        ]);
        // `[[rust]]` forward-resolves own-scope; it is an out-edge only for
        // that entry.
        let edges = compute_out_edges("[[rust]]", "jarvis.tony", &r, &idx);
        assert!(edges.contains("Agents/topics/rust"));
        assert!(!edges.contains("Lang/rust"));
        // A qualified link selects the shared entry.
        let edges = compute_out_edges("[[Lang/rust]]", "jarvis.tony", &r, &idx);
        assert!(edges.contains("Lang/rust"));
        assert!(!edges.contains("Agents/topics/rust"));
    }

    #[test]
    fn compute_out_edges_dangling_produces_nothing_but_keys_the_basename() {
        let tmp = TempDir::new().unwrap();
        let r = test_resolver(tmp.path());
        let idx = bare_index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        assert!(
            compute_out_edges("[[ghost]] and [g](ghost.md)", "jarvis.tony", &r, &idx).is_empty()
        );
        // ...but the dangling basename is still a shift key, so a later
        // membership change recomputes this note.
        let edges = compute_link_edges("[[ghost]]", "jarvis.tony", &r, &idx);
        assert!(edges.basenames.contains("ghost"));
    }

    // --- 2.1/2.2: eager build, query seam ---

    /// The pre-index computation: a full content scan per target, as the
    /// correctness oracle.
    fn scan_backlinks(storage: &Storage, scope: &str, target_clean: &str) -> Vec<String> {
        let resolver = storage.resolver();
        let index = storage.build_link_index(scope, BOTH).unwrap();
        let mut out = BTreeSet::new();
        for referrer in storage.list_visible(scope, BOTH).unwrap() {
            let Ok(physical) = resolver.resolve(scope, &referrer) else {
                continue;
            };
            let Ok(content) = storage.read(&physical) else {
                continue;
            };
            if crate::wikilink::references_to(&content, target_clean, scope, resolver, &index) {
                out.insert(referrer.as_str().to_string());
            }
        }
        out.into_iter().collect()
    }

    #[test]
    fn build_matches_scan_on_fixture_vault() {
        let tmp = fixture_vault();
        let engine = engine_over(&tmp, Duration::from_secs(3600), 256);
        engine.warm();
        let storage = Storage::new(test_resolver(tmp.path()), true, false, &[]);
        for scope in ["jarvis.tony", "jarvis.sam"] {
            // Every visible note as a target, plus a non-target.
            let mut targets: Vec<String> = storage
                .list_visible(scope, BOTH)
                .unwrap()
                .iter()
                .map(|p| {
                    p.as_str()
                        .strip_suffix(".md")
                        .unwrap_or(p.as_str())
                        .to_string()
                })
                .collect();
            targets.push("Agents/topics/missing".to_string());
            for target in targets {
                assert_eq!(
                    engine.backlinks(scope, &target),
                    scan_backlinks(&storage, scope, &target),
                    "scope {scope} target {target}"
                );
            }
        }

        // The spec scenarios, explicitly.
        assert_eq!(
            engine.backlinks("jarvis.tony", "Agents/topics/rust"),
            vec![
                "Actions/release.md",
                "Agents/notes/md.md",
                "Agents/notes/memo.md"
            ]
        );
        // Under sam (no own-scope rust) the shared referrer resolves to
        // Lang/rust instead.
        assert_eq!(
            engine.backlinks("jarvis.sam", "Lang/rust"),
            vec!["Actions/release.md", "Agents/notes/smemo.md"]
        );
        // Dangling links contribute nothing.
        assert!(
            engine
                .backlinks("jarvis.tony", "Agents/notes/ghost")
                .is_empty()
        );
    }

    #[test]
    fn eager_build_then_first_query_reads_nothing() {
        let tmp = fixture_vault();
        let engine = engine_over(&tmp, Duration::from_secs(3600), 256);
        assert!(!engine.is_ready());
        engine.warm();
        assert!(engine.is_ready());
        // tony sees 5 own + 2 shared notes; sam sees 1 own + 2 shared.
        assert_eq!(engine.ingested_count(), 10);
        assert_eq!(engine.resident_scope_count(), 2);

        // The first backlink query is served from the index: no note content
        // is read again, and the result is the full referrer set.
        assert_eq!(
            engine.backlinks("jarvis.tony", "Agents/topics/rust").len(),
            3
        );
        assert_eq!(engine.ingested_count(), 10);
        assert_eq!(engine.backlinks("jarvis.sam", "Lang/rust").len(), 2);
        assert_eq!(engine.ingested_count(), 10);

        // A repeated warm stays silent and builds nothing twice.
        engine.warm();
        assert_eq!(engine.ingested_count(), 10);
    }

    #[test]
    fn single_tenant_build_serves_empty_scope_queries() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/topics/rust.md")
            .write_str("the rust note")
            .unwrap();
        tmp.child("Agents/notes/memo.md")
            .write_str("see [[rust]]")
            .unwrap();
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse("").unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Simple,
            watch_debounce: Duration::ZERO,
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: Duration::from_secs(3600),
            index_dir: None,
        };
        let engine = BacklinkEngine::new(storage, BOTH.to_vec(), &config);
        engine.warm();
        assert_eq!(
            engine.backlinks("", "Agents/topics/rust"),
            vec!["Agents/notes/memo.md"]
        );
    }

    // --- 2.3: own-write/delete hooks and reconcile ---

    #[test]
    fn own_write_is_reflected_immediately_without_reconcile() {
        let tmp = fixture_vault();
        let engine = engine_over(&tmp, Duration::from_secs(3600), 256);
        engine.warm();
        let after_build = engine.ingested_count();

        // A new own-scope note linking the existing target, notified
        // synchronously (as the toolbox does on every write path).
        tmp.child("Agents/jarvis.tony/notes/fresh.jarvis.tony.md")
            .write_str("see [[rust.jarvis.tony]]")
            .unwrap();
        let resolver = test_resolver(tmp.path());
        let vpath = VirtualPath::new("Agents/notes/fresh.md").unwrap();
        let physical = resolver.resolve("jarvis.tony", &vpath).unwrap();
        engine.on_write("jarvis.tony", Region::InsideAgentsFolder, &physical);

        let backlinks = engine.backlinks("jarvis.tony", "Agents/topics/rust");
        assert!(backlinks.contains(&"Agents/notes/fresh.md".to_string()));
        // Only the one new note was read; no reconcile ran (frozen freshness).
        assert_eq!(engine.ingested_count(), after_build + 1);
    }

    /// The shift-aware maintenance: creating or deleting a note re-points other
    /// notes' existing links (own-scope-preferred tie-break and dangling-link
    /// activation), without any reconcile.
    #[test]
    fn membership_shift_repoints_existing_links() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Actions/release.md")
            .write_str("tracks [[rust]]")
            .unwrap();
        tmp.child("Lang/rust.md").write_str("shared rust").unwrap();
        tmp.child("Agents/jarvis.tony/notes/memo.jarvis.tony.md")
            .write_str("see [[rust]]")
            .unwrap();
        let engine = engine_over(&tmp, Duration::from_secs(3600), 256);
        engine.warm();
        // With no own-scope rust, both `[[rust]]` links resolve to the shared
        // note.
        assert_eq!(
            engine.backlinks("jarvis.tony", "Lang/rust"),
            vec!["Actions/release.md", "Agents/notes/memo.md"]
        );

        // Creating the own-scope rust re-points both links (own-scope
        // preferred).
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("own rust")
            .unwrap();
        let resolver = test_resolver(tmp.path());
        let physical = resolver
            .resolve(
                "jarvis.tony",
                &VirtualPath::new("Agents/topics/rust.md").unwrap(),
            )
            .unwrap();
        engine.on_write("jarvis.tony", Region::InsideAgentsFolder, &physical);
        assert_eq!(
            engine.backlinks("jarvis.tony", "Agents/topics/rust"),
            vec!["Actions/release.md", "Agents/notes/memo.md"]
        );
        assert!(engine.backlinks("jarvis.tony", "Lang/rust").is_empty());

        // Deleting it shifts resolution back to the shared note.
        std::fs::remove_file(physical.as_path()).unwrap();
        engine.on_delete("jarvis.tony", Region::InsideAgentsFolder, &physical);
        assert_eq!(
            engine.backlinks("jarvis.tony", "Lang/rust"),
            vec!["Actions/release.md", "Agents/notes/memo.md"]
        );
        assert!(
            engine
                .backlinks("jarvis.tony", "Agents/topics/rust")
                .is_empty()
        );
    }

    /// A shared write updates every resident scope, each resolving the shared
    /// note's links under its own visible set.
    #[test]
    fn shared_write_updates_every_resident_scope() {
        let tmp = fixture_vault();
        let engine = engine_over(&tmp, Duration::from_secs(3600), 256);
        engine.warm();
        // Rewrite the shared note to link sam's own note and Lang/rust.
        tmp.child("Actions/release.md")
            .write_str("tracks [[smemo.jarvis.sam]] and [[Lang/rust]]")
            .unwrap();
        let resolver = test_resolver(tmp.path());
        let physical = resolver
            .resolve("", &VirtualPath::new("Actions/release.md").unwrap())
            .unwrap();
        engine.on_write("jarvis.sam", Region::OutsideAgentsFolder, &physical);

        assert_eq!(
            engine.backlinks("jarvis.sam", "Agents/notes/smemo"),
            vec!["Actions/release.md"]
        );
        // Under sam, Lang/rust keeps its existing own referrer too.
        assert_eq!(
            engine.backlinks("jarvis.sam", "Lang/rust"),
            vec!["Actions/release.md", "Agents/notes/smemo.md"]
        );
        // Under tony, `[[smemo.jarvis.sam]]` is not sam's note: it dangles (the
        // suffix does not match tony's scope), so only the Lang/rust edge lands.
        assert_eq!(
            engine.backlinks("jarvis.tony", "Lang/rust"),
            vec!["Actions/release.md", "Agents/notes/memo.md"]
        );
    }

    #[test]
    fn external_edit_is_picked_up_by_the_stat_diff_reconcile() {
        let tmp = fixture_vault();
        // Freshness zero: every query reconciles by stat-diff (no watcher).
        let engine = engine_over(&tmp, Duration::ZERO, 256);
        engine.warm();
        assert!(
            !engine
                .backlinks("jarvis.tony", "Lang/rust")
                .contains(&"Agents/notes/plain.md".to_string())
        );

        // An external edit adds a link (as Obsidian would).
        let edited = tmp
            .path()
            .join("Agents/jarvis.tony/notes/plain.jarvis.tony.md");
        std::fs::write(&edited, "now links [[Lang/rust]]").unwrap();
        set_mtime(&edited, 9_000);

        // Under tony, Lang/rust's referrers are the explicit [[Lang/rust]]
        // links (the shared release note's [[rust]] resolves own-scope).
        assert_eq!(
            engine.backlinks("jarvis.tony", "Lang/rust"),
            vec!["Agents/notes/memo.md", "Agents/notes/plain.md"]
        );
    }

    #[test]
    fn external_delete_is_picked_up_by_the_stat_diff_reconcile() {
        let tmp = fixture_vault();
        let engine = engine_over(&tmp, Duration::ZERO, 256);
        engine.warm();
        assert_eq!(
            engine.backlinks("jarvis.tony", "Agents/topics/rust").len(),
            3
        );

        std::fs::remove_file(
            tmp.path()
                .join("Agents/jarvis.tony/notes/memo.jarvis.tony.md"),
        )
        .unwrap();
        assert_eq!(
            engine.backlinks("jarvis.tony", "Agents/topics/rust"),
            vec!["Actions/release.md", "Agents/notes/md.md"]
        );
        // And the memo's edge to Lang/rust is gone too.
        assert!(engine.backlinks("jarvis.tony", "Lang/rust").is_empty());
    }

    /// An external membership change shifts resolution the same way a
    /// server-side one does, within the reconcile window.
    #[test]
    fn external_membership_change_shifts_resolution_on_reconcile() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Actions/release.md")
            .write_str("tracks [[rust]]")
            .unwrap();
        tmp.child("Lang/rust.md").write_str("shared rust").unwrap();
        let engine = engine_over(&tmp, Duration::ZERO, 256);
        engine.warm();
        assert_eq!(
            engine.backlinks("jarvis.tony", "Lang/rust"),
            vec!["Actions/release.md"]
        );

        // Obsidian creates the own-scope note: the shared referrer's link
        // re-points to it on the next query's reconcile.
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("own rust")
            .unwrap();
        assert_eq!(
            engine.backlinks("jarvis.tony", "Agents/topics/rust"),
            vec!["Actions/release.md"]
        );
        assert!(engine.backlinks("jarvis.tony", "Lang/rust").is_empty());
    }

    #[test]
    fn watcher_marks_the_engine_dirty_for_the_next_query() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
            .write_str("rust")
            .unwrap();
        tmp.child("Agents/jarvis.tony/notes/watcher.jarvis.tony.md")
            .write_str("no links yet")
            .unwrap();
        // Long freshness: only the watcher's dirty flag can force a reconcile.
        let engine = engine_over(&tmp, Duration::from_secs(3600), 256);
        engine.start_watcher();
        engine.warm();
        assert!(
            engine
                .backlinks("jarvis.tony", "Agents/topics/rust")
                .is_empty()
        );

        // An external edit (as Obsidian would) adds a link.
        std::fs::write(
            tmp.path()
                .join("Agents/jarvis.tony/notes/watcher.jarvis.tony.md"),
            "now links [[rust.jarvis.tony]]",
        )
        .unwrap();

        // The watcher fires asynchronously; poll within the reconcile window.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let got = engine.backlinks("jarvis.tony", "Agents/topics/rust");
            if got == vec!["Agents/notes/watcher.md".to_string()] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "watcher-driven reconcile did not pick up the external edit: {got:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // --- 2.4: residency / eviction ---

    #[test]
    fn resident_scopes_never_exceed_the_cap() {
        let tmp = TempDir::new().unwrap();
        const SCOPES: usize = 4;
        const MAX_RESIDENT: usize = 2;
        for s in 0..SCOPES {
            let scope = format!("jarvis.user{s}");
            tmp.child(format!("Agents/{scope}/topics/target.{scope}.md"))
                .write_str("the target")
                .unwrap();
            tmp.child(format!("Agents/{scope}/notes/ref.{scope}.md"))
                .write_str(&format!("links [[target.{scope}]]"))
                .unwrap();
        }
        let engine = engine_over(&tmp, Duration::from_secs(3600), MAX_RESIDENT);
        engine.warm();

        for s in 0..SCOPES {
            let scope = format!("jarvis.user{s}");
            assert_eq!(
                engine.backlinks(&scope, "Agents/topics/target"),
                vec!["Agents/notes/ref.md".to_string()],
                "scope {scope} must see exactly its own referrer"
            );
            assert!(
                engine.resident_scope_count() <= MAX_RESIDENT,
                "resident scopes {} exceed the cap {MAX_RESIDENT} after querying {scope}",
                engine.resident_scope_count()
            );
        }
    }

    // --- 4.1: persistent index (MUNINN_RECALL_INDEX_DIR) ---

    /// Every regular file under `root`, recursively, sorted.
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

    /// An engine over `vault`, optionally persisting under `index_dir`. The
    /// freshness window is an hour and no watcher runs, so the only reconciles
    /// are the ones a test triggers — which makes `ingested_count` an exact
    /// count of the note bodies the engine chose to read.
    #[cfg(feature = "recall-tantivy")]
    fn persisted_engine(
        vault: &std::path::Path,
        index_dir: Option<&std::path::Path>,
        max_resident: usize,
        scheme: &str,
    ) -> BacklinkEngine {
        let resolver = PathResolver::new(
            vault.canonicalize().unwrap(),
            camino::Utf8PathBuf::from("Agents"),
            Scheme::parse(scheme).unwrap(),
        );
        let storage = Arc::new(Storage::new(resolver, true, false, &[]));
        let config = RecallConfig {
            backend: RecallBackendKind::Simple,
            watch_debounce: Duration::from_secs(3600),
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: max_resident,
            freshness: Duration::from_secs(3600),
            index_dir: index_dir.map(|p| p.to_path_buf()),
        };
        BacklinkEngine::new(storage, BOTH.to_vec(), &config)
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
    /// manifest recovered from the persisted snapshot makes every file compare
    /// equal under the stat-diff.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn restart_over_an_unchanged_vault_reindexes_nothing() {
        let vault = fixture_vault();
        let index = TempDir::new().unwrap();

        let cold = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        cold.warm();
        assert_eq!(cold.ingested_count(), 10, "the cold build reads every note");
        let before = cold.backlinks("jarvis.tony", "Agents/topics/rust");
        drop(cold);

        let warm = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        warm.warm();
        assert_eq!(
            warm.ingested_count(),
            0,
            "a restart over an unchanged vault must not re-read any note"
        );
        let after = warm.backlinks("jarvis.tony", "Agents/topics/rust");
        assert_eq!(before, after);
        assert_eq!(after.len(), 3);
        // Cross-scope isolation survives persistence.
        assert!(
            warm.backlinks("jarvis.tony", "Agents/notes/smemo")
                .is_empty()
        );
    }

    /// Everything that changed while the server was down is reconciled on the
    /// next start, and only that.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn changes_made_while_down_are_reconciled_and_nothing_else_is_reread() {
        let vault = fixture_vault();
        let index = TempDir::new().unwrap();

        let cold = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        cold.warm();
        assert!(
            cold.backlinks("jarvis.tony", "Agents/notes/memo")
                .is_empty()
        );
        drop(cold);

        // While "down": one note added (linking memo), one edited, one deleted.
        vault
            .child("Agents/jarvis.tony/topics/traits.jarvis.tony.md")
            .write_str("Traits describe shared behaviour, see [[memo.jarvis.tony]].")
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
                .join("Agents/jarvis.tony/notes/md.jarvis.tony.md"),
        )
        .unwrap();

        let warm = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        warm.warm();
        assert_eq!(
            warm.ingested_count(),
            2,
            "only the added and the edited note should be read"
        );

        // The addition is visible...
        assert_eq!(
            warm.backlinks("jarvis.tony", "Agents/notes/memo"),
            vec!["Agents/topics/traits.md"]
        );
        // ...and the deleted referrer is gone.
        let rust = warm.backlinks("jarvis.tony", "Agents/topics/rust");
        assert!(!rust.contains(&"Agents/notes/md.md".to_string()));
        assert_eq!(rust, vec!["Actions/release.md", "Agents/notes/memo.md"]);
    }

    /// A damaged snapshot is discarded rather than trusted or fatal.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn a_corrupted_persisted_index_is_wiped_and_rebuilt() {
        let vault = fixture_vault();
        let index = TempDir::new().unwrap();

        let cold = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        cold.warm();
        let before = cold.backlinks("jarvis.tony", "Agents/topics/rust");
        drop(cold);

        // Overwrite every snapshot with garbage.
        let mut corrupted = 0usize;
        for file in walk_files(index.path()) {
            if file.file_name().is_some_and(|n| n == "index.json") {
                std::fs::write(&file, b"not json").unwrap();
                corrupted += 1;
            }
        }
        assert_eq!(corrupted, 2, "one snapshot per scope: {corrupted}");

        let warm = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        warm.warm();
        assert_eq!(
            warm.ingested_count(),
            10,
            "a corrupted index must be rebuilt from the vault in full"
        );
        assert_eq!(warm.backlinks("jarvis.tony", "Agents/topics/rust"), before);
        // Every rebuilt directory is still claimed by its own scope.
        let markers = walk_files(index.path())
            .into_iter()
            .filter(|p| p.file_name().is_some_and(|n| n == "region.id"))
            .count();
        assert_eq!(markers, 2, "one identity marker per scope");
    }

    /// A configuration change that alters what gets indexed lands on a
    /// different fingerprint, so the previous snapshot is neither reused nor
    /// left on disk.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn a_changed_scheme_invalidates_and_removes_the_persisted_index() {
        let vault = fixture_vault();
        let index = TempDir::new().unwrap();

        let first = persisted_engine(vault.path(), Some(index.path()), 256, "<agent>.<user>");
        first.warm();
        drop(first);
        let before = fingerprint_dirs(index.path());
        assert_eq!(before.len(), 1, "one fingerprint directory: {before:?}");

        // A different scheme shapes the indexed view, so the fingerprint
        // changes and the stale directory is pruned.
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
        assert!(
            second
                .backlinks("jarvis.tony", "Agents/topics/rust")
                .contains(&"Agents/notes/memo.md".to_string()),
            "results must be correct under the new fingerprint"
        );
    }

    /// With persistence on, re-residence after eviction is a reopen plus a
    /// stat-diff, not a recompute.
    #[cfg(feature = "recall-tantivy")]
    #[test]
    fn re_resident_scopes_reopen_instead_of_recomputing() {
        let vault = fixture_vault();
        let index = TempDir::new().unwrap();
        // One resident scope, so each cross-scope query evicts the other.
        let engine = persisted_engine(vault.path(), Some(index.path()), 1, "<agent>.<user>");
        engine.warm();
        let after_build = engine.ingested_count();
        assert_eq!(after_build, 10);

        for _ in 0..3 {
            let tony = engine.backlinks("jarvis.tony", "Agents/topics/rust");
            assert_eq!(tony.len(), 3, "tony's edges survive the round trip");
            let sam = engine.backlinks("jarvis.sam", "Lang/rust");
            assert_eq!(sam.len(), 2, "sam's edges survive the round trip");
            assert!(engine.resident_scope_count() <= 1);
        }
        assert_eq!(
            engine.ingested_count(),
            after_build,
            "re-residence must reopen the persisted index, not recompute the scope"
        );
    }

    // --- 4.2: nothing is written without a configured directory ---

    /// Persistence is opt-in: with no index directory the engine writes nothing
    /// outside the vault (and nothing inside it either).
    #[test]
    fn without_an_index_dir_nothing_is_written_to_disk() {
        let vault = fixture_vault();
        let scratch = TempDir::new().unwrap();
        let engine = engine_over(&vault, Duration::from_secs(3600), 256);
        engine.warm();
        assert_eq!(
            engine.backlinks("jarvis.tony", "Agents/topics/rust").len(),
            3
        );
        assert!(
            walk_files(scratch.path()).is_empty(),
            "an unconfigured engine must not write index files anywhere"
        );
        assert_eq!(
            walk_files(vault.path()).len(),
            8,
            "the vault holds exactly its fixture notes — no index artifacts"
        );
    }
}
