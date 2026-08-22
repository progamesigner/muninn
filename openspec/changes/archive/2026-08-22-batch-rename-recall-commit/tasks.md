## 1. Engine API: split upsert from commit

- [x] 1.1 In `src/recall/mod.rs`, add a `Touched` type (a small set capable
      of holding "own scope `<rendered_scope>`" and/or "shared", at most two
      entries) that a caller accumulates across multiple writes.
- [x] 1.2 Add `RecallEngine::upsert_write(&self, rendered_scope, region,
      physical) -> Touched` performing the same index lookup and
      `BackendIndex::upsert`/`remove` work `apply_path` does today, but
      without calling `flush()`; returns which index the write landed in
      (or an empty `Touched` when recall isn't built yet / the region isn't
      resident, mirroring `on_write`'s existing early-return behavior).
      Verify by reading the diff that it factors out of the existing
      `apply_path`/`apply_path_with` rather than duplicating their logic.
- [x] 1.3 Add `RecallEngine::commit_touched(&self, touched: &Touched)`
      flushing exactly the index/indexes named in `touched` (own scope
      and/or shared), each at most once, reacquiring `state.lock()` only for
      this call.
- [x] 1.4 Reimplement the existing `RecallEngine::on_write` as
      `upsert_write` immediately followed by `commit_touched` on the
      resulting `Touched`, so every current single-write caller
      (`write_memory_note`, `write_memory_notes`, `edit_memory_note`,
      `append_diary_entry`, property updates) is unchanged in behavior.
      Verify with the existing recall test suite (`cargo test recall`) —
      no test should need to change for this step.

## 2. `rename_memory_note`: batch the referrer loop

- [x] 2.1 In `src/tools.rs`, `rename_memory_note` Phase 2: replace each
      `self.recall_on_write(...)` call in the destination write and the
      referrer-rewrite loop with `self.recall.upsert_write(...)`,
      accumulating the returned `Touched` sets into one running `Touched`
      for the whole call.
- [x] 2.2 Ensure the accumulated `Touched` set is committed via
      `commit_touched` on every exit path from the mutation phase —
      including an early return from a mid-loop `write_atomic`/`delete`
      failure — via an RAII guard (per design.md's "Decisions") rather than
      a call duplicated at each return point. Verify by reading the control
      flow: every `?`-early-return inside Phase 2 still results in a commit
      of whatever was upserted so far.
- [x] 2.3 Verify no behavior change for the tool's response shape: the
      existing `{ renamed, path, new_path, notes_rewritten }` fields are
      untouched by this refactor.

## 3. Test coverage

- [x] 3.1 Add a recall-backend test (tantivy-feature-gated, alongside the
      existing tests in `src/recall/tantivy.rs` or `src/recall/mod.rs`)
      asserting that renaming a note with N referring notes results in
      exactly one commit per touched index, not N — e.g. by counting
      `writer.commit()` invocations through a test seam, or by asserting on
      a lower-level `Touched`-set behavior if a direct commit count isn't
      already observable. Verify with `cargo test --features recall-tantivy`.
- [x] 3.2 Add or extend a `rename_memory_note` integration test confirming
      the existing "Recall reflects the rename immediately" scenario still
      holds post-refactor: after renaming a note with multiple referrers, an
      immediate recall in the caller's scope (and the shared region, when a
      referrer lives there) returns hits at the new path/content and none at
      the old, for every rewritten referrer. Verify with `cargo test
      rename`.
- [x] 3.3 Add a mid-rename-failure test: force a write failure partway
      through the referrer loop (e.g. a referrer whose region is not
      writable, matching an existing policy-gate test fixture) and assert
      the recall index reflects exactly the files that reached disk before
      the failure — no phantom entries, no missing entries for files that
      did land. Verify with `cargo test rename`.

## 4. Whole-suite verification

- [x] 4.1 Run `cargo fmt --check`, `cargo clippy --all-targets`, `cargo
      test`, and `cargo test --features recall-tantivy` and confirm all
      pass before considering the change ready to commit (per this repo's
      `CLAUDE.md` release/commit guidance).
