# Proposal: persistent-recall-index

## Why

The tantivy recall index lives entirely in RAM (`RamDirectory`), so every process restart pays a full rebuild — reading and tokenizing the whole vault — before `GET /readyz` turns green. Rebuild time grows linearly with the vault while the startup-probe budget is fixed; production already oscillates between ~40s and 3+ minutes, and a build that outruns the probe budget produces a kill-and-rebuild crash loop. Persisting the index turns restart cost from "re-index everything" into "open files and stat-diff what changed while down".

## What Changes

- Add an **opt-in** on-disk index mode for the tantivy backend: when `MUNINN_RECALL_INDEX_DIR` is set, per-region indexes open a tantivy `MmapDirectory` under that directory instead of `RamDirectory`. Unset (the default) keeps today's pure in-memory behavior byte-for-byte.
- Persist the reconcile manifest **inside the index itself**: each document additionally stores its file's `mtime` and `size`, and on open the manifest is rebuilt from stored fields — no sidecar file, and tantivy's atomic commits keep index and manifest consistent across crashes.
- Startup with a valid persisted index becomes: open the index, rebuild the manifest from stored fields, stat-walk the region, and re-index only files that changed while the server was down — then flip ready.
- Guard staleness with a fingerprint directory layer: an `INDEX_FORMAT_VERSION` constant combined with a hash of the configuration that shapes ingested content (VFS scheme, agents dir, visibility filters). A mismatch, an unreadable index, or a schema conflict wipes that directory and rebuilds from scratch — never crashes, never serves a wrong index.
- Reject at config-load time an index dir located inside the vault root (the recursive vault watcher would see every commit and force pointless reconciles).
- `simple` backend ignores the setting with a startup warning; eviction/re-residence of a persisted scope index becomes reopen + stat-diff instead of a full rebuild.

## Capabilities

### New Capabilities

_None — this extends existing recall and configuration behavior._

### Modified Capabilities

- `recall-search`: the "In-memory index lifecycle" requirement changes — indexes are in-memory by default but MAY be disk-backed when the index directory is configured; adds requirements for persisted startup reconcile, fingerprint invalidation, and corruption fallback.
- `configuration`: the "Recall configuration variables" requirement changes — a new optional `MUNINN_RECALL_INDEX_DIR` variable (with validation that it lies outside the vault root), replacing the current "no on-disk index directory" guarantee with "no disk writes unless explicitly configured".

## Impact

- `src/recall/tantivy.rs`: `TantivyIndex::new` gains a persistent-open path (`MmapDirectory` + `Index::open_or_create`), two stored fields (`mtime`, `size`), and a manifest-recovery scan.
- `src/recall/mod.rs`: `new_region_index` / `ensure_built` / `ensure_scope_resident` compute per-region directories and route through open-or-rebuild; reconcile writes the new stored fields.
- `src/config.rs`: new `index_dir: Option<PathBuf>` on `RecallConfig`, env/CLI parsing, and the inside-vault-root validation.
- No new crate dependencies (tantivy's default `mmap` feature is already compiled in under `recall-tantivy`).
- Deployment (out of repo scope): mount the index dir as an `emptyDir` volume — survives liveness-kill container restarts within the pod; a pod reschedule pays one full rebuild.
