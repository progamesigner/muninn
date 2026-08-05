# recall-search Specification

## Purpose
Content-ranked recall over a caller's visible memory notes, backed by per-scope in-memory indexes with structural scope isolation.

## Requirements
### Requirement: `recall_memory_notes` tool
The system SHALL expose a `recall_memory_notes` tool that returns content-ranked
hits for a query over the caller's visible set. Each hit SHALL be an object
`{ path, score, snippets, modified_at }` where `path` is the clean virtual path,
`score` is a relevance value normalized to the range 0–1, `snippets` are matching
text fragments with surrounding context, and `modified_at` is the note's last
modification time as an RFC 3339 UTC timestamp sourced from the in-memory index
manifest. The tool SHALL accept the active scope keys plus optional `query`
(full-text), `filters` (frontmatter property predicates), `regex`,
`modified_after`, `modified_before`, `path_prefix`, `limit`, and `cursor`
arguments. At least one of `query`, `filters`, `regex`, `modified_after`, or
`modified_before` MUST be supplied. The `query` and `regex` matchers SHALL be
evaluated against each note's clean virtual path in addition to its body; a path
match SHALL contribute to the relevance score with equal weight to a body match.
When a note matches only on its path, it SHALL still be returned as a hit, with the
matching path surfaced as a snippet.

`modified_after` and `modified_before` SHALL each accept an RFC 3339 timestamp or
a bare `YYYY-MM-DD` date, the latter interpreted as start of day in the configured
`MUNINN_TIMEZONE`; any other value SHALL be rejected with `invalid_argument`.
The bounds form a half-open interval (`modified_after ≤ mtime < modified_before`)
applied identically on every backend. When at least one of `query`, `regex`, or
`filters` is present, hits remain ordered by descending normalized score and the
time bounds act as a filter. When only time bounds are supplied, hits SHALL be
drawn from the index manifest without a content scan, carry `score: 1.0` and empty
`snippets`, and be ordered by `modified_at` descending then path ascending.

#### Scenario: Full-text recall returns ranked hits
- **WHEN** the tool is invoked with `query="borrow checker"` for the active scope and
  the scope owns notes whose content matches
- **THEN** the response contains a `hits` array of `{ path, score, snippets, modified_at }`
  objects ordered by descending `score`, each `path` being a clean virtual path the
  agent can pass to `read_memory_note`

#### Scenario: Recall matches the virtual path
- **WHEN** the tool is invoked with `regex="2026-06-10"` (or `query="2026-06-10"`) and
  the scope owns a note at `Agents/diary/2026-06-10.md` whose body does not contain
  that string
- **THEN** the note is returned as a hit, with the matching path surfaced as a snippet

#### Scenario: Path and body matches are weighted equally
- **WHEN** a query matches one note in its body and another note only in its path
- **THEN** each occurrence counts the same toward the match-count score, so a single
  path match and a single body match yield comparable raw scores before normalization

#### Scenario: Empty recall is rejected
- **WHEN** the tool is invoked with none of `query`, `filters`, `regex`,
  `modified_after`, or `modified_before` supplied
- **THEN** the response is an MCP error with code `invalid_argument` and no full dump
  of the vault is returned

#### Scenario: Time-only recall returns recent notes
- **WHEN** the tool is invoked with only `modified_after="2026-06-09"` and the scope
  owns notes modified on and after that date in the configured timezone
- **THEN** the response contains exactly the notes whose mtime falls in the bound,
  ordered by `modified_at` descending then path ascending, each hit carrying
  `score: 1.0` and empty `snippets`

#### Scenario: Time bounds filter a content query
- **WHEN** the tool is invoked with `query="rust"` and `modified_before` set such that
  only some matching notes fall inside the interval
- **THEN** only matching notes with `mtime < modified_before` are returned and the
  ordering remains by descending normalized score

#### Scenario: Time bounds behave identically on both backends
- **WHEN** the same vault and the same time-bounded query are served once by the
  `simple` backend and once by the `tantivy` backend
- **THEN** both return the same set of paths (scores may differ where a content
  predicate is involved)

#### Scenario: Invalid timestamp is rejected
- **WHEN** the tool is invoked with `modified_after="last tuesday"`
- **THEN** the response is an MCP error with code `invalid_argument`

#### Scenario: Half-open interval
- **WHEN** a note's mtime equals the supplied `modified_before`
- **THEN** the note is excluded; a note whose mtime equals `modified_after` is included

#### Scenario: Pagination mirrors list_memory_notes
- **WHEN** the tool is invoked with `limit=50` and more than 50 hits match
- **THEN** the response contains at most 50 hits and a non-null `next_cursor`; passing
  that cursor back returns the next page; the final page returns `next_cursor: null`;
  `limit` defaults to 200 and is capped at 1000

#### Scenario: Snippets carry no foreign scope suffix
- **WHEN** a hit's underlying note content contains the caller's own-scope link suffix
- **THEN** the returned snippet has the caller's own suffix stripped, identical to the
  read-path transform, and never exposes another scope's suffix

### Requirement: Structural per-scope index isolation
Recall SHALL be backed by per-scope in-memory indexes plus a single shared-region
in-memory index. A recall query SHALL open only the caller's own-scope index and,
when the active policy permits reading the shared region, the shared index. A
scope's content SHALL reside only in that scope's index, so a recall can never
return a hit, path, or snippet belonging to another scope — isolation is structural,
not a query-time filter.

#### Scenario: Other scopes' notes are unreachable
- **WHEN** the tool is invoked for scope `{agent:"jarvis", user:"tony"}` and the vault
  also contains matching notes for `jarvis.sam`
- **THEN** no `jarvis.sam` hit, path, or snippet appears in the response, because the
  query never opens `jarvis.sam`'s index

#### Scenario: scoped policy omits the shared index
- **WHEN** the tool is invoked under policy `scoped`
- **THEN** the query opens only the caller's own-scope index and returns no hits from
  the shared region

#### Scenario: Ignored and hidden notes never match
- **WHEN** a note is excluded by hidden filtering or by an active
  `.gitignore`/`.obsidianignore`/`.ignore` rule
- **THEN** that note is absent from every index and can never appear as a recall hit

### Requirement: Indexed content matches the read-path view
Each per-scope index SHALL ingest note content with that scope's own link suffixes stripped, identical to the transform `read_memory_note` applies for that scope, at every ingestion path (startup warm build, watcher reconcile, periodic stat-diff reconcile, eviction rebuild, and the synchronous own-write hook). Full-text matching, regex matching, frontmatter property filters, and snippet extraction SHALL therefore evaluate against the clean agent-facing content on every backend. The shared-region index SHALL ingest content verbatim (the cross-scope leak guard guarantees shared files carry no scope suffix). Scope suffix idents SHALL NOT be matchable as content: a query or regex matching only a stored link suffix SHALL NOT produce a hit.

#### Scenario: Regex matches the clean link form
- **WHEN** an own-scope note is persisted containing `[[rust.jarvis.tony]]` for scope `{agent:"jarvis", user:"tony"}` and the tool is invoked with `regex="\[\[rust\]\]"`
- **THEN** the note is returned as a hit, with snippets showing `[[rust]]`

#### Scenario: Scope idents are not phantom content
- **WHEN** the only occurrences of the string `tony` in a scope's notes are stored link suffixes and the tool is invoked with `query="tony"`
- **THEN** no hit is returned for those notes, on the `simple` and the `tantivy` backend alike

#### Scenario: Property filters compare agent-facing values
- **WHEN** recall runs the tantivy backend, an own-scope note's persisted frontmatter contains `related: "[[rust.jarvis.tony]]"`, and the tool is invoked with filter `{ key: "related", op: "eq", value: "[[rust]]" }`
- **THEN** the note is returned as a hit

#### Scenario: Every ingestion path strips identically
- **WHEN** the same suffixed note enters the index via the startup warm build, via the synchronous own-write hook, and via a rebuild after eviction
- **THEN** the indexed and stored content is identical in all three cases, with the scope's own suffixes stripped

#### Scenario: Shared index is verbatim
- **WHEN** a shared-region note is indexed
- **THEN** its content is ingested unchanged, and queries match it exactly as stored

### Requirement: Configurable search backend with a simple fallback
The system SHALL select a recall backend via `MUNINN_RECALL_BACKEND`, accepting
`simple`, `tantivy`, and `off`, defaulting to `simple`. The `tantivy` backend SHALL
be available only when the binary is built with its optional cargo feature; when the
feature is absent, or initialization fails, or `simple` is selected, the system SHALL
use the `simple` backend. When `off` is selected the `recall_memory_notes` tool SHALL
NOT be registered. The `simple` backend SHALL support `query` (case-insensitive
substring) and `regex`, and SHALL NOT support frontmatter property `filters`.

#### Scenario: Default build uses the simple backend
- **WHEN** the server starts with `MUNINN_RECALL_BACKEND` unset and the `tantivy`
  feature not compiled in
- **THEN** the `simple` backend serves recall and full-text + regex queries succeed

#### Scenario: Property filters require tantivy
- **WHEN** a recall carries `filters` while the active backend is `simple`
- **THEN** the response is an MCP error with code `unsupported` whose message states
  that property filters require the tantivy backend

#### Scenario: tantivy selected without the feature falls back
- **WHEN** `MUNINN_RECALL_BACKEND=tantivy` but the binary was built without the
  tantivy feature
- **THEN** the server logs the fallback and serves recall with the `simple` backend

#### Scenario: Recall disabled
- **WHEN** `MUNINN_RECALL_BACKEND=off`
- **THEN** the `recall_memory_notes` tool is not present in the tool listing
### Requirement: In-memory index lifecycle
Recall indexes SHALL be held entirely in memory; the system SHALL NOT write any
index data to disk. At startup the system SHALL eagerly build every scope index and
the shared index. The system SHALL update the owning index synchronously on its own
note writes, reconcile external edits via a filesystem watcher (debounced and
ignore-filtered, routing each event to the owning index idempotently by file
metadata), and run a periodic stat-diff reconcile as a backstop for missed watcher
events. Idle per-scope indexes SHALL be evicted least-recently-accessed-first so
that after a recall completes the number of resident per-scope indexes does not
exceed the configured `max_resident_scopes` bound (a configured value of 0 is
treated as 1), and evicted indexes SHALL be rebuilt on next access. The engine
SHALL expose the current resident per-scope index count so the eviction bound is
verifiable by tests and benchmarks.

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
- **THEN** the call blocks until the index is rebuilt and then returns correct results

#### Scenario: Resident indexes stay within the eviction bound
- **WHEN** the engine is configured with `max_resident_scopes` smaller than the
  number of scopes in the vault and recalls are issued against each scope in turn
- **THEN** after every recall completes, the resident per-scope index count reported
  by the engine is at most `max_resident_scopes`

### Requirement: Eager index build is observable in logs
The system SHALL emit an INFO-level log line when the eager recall index build
starts and an INFO-level log line when it completes. The completion line SHALL
carry the effective backend, the number of scope indexes built, and the elapsed
build duration, so operators can monitor build time against their startup-probe
budget as the vault grows.

#### Scenario: Build start and completion are logged with duration
- **WHEN** the server starts with recall enabled and the eager index build runs
  over the vault
- **THEN** the log contains an INFO line marking the build start, followed by an
  INFO line marking readiness that includes the backend name, the scope count,
  and the elapsed duration of the build

#### Scenario: No build logs when recall is off
- **WHEN** the server starts with `MUNINN_RECALL_BACKEND=off`
- **THEN** no index-build start or completion lines are emitted

### Requirement: Normalized cross-index ranking
The system SHALL normalize result scores per index before merging across indexes.
When a recall opens both the caller's scope index and the shared index, each index's
result scores SHALL be normalized to the range 0–1 within that index's result set
before merging and sorting, and the normalized value SHALL be returned as the hit
`score`.

#### Scenario: Scores from two indexes are comparable
- **WHEN** matching hits come from both the caller's scope index and the shared index
- **THEN** every returned `score` is in 0–1 and the merged result is ordered by the
  normalized score regardless of the differing corpus statistics of the two indexes
