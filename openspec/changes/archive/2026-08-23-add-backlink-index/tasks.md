## 1. Reverse index data model

- [x] 1.1 Add `BacklinkIndex` (target_clean_path → `BTreeSet<referrer_clean_path>`) and a `ScopeBacklinks` container in `src/backlink.rs`; unit-test edge insert/remove/lookup. Verify with `cargo test backlink`.
- [x] 1.2 Add `compute_out_edges(content, rendered_scope, resolver, index) -> BTreeSet<String>` using `wikilink::rewrite_links` collector mode (as `references_to` does) and `resolve_target`. Verify it returns the identical target set to `wikilink::references_to` on the scenarios in `tests/wikilinks.rs` (`references_to_*` tests). Verify with `cargo test`.

## 2. BacklinkEngine lifecycle

- [x] 2.1 Implement `BacklinkEngine` with a state lock, `build_scope(scope, regions, storage)` (eager), `on_write(scope, region, physical)`, and `on_delete(...)`; verify build yields the same referrer sets as `collect_backlinks` on a fixture vault. Verify with `cargo test backlink`.
- [x] 2.2 Wire eager startup build across all scopes + shared (reuse `storage.list_scope_dirs` and recall's INFO start/complete log lines). Verify the first backlink query does not read every note's content (observable via a read-count seam or benchmark). Verify with `cargo test`.
- [x] 2.3 Add watcher reconcile and periodic stat-diff reconcile hooks mirroring `RecallEngine`; verify an external edit is reflected within the reconcile window using the existing watcher test harness. Verify with `cargo test`.
- [x] 2.4 Add per-scope residency/eviction under `max_resident_scopes` (reuse recall's bound and reporting); verify resident scope count stays within bound after repeated cross-scope backlink queries. Verify with `cargo test`.

## 3. Query integration

- [x] 3.1 Change `collect_backlinks` (`src/tools.rs:1035`) to look up the caller's reverse index instead of `list_visible` + `references_to`; verify identical results to the prior scan via the existing backlink tests (`tests/tools.rs:797+`, `tests/wikilinks.rs:298+`). Verify with `cargo test`.
- [x] 3.2 Change `rename_memory_note` Phase 1 referrer discovery (`src/tools.rs:1329`) to use the reverse index lookup; keep the existing per-referrer content rewrite (Phase 1 still computes each referrer's rewritten content). Verify rename still rewrites exactly the right referrers via the rename tests in `tests/tools.rs`. Verify with `cargo test`.
- [x] 3.3 Invoke `BacklinkEngine::on_write`/`on_delete` on every write path (`write_memory_note`, `write_memory_notes`, `edit_memory_note`, `append_diary_entry`, `update_note_properties`, `rename_memory_note`) so the index stays current; verify a write-then-backlink round-trip for each tool. Verify with `cargo test`.

## 4. Optional disk persistence

- [x] 4.1 When `MUNINN_RECALL_INDEX_DIR` is set, serialize/load each scope's reverse index under the recall fingerprint layer; reopen + stat-diff reconcile on startup, rebuild on mismatch/corruption. Verify a restart reopens (stat-diff only, no full re-scan) with a feature-gated test. Verify with `cargo test --features recall-tantivy`.
- [x] 4.2 When unset, assert no reverse-index files are written to disk. Verify with `cargo test`.

## 5. Whole-suite verification

- [x] 5.1 Run `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`, and `cargo test --features recall-tantivy` and confirm all pass before considering the change ready to commit (per `CLAUDE.md`).
