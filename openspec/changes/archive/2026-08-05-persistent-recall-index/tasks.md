# Tasks: persistent-recall-index

## 1. Configuration surface

- [x] 1.1 Add `index_dir: Option<PathBuf>` to `RecallConfig` (`src/config.rs`), parsed from `MUNINN_RECALL_INDEX_DIR` with a matching CLI flag override
- [x] 1.2 Validate at config load: value must be absolute and (canonicalized) must not equal or lie under the vault root — fail fast with a human-readable stderr line naming both variables
- [x] 1.3 Warn and ignore the setting when the effective backend is not `tantivy` (at engine construction, next to the existing feature-fallback warning)

## 2. Fingerprint layer

- [x] 2.1 Add `INDEX_FORMAT_VERSION` constant and a stable fingerprint hash over scheme, agents dir, and visibility settings (`honor_ignore_files`, `include_hidden`, `include_hidden_globs`)
- [x] 2.2 Compute per-region directories `<index_dir>/<fingerprint>/{shared,scope-<hash>}` with hashed scope names
- [x] 2.3 On engine startup with persistence enabled, delete fingerprint directories other than the current one

## 3. Persistent tantivy backend

- [x] 3.1 Extend the tantivy schema with stored `mtime` and `size` fields (u64), written on every upsert (`src/recall/tantivy.rs`)
- [x] 3.2 Give `TantivyIndex::new` an `Option<PathBuf>` region-dir parameter: `None` ⇒ `create_in_ram` (unchanged); `Some` ⇒ `MmapDirectory::open` + `Index::open_or_create`
- [x] 3.3 Implement manifest recovery: scan stored fields on open and return `(clean_path, mtime, size)` entries for the engine to seed the region manifest (re-deriving physical paths via the resolver)
- [x] 3.4 Route every persistent-open error (directory, schema, lock, recovery) through one fallback: warn, wipe the region dir, recreate empty, continue with an empty manifest
- [x] 3.5 Thread the region dir through `new_region_index`, `ensure_built`, and `ensure_scope_resident` (`src/recall/mod.rs`); seed `RegionIndex.manifest` from recovery before the first reconcile

## 4. Tests

- [x] 4.1 mtime round-trip test: upsert, commit, recover manifest, and assert the recovered `SystemTime` compares equal to `fs::metadata` output — restart over an unchanged vault must re-index zero files (assert via upsert counting or a probe on the backend)
- [x] 4.2 Restart reconcile test: build persisted index, drop engine, mutate vault (add/edit/delete), rebuild engine over the same dir, assert recall reflects all changes and unchanged files were not re-read
- [x] 4.3 Fingerprint test: rebuild engine with a different scheme over the same index dir, assert the old fingerprint directory is removed and results are correct under the new config
- [x] 4.4 Corruption test: truncate a segment file, reopen, assert warn+rebuild yields correct results and no panic
- [x] 4.5 Config tests: rejection of index dir inside vault root; warn-and-ignore under `simple`; default (unset) writes nothing to disk (assert index_dir absent ⇒ no filesystem writes outside the vault)
- [x] 4.6 Eviction test: with persistence enabled and `max_resident_scopes=1`, alternate recalls across two scopes and assert re-residence does not re-index unchanged files

## 5. Docs & verification

- [x] 5.1 Document `MUNINN_RECALL_INDEX_DIR` in the configuration docs, including the emptyDir deployment pattern, the one-writer-per-directory constraint, and disk sizing guidance
- [x] 5.2 Benchmark on a large synthetic vault: cold build vs reopen — record numbers in the PR description
- [x] 5.3 Run `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo test` (with and without `--features recall-tantivy`)
