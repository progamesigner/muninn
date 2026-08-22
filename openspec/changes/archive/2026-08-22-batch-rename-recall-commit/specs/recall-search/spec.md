## MODIFIED Requirements

### Requirement: In-memory index lifecycle
Recall indexes SHALL be held entirely in memory by default; the system SHALL NOT
write any index data to disk unless an index directory is explicitly configured.
When `MUNINN_RECALL_INDEX_DIR` is set and the effective backend is `tantivy`,
each region index SHALL instead be disk-backed under that directory and survive
process restarts; the `simple` backend SHALL ignore the setting and log a startup
warning. At startup the system SHALL eagerly build (or, when a valid persisted
index exists, open and reconcile) every scope index and the shared index. The
system SHALL update the owning index synchronously on its own note writes,
reconcile external edits via a filesystem watcher (debounced and ignore-filtered,
routing each event to the owning index idempotently by file metadata), and run a
periodic stat-diff reconcile as a backstop for missed watcher events. When a
single tool call performs multiple note writes as one logical operation (for
example, `rename_memory_note` rewriting several referring notes), the system
SHALL commit each affected index at most once for the whole operation rather
than once per file, and SHALL guarantee the index reflects every one of the
operation's writes by the time the tool call returns — matching the single-write
"reflected immediately" guarantee, just amortized over the batch. Idle
per-scope indexes SHALL be evicted least-recently-accessed-first so that after a
recall completes the number of resident per-scope indexes does not exceed the
configured `max_resident_scopes` bound (a configured value of 0 is treated as 1).
Evicted indexes SHALL be rebuilt on next access; when the index is disk-backed,
re-residence SHALL reopen the persisted index and reconcile rather than re-index
the whole scope. The engine SHALL expose the current resident per-scope index
count so the eviction bound is verifiable by tests and benchmarks.

#### Scenario: Server write is reflected immediately
- **WHEN** `write_memory_note` creates or replaces a note in the caller's scope
- **THEN** a subsequent recall in that scope reflects the new content without any
  external trigger

#### Scenario: External edit is picked up
- **WHEN** a human edits a note directly in Obsidian while the server is running
- **THEN** the watcher updates the owning index and a subsequent recall reflects the
  edit; if the watcher event is missed, the periodic stat-diff reconcile corrects it

#### Scenario: Evicted scope is rebuilt on access
- **WHEN** a per-scope index has been evicted under the memory bound and a recall for
  that scope arrives
- **THEN** the call blocks until the index is resident again and then returns correct
  results; with a disk-backed index, residency is restored by reopening and
  reconciling rather than re-indexing every note

#### Scenario: Resident indexes stay within the eviction bound
- **WHEN** the engine is configured with `max_resident_scopes` smaller than the
  number of scopes in the vault and recalls are issued against each scope in turn
- **THEN** after every recall completes, the resident per-scope index count reported
  by the engine is at most `max_resident_scopes`

#### Scenario: No disk writes without explicit opt-in
- **WHEN** recall runs with `MUNINN_RECALL_INDEX_DIR` unset, under any backend
- **THEN** no index data is written to disk and behavior is identical to the
  pre-persistence in-memory lifecycle

#### Scenario: A multi-note rename commits once per affected index
- **WHEN** `rename_memory_note` rewrites 50 referring notes across the
  caller's own scope and the shared region in a single call
- **THEN** the owning scope index and the shared index are each committed at
  most once for the whole call, not once per rewritten referrer, and by the
  time the call returns a recall in either region reflects every rewritten
  referrer and the renamed note at its new path

#### Scenario: A failed mid-rename write still leaves recall consistent with disk
- **WHEN** `rename_memory_note` writes some referring notes to disk and then
  a later write in the same operation fails
- **THEN** the recall index for any index that was committed reflects exactly
  the files that were actually written to disk before the failure — no
  index entry for a file that failed to write, and no missing entry for one
  that succeeded
