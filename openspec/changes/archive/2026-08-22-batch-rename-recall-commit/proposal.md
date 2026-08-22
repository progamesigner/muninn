## Why

`rename_memory_note` rewrites every visible referring note whose links
resolve to the renamed note, and for each one calls `recall_on_write`,
which — on the tantivy backend — calls `writer.commit()` and
`reader.reload()` synchronously (`apply_path_with`, `src/recall/mod.rs` /
`src/recall/tantivy.rs`). `commit()` is a real index segment flush, not a
cheap in-memory update; doing it once per referring note, serially, under
the recall engine's single state lock, means a rename of a heavily-linked
note in a large vault pays N sequential disk commits in the same request.
In a 1000+-note vault this is large enough to time out the tool call, even
though the discovery scan that finds the referrers (the same
read-only walk `read_memory_note(backlinks: true)` uses) is not itself
slow.

## What Changes

- The recall engine's write-side API is split into an in-memory upsert step
  (already cheap: `writer.delete_term` + `writer.add_document`, no commit)
  and an explicit commit/reload step, so a caller that is about to perform
  several writes in one logical operation can upsert every one of them and
  commit exactly once.
- `rename_memory_note`'s referrer-rewrite loop upserts the destination note
  and every rewritten referrer into the recall index as it writes each file
  to disk, then commits the owning index(es) once after the loop, instead
  of once per file.
- Behavior at the tool-call boundary is unchanged: by the time
  `rename_memory_note` returns, recall for the caller's scope (and the
  shared index, when a referrer lives there) reflects the rename exactly as
  it does today — same tool response shape, same `notes_rewritten` count,
  same "recall reflects the rename immediately" guarantee. What changes is
  only how many commits it costs to get there, and that mid-rewrite state
  is no longer partially visible to a concurrent recall on another
  connection (it wasn't specified either way before; this fix makes it
  atomic).

## Capabilities

### New Capabilities
(none)

### Modified Capabilities
- `recall-search`: the "In-memory index lifecycle" requirement is clarified
  to allow a single tool-call operation that performs multiple note writes
  to batch them into one index commit per affected index, rather than
  requiring a commit per file, while still guaranteeing the index reflects
  every one of the operation's writes by the time the call returns.

## Impact

- `src/recall/mod.rs`, `src/recall/simple.rs`, `src/recall/tantivy.rs`: the
  `BackendIndex`/engine write API gains an explicit
  upsert-without-commit vs. commit split (the `simple` backend's `flush` is
  already a no-op, so it is unaffected either way).
- `src/tools.rs`: `rename_memory_note`'s Phase 2 write loop switches from
  per-file `recall_on_write` to per-file upsert + a single trailing commit
  per touched index.
- No tool schema or response shape changes. No changes to
  `write_memory_note`, `write_memory_notes`, `edit_memory_note`, or any
  other single-note write path — they still commit once per call, which was
  already the minimum.
