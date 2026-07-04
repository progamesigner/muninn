## MODIFIED Requirements

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
