## Why

`rename_memory_note` Phase 1 and `read_memory_note(backlinks:true)` discover a
note's referrers by scanning **every** visible note's content
(`storage.list_visible` + a per-file read + `wikilink::references_to`, which
re-parses every link in each referrer and resolves it against the caller's
`LinkIndex`). The cost therefore grows with the whole vault, not with the
number of links actually pointing at the note. On a large vault this discovery
scan is a latency/timeout risk independent of the Phase 2 commit cost already
addressed by `batch-rename-recall-commit`, and it is redundantly repeated on
every backlink query and rename.

## What Changes

- Introduce a maintained reverse-link index: for each rendered scope, a mapping
  from each visible target note (clean virtual path) to the set of visible notes
  whose links resolve to it, using the same resolution rules as the forward link
  transform.
- Build the reverse index eagerly at startup (per scope + shared), mirroring the
  recall engine's eager build, so the first backlink query never scans the vault.
- Maintain the index incrementally on every server-side write/delete path
  (`write_memory_note`, `write_memory_notes`, `edit_memory_note`,
  `append_diary_entry`, `update_note_properties`, `rename_memory_note`) and on
  external edits via the filesystem watcher reconcile and the periodic stat-diff
  reconcile — reusing the recall engine's update triggers.
- Serve `collect_backlinks` and `rename_memory_note` Phase 1 discovery from the
  reverse index lookup instead of a full content scan, so cost is proportional to
  the result set, not the vault.
- When `MUNINN_RECALL_INDEX_DIR` is configured, persist the reverse index under
  the same fingerprint layer so restarts reopen rather than re-scan.
- No change to the **set** of backlinks returned (same resolution rules), and no
  tool schema or response-shape changes.

## Capabilities

### New Capabilities
- `backlink-index`: an incrementally-maintained, per-scope reverse link index
  that serves backlink discovery in time proportional to the result rather than
  the vault, consistent with server writes and external edits.

### Modified Capabilities
(none — backlink *correctness* is already specified by `wikilink-references`;
this change only adds the maintained-index mechanism and its speed/consistency
guarantees. The `batch-rename-recall-commit` change separately owns Phase 2
commit batching.)

## Impact

- New `src/backlink.rs`: the reverse index structure, out-edge computation, and
  a `BacklinkEngine` mirroring `RecallEngine`'s lifecycle.
- `src/wikilink.rs`: reuse `rewrite_links`/`resolve_target` to compute a note's
  out-edges (no resolution-rule changes).
- `src/recall/...` lifecycle patterns: eager build, watcher reconcile, periodic
  stat-diff reconcile, per-scope residency/eviction, and the fingerprint
  persistence layer — reused by reference, not duplicated as a new design.
- `src/tools.rs`: `collect_backlinks` and `rename_memory_note` Phase 1 use the
  index for discovery; the per-referrer content rewrite in Phase 1 is unchanged.
- New spec `backlink-index`; no changes to existing tool schemas or responses.
