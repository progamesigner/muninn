# backlink-index Specification

## Purpose
Maintains an incrementally-updated, per-scope reverse link index so that
backlink discovery (for reads and for rename) costs time proportional to the
number of referrers, not the size of the vault, while staying consistent with
server writes and external edits.
## Requirements
### Requirement: Maintained per-scope reverse link index
The system SHALL maintain, for each rendered scope, a reverse link index mapping
each visible target note (clean virtual path, `.md` stripped) to the set of
visible notes whose links resolve to that target under that scope's visible set,
computed with the same resolution rules as the forward link transform
(own-scope-preferred tie-break, shortest unambiguous names, all supported link
forms, dangling links count toward nothing). The shared region's notes SHALL be
indexed once per scope as referrers, with their links resolved against that
scope's visible set.

#### Scenario: Reverse index mirrors forward resolution
- **WHEN** scope `jarvis.tony` owns `Agents/notes/memo.md` linking `[[rust]]`
  that forward-resolves to `Agents/topics/rust.md`
- **THEN** the reverse index for scope `jarvis.tony` maps
  `Agents/topics/rust` to a set containing `Agents/notes/memo`

#### Scenario: Shared referrer is resolved under the caller's scope
- **WHEN** the shared note `Actions/release.md` links `[[rust]]`, scope
  `jarvis.tony` owns `Agents/topics/rust.md`, and scope `jarvis.sam` does not
- **THEN** under scope `jarvis.tony` the reverse index maps
  `Agents/topics/rust` to a set containing `Actions/release`, while under scope
  `jarvis.sam` (no own-scope `rust`) it maps the shared `Lang/rust` instead

#### Scenario: Dangling links produce no entry
- **WHEN** a visible note links `[[ghost]]` and no visible note resolves that target
- **THEN** the reverse index contains no entry pointing at any `ghost` target
  contributed by that note

### Requirement: Backlink discovery is index-backed, not a full scan
`read_memory_note(backlinks:true)` and `rename_memory_note` Phase 1 discovery
SHALL resolve the referrer set by looking up the target in the caller's reverse
index, not by scanning every visible note's content. The work SHALL be
proportional to the number of referrers returned, independent of vault size.

#### Scenario: collect_backlinks scales with referrers, not vault
- **WHEN** backlinks are requested for a heavily-linked note in a 1000-note vault
- **THEN** the response returns all referrers without reading every note's
  content, and the discovery cost grows with the result set, not the vault

#### Scenario: Rename discovery scales with referrers
- **WHEN** a note with K referrers is renamed in a large vault
- **THEN** Phase 1 discovers exactly those K referrers via an index lookup,
  never by scanning all visible notes

### Requirement: Reverse index reflects server writes immediately
On every server-side note write or delete (`write_memory_note`,
`write_memory_notes`, `edit_memory_note`, `append_diary_entry`,
`update_note_properties`, `rename_memory_note`), the system SHALL update the
affected scope's (and the shared region's) reverse index so that a subsequent
backlink query in that scope reflects the new or removed out-links before the
tool call returns.

#### Scenario: A new link appears in backlinks after a write
- **WHEN** a note is written containing a link to target T and then the
  backlinks of T are read in the same scope
- **THEN** the newly written note is present in the result

#### Scenario: A removed link disappears from backlinks
- **WHEN** a referrer's only link to T is edited away and then the backlinks of
  T are read
- **THEN** that referrer is absent from the result

### Requirement: Reverse index reconciles external edits
The system SHALL update the reverse index from external edits via the
filesystem watcher (debounced and ignore-filtered) and the periodic stat-diff
reconcile as a backstop, so backlink queries reflect Obsidian or human edits
within the reconcile window.

#### Scenario: An external edit is picked up
- **WHEN** a human adds a link to T in a note via Obsidian while the server runs
- **THEN** after the watcher reconcile a backlink query for T includes that note

### Requirement: Eager startup build and optional persistence
At startup the system SHALL build every scope's reverse index (and the shared
region's referrer view) eagerly so the first backlink query does not scan the
vault. When `MUNINN_RECALL_INDEX_DIR` is set, the reverse index SHALL be
persisted under the same fingerprint layer as the recall index and reopened
(not re-scanned) on restart; a fingerprint mismatch or corruption SHALL discard
and rebuild from the vault. The system SHALL NOT write reverse-index data to
disk unless that directory is configured.

#### Scenario: Startup builds in memory without disk
- **WHEN** `MUNINN_RECALL_INDEX_DIR` is unset
- **THEN** the reverse index is built in memory at startup and no index data is
  written to disk

#### Scenario: Restart reopens a persisted index
- **WHEN** the directory is set and a valid persisted reverse index exists
- **THEN** startup reopens it and reconciles by stat-diff only, without
  re-scanning every note's content

#### Scenario: Fingerprint change forces a rebuild
- **WHEN** configuration changes the index fingerprint
- **THEN** the previously persisted reverse index is discarded and rebuilt from
  the vault
