## Context

See `proposal.md` — Why. Two pieces of existing machinery matter here:

- `src/recall/mod.rs`'s `RecallEngine::on_write(rendered_scope, region,
  physical)`: looks up the owning `RegionIndex` under `self.state.lock()`,
  and calls `apply_path` — which upserts the document *and* calls
  `idx.backend.flush()` — all inside that one lock acquisition, once per
  call.
- `src/tools.rs`'s `rename_memory_note` Phase 2: writes the destination,
  then loops over every rewritten referrer calling `storage.write_atomic`
  followed by `self.recall_on_write(...)` — i.e. one `on_write` call, one
  `flush()`, per referrer, all before the loop moves to the next file.

`flush()` on the tantivy backend is `writer.commit()` + `reader.reload()` —
a real segment commit, not a pointer swap. The `simple` backend's `flush()`
is a no-op, so it is already fine; this design only changes cost on the
`recall-tantivy` build.

## Goals / Non-Goals

**Goals:**
- Reduce a rename's index-commit cost from O(referrers) to O(distinct
  indexes touched) — in practice 1 (own scope only) or 2 (own scope +
  shared), regardless of how many referring notes are rewritten.
- Preserve the existing "reflects immediately" guarantee for every
  single-file write path (`write_memory_note`, `write_memory_notes`,
  `edit_memory_note`, `append_diary_entry`, property updates) exactly as
  today — none of those change.
- Keep the recall engine's lock held only for short, in-memory-only
  critical sections — never across a file's disk I/O, and never across the
  whole referrer loop.

**Non-Goals:**
- Not building a persistent reverse-link index. That would let
  `rename_memory_note`'s *discovery* phase (finding which notes reference
  the source) skip its own full-vault scan too — a materially bigger change
  (new persisted state, invalidation on every write, migration for
  existing vaults) that this change deliberately defers. This change only
  addresses the write-side commit cost in Phase 2; the discovery scan in
  Phase 1 was already shown not to be the bottleneck (a live 1000+-note
  vault answered the equivalent read-only backlink scan quickly).
- Not changing `write_memory_notes` (the batch-write tool) — it already
  writes N files but was never observed to be the source of the timeout
  report, and each of its entries is logically an independent note, not
  one rename's fan-out. Left alone; revisit separately if it turns out to
  need the same treatment.
- Not making the commit itself asynchronous/background relative to the
  tool call — see Decisions.

## Decisions

**Split the engine's write API into upsert (no commit) and commit,
keep a commit-per-call wrapper for the existing single-write callers.**

```rust
impl RecallEngine {
    // Existing behavior, unchanged for every single-write caller:
    // upsert + commit the owning index, one lock acquisition.
    pub fn on_write(&self, rendered_scope: &str, region: Region, physical: &PhysicalPath) { .. }

    // New: upsert only, no commit. Used by multi-write operations.
    fn upsert_write(&self, rendered_scope: &str, region: Region, physical: &PhysicalPath) -> Touched { .. }

    // New: commit exactly the indexes touched since the last commit.
    fn commit_touched(&self, touched: &Touched) { .. }
}
```

`Touched` is a small set (own-scope index / shared index — at most two
entries) accumulated by the caller as it upserts. `rename_memory_note`'s
Phase 2 calls `upsert_write` per file (destination, then each referrer) and
`commit_touched` exactly once after the loop.

Alternative considered: keep a single `on_write`-shaped call and have the
*engine* detect batching (e.g. a "begin batch" / "end batch" pair on
`RecallEngine` with internal state). Rejected: it would make every other
call site implicitly stateful (did someone forget to end the batch on an
error path?) for the benefit of exactly one caller. An explicit
upsert/commit split keeps the batching visible at the call site that
actually needs it.

**Lock discipline: short critical sections, not one held across the
loop.** Each `upsert_write` call acquires `state.lock()` only long enough
to upsert into the in-memory writer (cheap — no I/O), then releases it;
`commit_touched` acquires it again, once, to flush. This matches today's
per-call lock discipline (already short, already re-acquired per file) —
the only thing removed is the per-file `flush()`/`reader.reload()` work
happening *inside* that per-file critical section. The lock is never held
across `storage.write_atomic`'s disk I/O, before or after this change.

**Commit runs on every exit path, including a mid-loop failure, via an
RAII guard.** Phase 2 can fail partway through (an individual
`write_atomic` erroring on referrer K of N) after some earlier referrers
were already written and upserted. Rust's `?`-based early return makes
"commit no matter which path we leave by" easy to get wrong by hand, so
the touched-set commit happens in a guard's `Drop`, not as an explicit call
duplicated on every return: the accumulated `Touched` set is committed
exactly once, whether the function returns `Ok` or propagates an `Err`,
so the recall index is never left further behind disk truth than
"whatever hasn't been upserted yet" — which is the same as today's
behavior on a mid-loop failure (each prior file's write and index update
had already both landed).

Alternative considered: defer the commit to a background task, off the
request path entirely. Rejected — it would break "recall reflects the
rename immediately" for the *caller's own next call*, which the existing
`memory-tools` spec ("Recall reflects the rename immediately") and this
change's own `recall-search` delta both require to hold by the time the
tool call returns. Batching still commits synchronously; it just does it
once instead of N times.

**Scope of the fix stays inside `RecallEngine` + `rename_memory_note`.**
No change to `BackendIndex::upsert`/`remove` (still cheap, still no
commit) or to `flush()` itself — only to how often `flush()` gets called
and from where.

## Risks / Trade-offs

- [Risk] Between the first upsert and the final commit, a concurrent
  `recall_memory_notes` call on another connection sees neither the old
  nor the fully-new state for the indexes being touched — it's whatever
  was last committed, which during a large rename could be noticeably
  stale for the whole duration of the rename (previously it grew stale-to-
  fresh incrementally, one referrer at a time). → Acceptable: no existing
  requirement promises intermediate consistency during an in-flight
  multi-write operation, and the *post-completion* guarantee (what's
  actually specified) is unchanged and now atomic instead of torn.
- [Risk] If the process crashes between an upsert and the deferred commit,
  the in-memory writer's buffered-but-uncommitted documents are lost on
  restart (tantivy semantics: uncommitted adds don't survive a crash) —
  but the *files themselves* are already durably written by
  `write_atomic`'s fsync, so a restart's eager index build / reconcile
  picks them up from disk. → No data loss, just a startup rebuild for that
  scope instead of restart-with-fast-reopen; same as today's crash-during-
  write behavior for any single write that hadn't reached the periodic
  reconcile yet.
- [Risk] `commit_touched` committing two indexes (own scope + shared) when
  a rename's referrers span both regions doubles the commit count for that
  case, but it's still O(regions touched) ≤ 2, not O(referrers). →
  Acceptable; still the intended complexity reduction.

## Migration Plan

None required. Internal engine API change plus one call site
(`rename_memory_note`); no persisted format change, no config flag, no
data migration. Ships in the next release.
