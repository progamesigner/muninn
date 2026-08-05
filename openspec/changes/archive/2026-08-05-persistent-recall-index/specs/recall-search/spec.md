# recall-search Delta

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
periodic stat-diff reconcile as a backstop for missed watcher events. Idle
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

## ADDED Requirements

### Requirement: Persisted index reuse across restarts
When the index directory is configured and holds a valid persisted index for a
region, startup SHALL NOT re-read or re-tokenize unchanged vault files for that
region. The system SHALL store each document's source-file modification time and
size inside the index, rebuild its reconcile manifest from those stored fields on
open, and then run a stat-diff of the region so that only files created, modified,
or deleted while the server was down are re-indexed or removed. Recall results
after such a startup SHALL be identical to those after a full rebuild.

#### Scenario: Restart with an unchanged vault is fast
- **WHEN** the server restarts against an unchanged vault with a valid persisted
  index
- **THEN** readiness is reached after opening the indexes and a stat-only walk of
  the vault, with no note content read or tokenized, and recall results match
  those of a freshly built index

#### Scenario: Changes made while down are reconciled on startup
- **WHEN** notes were added, edited, and deleted while the server was stopped and
  the server restarts over a persisted index
- **THEN** after readiness, recall reflects the additions and edits and no longer
  returns the deleted notes, while unchanged notes were not re-indexed

#### Scenario: Crash between commit and shutdown loses no consistency
- **WHEN** the process is killed at an arbitrary point while serving writes over a
  disk-backed index
- **THEN** on restart the index opens at its last committed state, the manifest
  rebuilt from stored fields matches that state exactly, and the startup stat-diff
  re-indexes at most the files whose changes had not yet been committed

### Requirement: Index fingerprint invalidation and corruption fallback
Persisted indexes SHALL live under a fingerprint layer derived from a
format-version constant and the configuration that shapes ingested content (at
minimum the VFS scheme, the agents dir, and the visibility filter settings). On
open, a fingerprint mismatch, an unopenable or schema-incompatible index, or any
other persistence error SHALL cause the system to discard the affected persisted
state and rebuild from the vault — logging a warning, never crashing, and never
serving results from an index built under a different fingerprint. Stale
fingerprint directories SHALL be removed so disk use does not accumulate across
format or configuration changes.

#### Scenario: Config change invalidates the persisted index
- **WHEN** the server restarts with the same index directory but a changed
  `MUNINN_VFS_SCHEME`
- **THEN** the previously persisted indexes are not reused; the system rebuilds
  from the vault under the new fingerprint and removes the stale fingerprint
  directory

#### Scenario: Corrupted index falls back to a rebuild
- **WHEN** the persisted index files are truncated or otherwise unreadable at open
- **THEN** the server logs a warning, deletes the affected region's persisted
  state, rebuilds it from the vault, and reaches readiness with correct recall
  results — the process does not exit

#### Scenario: Format bump forces a clean rebuild
- **WHEN** a new server version raises the index format-version constant and starts
  over an index persisted by the previous version
- **THEN** the old fingerprint directory is discarded and indexes are rebuilt,
  with no attempt to open the old segments
