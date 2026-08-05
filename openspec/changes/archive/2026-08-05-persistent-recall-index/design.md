# Design: persistent-recall-index

## Context

See proposal.md — Why, and the `unblock-tool-dispatch` change (which should land
first: it makes long builds survivable and adds the build-duration logging that
sizes this change's payoff). Mechanics that shape this design:

- Two things evaporate on restart today: the tantivy segments (`RamDirectory`,
  `src/recall/tantivy.rs`) **and** the reconcile manifest
  (`BTreeMap<PathBuf, FileMeta{clean_path, mtime, size}>` per region,
  `src/recall/mod.rs`). Persisting segments alone is useless — an empty manifest
  makes the stat-diff treat every file as changed and re-index everything.
- Scoped indexes ingest a *transformed* view (wikilink scope suffixes stripped
  via `read_for_index`), so persisted content is only valid under the scheme /
  agents-dir / visibility config it was built with.
- The vault watcher watches the vault root recursively; index files inside it
  would mark the engine dirty on every commit.
- tantivy 0.26's default features already include `mmap`; no Cargo change.
- The `BackendIndex` trait (upsert/remove/flush/query) needs no signature change.

## Goals / Non-Goals

**Goals:**
- Restart cost proportional to *change since shutdown*, not vault size, when the
  operator opts in.
- Unset `MUNINN_RECALL_INDEX_DIR` ⇒ byte-for-byte today's behavior.
- Never crash and never serve a stale/foreign index: any doubt ⇒ wipe & rebuild.

**Non-Goals:**
- Persistence for the `simple` backend (it has no on-disk representation; warn
  and ignore).
- Multi-process sharing of one index dir (tantivy's writer lock forbids it; the
  deployment model is one writer per dir — emptyDir per pod).
- Changing recall semantics, ranking, or the reconcile model.

## Decisions

### D1: Manifest lives inside the index, not in a sidecar file

Add two stored-only fields to the tantivy schema — `mtime` (u64 nanos or millis
since epoch) and `size` (u64) — written on every upsert. On open, rebuild the
in-memory manifest by scanning stored fields of all docs (no tokenization; cheap
relative to a rebuild). The physical path key is re-derived from the stored
clean path via the resolver (the same mapping `current_files` already uses).

- Why not a sidecar JSON: two artifacts mean crash-consistency ordering
  (commit-then-write, tmp+rename) and a desync failure mode. tantivy commits are
  atomic (`meta.json` swap), so in-index metadata is *always* exactly as fresh
  as the segments — the "crash between commit and manifest write" class of bug
  cannot exist.
- The in-RAM path simply skips manifest recovery (fresh index has no docs), so
  one code path serves both modes.

### D2: Fingerprint = format-version constant + config hash, as a directory layer

Layout: `<index_dir>/<fingerprint>/{shared,scope-<hash>}/`. The fingerprint is
`INDEX_FORMAT_VERSION` (bumped manually on schema/ingestion changes) combined
with a stable hash of scheme, agents dir, and visibility settings
(`honor_ignore_files`, `include_hidden`, `include_hidden_globs`). Rendered scope
names are hashed for directory names (scopes may contain separators/unicode).
On startup, fingerprint directories other than the current one are deleted.

- Why a directory layer instead of a marker file checked per region: mismatch
  handling collapses to "this directory isn't ours — remove it", and partial
  states (some regions migrated, some not) are impossible.
- Deliberately **not** in the hash: tantivy crate version (schema open errors
  already catch incompatibility → fallback), tuning knobs like freshness or
  eviction caps (they don't change ingested content).

### D3: Open-or-rebuild funnel with unconditional fallback

`TantivyIndex::new` grows a `Option<PathBuf>` region directory. Persistent path:
`MmapDirectory::open` + `Index::open_or_create(dir, schema)` + manifest
recovery. *Any* error anywhere in that sequence (unreadable segments, schema
mismatch, lock failure, manifest-recovery error) ⇒ log a warning, delete the
region directory, recreate empty, proceed — which lands in the normal "manifest
empty ⇒ reconcile indexes everything" flow. The error path is thus the cold
path of the normal build, not separate machinery.

### D4: Config validation at load time

`RecallConfig.index_dir: Option<PathBuf>`, from `MUNINN_RECALL_INDEX_DIR` / CLI
flag. Validation in config parsing (fail-fast, consistent with existing
misconfiguration handling): must be absolute; canonicalized path must not equal
or descend from the canonicalized vault root. Non-tantivy effective backend with
the setting present ⇒ `tracing::warn!` at engine construction (where the
simple-fallback warning already lives).

### D5: Eviction becomes drop-and-reopen for persisted indexes

`ensure_scope_resident` passes the region directory, so a re-resident scope
opens its persisted segments and stat-diffs instead of full-rebuilding. Eviction
itself commits (flush) before dropping so nothing uncommitted is lost.

## Risks / Trade-offs

- [Startup stat-walk still touches every file] → Accepted: N stats ≪ N reads +
  tokenize. On networked storage this is seconds, not minutes; the
  build-duration log from `unblock-tool-dispatch` will quantify it.
- [Stale writer lock after a crash] → tantivy's lock is advisory per-process;
  D3 treats a failed writer acquisition as corruption (wipe & rebuild). With
  emptyDir-per-pod there is no cross-process contention by construction.
- [Disk usage: segments + merge headroom] → Roughly vault-text-sized plus merge
  transients; emptyDir sizing note in deployment docs. Old fingerprints are
  pruned (D2).
- [Commit now does real I/O (fsync, merges) on the write path] → Runs under
  `spawn_blocking` after `unblock-tool-dispatch`; latency is per-write
  milliseconds, invisible to liveness.
- [mtime-nanos truncation vs `SystemTime` equality in the manifest diff] →
  Store full nanos; round-trip through the stored field must reproduce the
  `SystemTime` used by `reconcile_with`'s equality check, or every restart
  re-indexes everything. Covered by a dedicated test (tasks 4.x).

## Migration Plan

1. Land after `unblock-tool-dispatch`.
2. Ship with the variable unset — zero behavior change; verify in staging.
3. Opt in on staging: mount an `emptyDir` at e.g. `/index`, set
   `MUNINN_RECALL_INDEX_DIR=/index`, restart twice; second start must reach
   ready in seconds (watch the build-duration log).
4. Roll out to production. Rollback = unset the variable (in-memory mode needs
   nothing from disk); leftover index dirs are inert and vanish with the pod.

## Open Questions

*(Both settled during implementation.)*

- ~~Exact stored representation of `mtime`~~ → u64 nanoseconds since the epoch.
  `SystemTime` is a whole number of nanoseconds on the supported platforms, so the
  round trip reproduces the value `fs::metadata` reported exactly (asserted against
  a real file's stat in `src/recall/tantivy.rs`).
- ~~Whether manifest recovery should stream via a collector or page through stored
  docs~~ → **neither: read the columnar (`FAST`) fields.** `path`, `mtime`, and
  `size` are fast fields, and recovery iterates the three columns per segment.
  Benchmarked on the 10 000-note synthetic vault (`recall/persistent_start`):

  | note size | cold build | reopen |
  |---|---|---|
  | ~250 B | 694 ms | 252 ms |
  | ~4 KB | 792 ms | 259 ms |

  Reopen is flat as note size grows 16× while the cold build climbs — recovery is
  O(documents), not O(corpus bytes). Reading the same three values out of the
  *document store* instead measured the same at ~250 B/note but is O(bytes): the
  store keeps each body in the same compressed block, so recovery would decompress
  the whole corpus and the advantage would shrink toward zero on a vault of real
  notes. The remaining ~250 ms reopen floor is fixed cost shared with the cold
  build (one 30 MB writer heap per region, the vault walk, one stat per file), not
  recovery.
