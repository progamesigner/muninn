## Context

See `proposal.md` — Why. Today `collect_backlinks` (`src/tools.rs:1035`) and
`rename_memory_note` Phase 1 (`src/tools.rs:1329`) iterate
`storage.list_visible` and, per referrer, read the file and call
`wikilink::references_to` (`src/wikilink.rs:190`), which re-parses every link and
resolves it against the caller's `LinkIndex` (`src/storage.rs:65` — a forward
basename→notes map). The `LinkIndex` is rebuilt cheaply per call (paths only);
the expensive part is the per-referrer content read + link resolution, which is
O(vault).

The recall engine (`src/recall/mod.rs`) already solves the analogous "maintain
derived state from the vault" problem with: eager startup build, a single state
lock, a synchronous own-write hook, watcher reconcile, periodic stat-diff
reconcile, per-scope indexes + one shared, and optional disk persistence with a
fingerprint layer (`src/recall/tantivy.rs`). The backlink index is a second such
derived structure; we mirror that lifecycle rather than invent a new one.

Key constraint: backlink resolution is **per visible set** (per scope). A shared
referrer's `[[rust]]` resolves to a different target under different scopes'
indexes because of own-scope-preferred tie-breaking. Therefore the reverse index
must be per rendered scope (each scope's own notes + the shared notes indexed as
that scope sees them), not a single global map. This is the reason the earlier
`batch-rename-recall-commit` design deferred a "persistent reverse-link index"
as "materially bigger" — the cost is per-scope, not paid once per vault. We
accept that and bound it by reusing the recall engine's per-scope
residency/eviction model.

## Goals / Non-Goals

**Goals:**
- Backlink discovery cost ∝ result set, not vault size, for both
  `read_memory_note(backlinks)` and rename Phase 1.
- Consistency: server writes reflected immediately; external edits within the
  reconcile window.
- Reuse recall's lifecycle (eager build, watcher, periodic reconcile,
  fingerprint persistence, per-scope residency) so there is no separate
  migration story — stale/!fingerprint ⇒ rebuild from vault.
- No change to the backlink *set* returned, and no tool schema/response changes.

**Non-Goals:**
- Not changing the forward `LinkIndex` or link resolution rules.
- Not making the per-scope build sub-linear; the work moves from per-query to
  once at startup / per reconcile, which is the win. We do not claim O(1) build.
- Not persisting anything when `MUNINN_RECALL_INDEX_DIR` is unset (in-memory,
  rebuilt each startup, like recall's default).
- Not integrating with `batch-rename-recall-commit`'s commit batching beyond
  sharing the same write paths; that change owns Phase 2.

## Decisions

**Per-scope reverse indexes, mirroring `RecallEngine`.** A `BacklinkEngine` holds
`HashMap<RenderedScope, ScopeBacklinks>` under one state lock. Each scope's
`ScopeBacklinks` indexes the scope's own notes *and* the shared notes, resolved
under that scope's visible set — the shared region's referrer view is realized
per scope, so a shared write updates every resident scope. `ScopeBacklinks`
holds the reverse map (`HashMap<target_clean_path, BTreeSet<referrer_clean_path>>`),
a per-referrer record of resolved out-edges and raw linked basenames, an
inverted basename→referrers map, and the scope's forward `LinkIndex`. To
populate, for each note R visible to scope `c`: parse R's links via
`wikilink::rewrite_links` (collector mode, as `references_to` does), resolve
each via the scope's `LinkIndex`, and for each resolved target T add edge R→T.
Shared notes are indexed once per scope (their edges differ per scope).

Alternative considered: a single global reverse map computed under each
referrer's *own* context. Rejected: a shared referrer's `[[rust]]` resolves to a
different target depending on the querying scope (own-scope-preferred), so a
global map would return wrong referrer sets for some callers. Per-scope indexing
is the only correct decomposition.

**Reuse the recall engine's lifecycle hooks.** `BacklinkEngine` gets the same
triggers as `RecallEngine`: eager startup build over `storage.list_visible` per
scope, a synchronous `on_write(scope, region, physical)` updating only the
affected scope's (and shared's) index, watcher reconcile (idempotent by file
metadata), periodic stat-diff reconcile, and per-scope residency/eviction under
`max_resident_scopes`. This avoids a new invalidation story and keeps the two
structures consistent — they are updated by the same writes.

**Incremental update = recompute the changed note's out-edges, plus the
referrers whose resolution the change can shift.** A backlink edge is a function
of the whole visible set, not only the referrer's content: `resolve_target`'s
tie-breaks (own-scope preferred, then smallest clean path) mean adding or
removing a note can re-point *other* notes' existing links — a new own-scope
note wins its basename from a shared note, a deletion hands it back, and a new
note activates previously dangling links. Those shifts are exactly keyed by
basename: `resolve_target` matches candidates by basename, so a membership
change to a note with basename `b` can only alter the resolution of links whose
target basename is `b`. On `on_write`, read the one note's content, compute its
new out-edge set (resolve links against the scope's forward `LinkIndex`), remove
its previous edges from every target it used to point to, and add the new edges.
A delete removes its edges. A **membership** change (create/delete/rename)
additionally recomputes the out-edges of the referrers linking basename `b`
(found via the inverted basename→referrers map) — O(affected referrers), still
independent of vault size. A content-only edit stays O(links in the note), so
server writes stay cheap. The stat-diff reconcile feeds added/removed files
through the same basename-keyed recompute, so external membership changes
converge within the reconcile window. `rename_memory_note` already computes each
referrer's rewritten content in Phase 1; the index update is driven by the same
`on_write` hook this engine exposes, so it composes with the batch-commit change
automatically.

**Disk persistence reuses the recall fingerprint layer.** When
`MUNINN_RECALL_INDEX_DIR` is set, serialize each scope's reverse index
(target→referrers) under a fingerprint derived from the same fields as the recall
fingerprint (VFS scheme, agents dir, visibility filter). On startup, open +
reconcile (stat-diff) rather than re-scan; mismatch/corruption ⇒ rebuild. No
separate migration format. When unset, nothing is written (in-memory, rebuilt at
startup).

**Lock discipline: short critical sections.** `on_write` acquires the state lock
only to update the in-memory maps, never across disk I/O. Build at startup holds
the lock per scope, releasing between scopes so queries can proceed. This matches
recall's discipline.

## Risks / Trade-offs

- [Risk] Per-scope build scans shared notes once per scope at startup ⇒ startup
  cost grows with `scopes × (own + shared)`. → Acceptable: it replaces what
  rename/backlink queries would otherwise pay per call, and is bounded by
  `max_resident_scopes` residency (non-resident scopes build lazily on first
  backlink query, like recall). Reuse recall's INFO start/complete log lines.
- [Risk] Memory: per-scope reverse index duplicates shared referrer edges across
  scopes. → Acceptable: bounded by residency/eviction; shared edges are small
  sets. If needed later, shared referrers could be stored once and joined at
  query time — an explicit Open Question, not in scope.
- [Risk] A bug in incremental update leaves the reverse index inconsistent with
  disk. → Mitigated by the periodic stat-diff reconcile backstop (same as
  recall), which routes added/removed files through the same basename-keyed
  shift recompute, and by the eager rebuild on fingerprint mismatch; backlink
  *correctness* is independently specified by `wikilink-references`, so tests
  assert the result set equals the scan-based computation.
- [Risk] Watcher/reconcile lag means a backlink query can briefly miss a very
  recent external edit. → Acceptable: same consistency window recall already
  offers; documented.

## Migration Plan

None required beyond recall's existing fingerprint mechanism. Internal engine +
query call sites only. No tool schema change, no new config flag (reuses
`MUNINN_RECALL_INDEX_DIR`), no data migration: a missing/invalid persisted
reverse index rebuilds from the vault.

## Open Questions

- Should shared referrer edges be stored once and joined at query time to cut
  memory? Deferrable; current design duplicates per scope for correctness and
  simplicity. Does not change specs or task breakdown.
