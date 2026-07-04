# memory-tools Specification

## Purpose
TBD - created by archiving change build-agentmem-mcp-server. Update Purpose after archive.
## Requirements
### Requirement: `list_memory_notes` tool
The system SHALL expose a `list_memory_notes` tool that returns a paginated set of virtual paths visible to a given scope, including both inside-agents-folder files belonging to that scope and outside-agents-folder files reachable under the active policy. The tool SHALL accept an optional `glob` argument that filters the visible set to entries whose clean, vault-root-relative virtual path matches the glob pattern; `glob` is applied as an in-memory filter over visible paths and SHALL NOT read note contents. When both `path_prefix` and `glob` are supplied, an entry SHALL be returned only if it satisfies both. An invalid glob pattern SHALL be rejected with `invalid_argument`. The tool SHALL accept an optional `order` argument selecting the result ordering by clean virtual path: `name_asc` (the default) or `name_desc`. Ordering SHALL be applied before pagination so `limit`/`cursor` page over the ordered set, and SHALL remain deterministic across calls with identical arguments. An unrecognized `order` value SHALL be rejected with `invalid_argument`. The tool SHALL accept an optional `view` argument selecting what the items represent: `files` (the default) returns individual note virtual paths; `dirs` returns the distinct directory virtual paths derived from the visible set — the deduplicated set of every ancestor directory of a visible note. The `dirs` view SHALL be derived purely from the visible paths without reading note contents, SHALL honor the `path_prefix` filter and pagination, and SHALL preserve deterministic ordering. An unrecognized `view` value SHALL be rejected with `invalid_argument`.

#### Scenario: Lists own-scope and outside files under namespaced policy
- **WHEN** the tool is invoked with the active scope, policy is `namespaced`, and the vault contains scope-owned files inside the agents folder plus human-authored files outside it
- **THEN** the response contains both sets, each entry represented as the clean virtual path the agent would use in subsequent calls

#### Scenario: Optional path prefix filter
- **WHEN** the tool is invoked with `path_prefix="topics"` and the agents folder is `Agents`
- **THEN** only entries whose virtual path begins with `topics` (under the agents folder) are returned

#### Scenario: Optional glob filter over the virtual path
- **WHEN** the tool is invoked with `glob="Agents/diary/2026-*"` and the visible set contains `Agents/diary/2026-06-10.md` and `Agents/topics/rust.md`
- **THEN** only `Agents/diary/2026-06-10.md` is returned

#### Scenario: glob composes with path_prefix
- **WHEN** the tool is invoked with `path_prefix="topics"` and `glob="**/*.md"`
- **THEN** only entries that both fall under `topics` (within the agents folder) and match `**/*.md` are returned

#### Scenario: Invalid glob is rejected
- **WHEN** the tool is invoked with a `glob` argument that is not a valid glob pattern
- **THEN** the response is an MCP error with code `invalid_argument`

#### Scenario: Default ordering is ascending by path
- **WHEN** the tool is invoked with `order` unset
- **THEN** entries are returned in ascending clean-virtual-path order

#### Scenario: Descending order by path
- **WHEN** the tool is invoked with `order="name_desc"` and the visible set contains `Agents/diary/2026-01-01.md` and `Agents/diary/2026-06-10.md`
- **THEN** `Agents/diary/2026-06-10.md` is returned before `Agents/diary/2026-01-01.md`

#### Scenario: Unrecognized order value is rejected
- **WHEN** the tool is invoked with an `order` value other than `name_asc` or `name_desc`
- **THEN** the response is an MCP error with code `invalid_argument`

#### Scenario: Default view lists files
- **WHEN** the tool is invoked with `view` unset
- **THEN** the items are individual note virtual paths, as before

#### Scenario: Directory view lists distinct directories
- **WHEN** the tool is invoked with `view="dirs"` and the visible set contains `Agents/diary/2026-06-10.md`, `Agents/topics/rust.md`, and `Agents/topics/python.md`
- **THEN** the items are the distinct directory paths `Agents`, `Agents/diary`, and `Agents/topics` (no individual file paths), deduplicated and deterministically ordered

#### Scenario: Unrecognized view value is rejected
- **WHEN** the tool is invoked with a `view` value other than `files` or `dirs`
- **THEN** the response is an MCP error with code `invalid_argument`

#### Scenario: Other scopes' files are hidden
- **WHEN** the tool is invoked with scope `{agent:"jarvis", user:"tony"}` and the vault also contains files for `jarvis.sam`
- **THEN** the `jarvis.sam` files do NOT appear in the response

#### Scenario: scoped policy hides everything outside agents folder
- **WHEN** the tool is invoked under policy `scoped`
- **THEN** the response contains only the caller's own-scope files inside the agents folder and no entries from outside it

#### Scenario: Pagination via limit and cursor
- **WHEN** the tool is invoked with `limit=50` and the visible set contains more than 50 entries
- **THEN** the response contains exactly 50 entries and a non-null `next_cursor` opaque string; passing that `next_cursor` back in a follow-up call returns the next page; the final page's response has `next_cursor: null`

#### Scenario: Default page size
- **WHEN** the tool is invoked with `limit` unset
- **THEN** the server applies a default page size of 200 and caps `limit` at 1000; values above 1000 are rejected with `invalid_argument`

#### Scenario: Stable ordering across pages
- **WHEN** the tool is called twice in a row with the same arguments and no concurrent writes occur between the calls
- **THEN** the entries appear in the same deterministic order in both responses

### Requirement: `read_memory_note` tool
The system SHALL expose a `read_memory_note` tool that returns the UTF-8 contents of a single file identified by its virtual path, subject to the active policy, region detection, and visibility filters. The tool SHALL accept an optional boolean `backlinks` argument; when `true`, the structured result SHALL additionally carry a `backlinks` array containing the clean virtual path of every visible note that has at least one link resolving to the target note. The array SHALL be deduplicated (one entry per referring note regardless of how many of its links resolve to the target), sorted ascending by clean virtual path, and computed over exactly the caller's visible set (own scope plus the shared region when the active policy permits reading it). When `backlinks` is absent or `false`, the response SHALL NOT contain a `backlinks` field and SHALL be unchanged from prior behavior.

The tool SHALL accept optional `offset` and `limit` integer arguments selecting a line range of the note: `offset` is the 1-based index of the first returned line (default 1) and `limit` is the maximum number of lines returned (default: all remaining lines). The range SHALL be applied to the agent-facing content after the own-suffix link strip, with lines delimited by `\n` and delimiters preserved, so that concatenating consecutive slices reproduces the full content byte-for-byte. When at least one of `offset`/`limit` is supplied, the structured result SHALL additionally carry `total_lines`, the line count of the full agent-facing content. An `offset` past the last line SHALL return empty content (not an error) with a truthful `total_lines`. An `offset` or `limit` of `0` SHALL be rejected with `invalid_argument`. When neither `offset` nor `limit` is supplied, the response SHALL NOT contain a `total_lines` field and SHALL be unchanged from prior behavior. The `backlinks` and range arguments compose.

#### Scenario: Read of an own-scope file inside the agents folder
- **WHEN** the tool is called with virtual path `PERSONA.md` (resolved under the agents folder) for the active scope and that file exists
- **THEN** the response contains the file's contents as a string

#### Scenario: Read outside the agents folder under namespaced policy
- **WHEN** policy is `namespaced` and the tool is called with virtual path `Actions/release.md` and that file exists
- **THEN** the response contains the file's contents as a string

#### Scenario: Read outside the agents folder under scoped policy
- **WHEN** policy is `scoped` and the tool is called with virtual path `Actions/release.md`
- **THEN** the response is an MCP error with code `path_not_permitted`

#### Scenario: Read of a missing file
- **WHEN** the tool is called with a virtual path that resolves to a non-existent file
- **THEN** the response is an MCP error with code `not_found`

#### Scenario: Read of a hidden or ignored file
- **WHEN** the tool is called with a virtual path that is excluded by hidden filtering or by an active `.gitignore`/`.obsidianignore` rule
- **THEN** the response is an MCP error with code `path_not_permitted` and the message does NOT reveal whether the file actually exists

#### Scenario: Line range returns the requested slice with total_lines
- **WHEN** a note has 50 lines and the tool is called with `offset=11` and `limit=10`
- **THEN** the returned content is exactly lines 11 through 20 (line delimiters preserved) and the structured result carries `total_lines: 50`

#### Scenario: Offset alone reads to the end
- **WHEN** a note has 50 lines and the tool is called with `offset=41` and no `limit`
- **THEN** the returned content is lines 41 through 50 and `total_lines` is 50

#### Scenario: Limit alone reads from the start
- **WHEN** the tool is called with `limit=5` and no `offset`
- **THEN** the returned content is the first 5 lines and the structured result carries `total_lines`

#### Scenario: Offset past the end is empty, not an error
- **WHEN** a note has 10 lines and the tool is called with `offset=11`
- **THEN** the call succeeds with empty content and `total_lines: 10`

#### Scenario: Zero offset or limit is rejected
- **WHEN** the tool is called with `offset=0` or `limit=0`
- **THEN** the response is an MCP error with code `invalid_argument`

#### Scenario: Range slices the link-stripped view
- **WHEN** the persisted note contains `[[rust.jarvis.tony]]` on line 3 for scope `{agent:"jarvis", user:"tony"}` and the tool is called with `offset=3`, `limit=1`
- **THEN** the returned line contains `[[rust]]` — the slice is taken after the suffix strip, and line numbers match a whole-note read

#### Scenario: Default response is unchanged
- **WHEN** the tool is called without `offset` or `limit`
- **THEN** the full content is returned and the structured result contains no `total_lines` field, byte-identical to prior behavior

#### Scenario: Backlinks returned on request
- **WHEN** the tool is called with `backlinks=true` for note `Agents/topics/rust.md` and the caller's visible notes `Agents/diary/2026-06-10.md` (containing `[[rust]]`) and `Agents/MEMORY.md` (containing `[[topics/rust|the Rust note]]`) both resolve those links to the target
- **THEN** the structured result carries `backlinks: ["Agents/MEMORY.md", "Agents/diary/2026-06-10.md"]` alongside the content

#### Scenario: All link forms count as backlinks
- **WHEN** a visible note references the target via an embed `![[target]]`, a heading link `[[target#section]]`, an aliased link `[[target|label]]`, or a relative markdown link `[text](path/target.md)`
- **THEN** that note appears in the target's `backlinks` array

#### Scenario: A referring note appears once
- **WHEN** a single visible note contains three distinct links that all resolve to the target
- **THEN** the `backlinks` array contains that note's clean virtual path exactly once

#### Scenario: Backlinks honor forward-resolution tie-breaks
- **WHEN** the caller's visible set contains both `Agents/topics/rust.md` and `Lang/rust.md`, and a visible note contains `[[rust]]` which forward resolution resolves to `Agents/topics/rust.md`
- **THEN** that note appears in the backlinks of `Agents/topics/rust.md` and NOT in the backlinks of `Lang/rust.md`

#### Scenario: Other scopes' links are structurally invisible
- **WHEN** the tool is called with `backlinks=true` for shared note `Actions/release.md` by scope `{agent:"jarvis", user:"tony"}`, and a note belonging to scope `jarvis.sam` links to that shared note
- **THEN** no `jarvis.sam` path appears in the `backlinks` array, because another scope's notes are never scanned

#### Scenario: scoped policy excludes the shared region from the scan
- **WHEN** policy is `scoped` and the tool is called with `backlinks=true` for an own-scope note that a shared-region note links to
- **THEN** the `backlinks` array does not contain the shared note's path

#### Scenario: Backlinks omitted by default
- **WHEN** the tool is called without the `backlinks` argument
- **THEN** the structured result contains no `backlinks` field and is unchanged from prior behavior

### Requirement: `read_memory_notes` tool
The system SHALL expose a `read_memory_notes` tool that reads multiple notes in one call. The `paths` argument SHALL be an array of 1 to 20 entries, each entry being either a **vault-root-relative** virtual path string (the whole note) or an object `{ path, offset?, limit? }` requesting a line range of that note with the same semantics as `read_memory_note` (`offset` is the 1-based first line, `limit` the maximum line count, applied to the agent-facing content after the own-suffix link strip). An empty array, an array exceeding 20 entries, an entry that is neither a string nor such an object, or an `offset`/`limit` of `0` SHALL be rejected with `invalid_argument`. The result SHALL contain a `notes` array with exactly one entry per requested path, in request order, each entry being either `{ path, content }` on success or `{ path, error: { code, message } }` on failure, where `path` echoes the requested path verbatim; a successful entry for which a range was requested SHALL additionally carry `total_lines`, the line count of that note's full agent-facing content. Per-path semantics — policy gating, region detection, visibility filtering, suffix resolution, and own-suffix link stripping — SHALL be identical to `read_memory_note`, and a per-path failure SHALL use the same error code the single-read tool would return. A per-path failure SHALL NOT fail the call; the tool-level result is an error only for malformed arguments or invalid scope keys. Duplicate paths SHALL each produce their own entry.

#### Scenario: Batch read returns contents in request order
- **WHEN** the tool is called with `paths=["Agents/topics/rust.md", "Actions/release.md"]` under `namespaced` policy and both notes exist
- **THEN** the result's `notes` array has two entries in that order, each carrying the note's content with the caller's own link suffixes stripped

#### Scenario: Mixed string and ranged entries
- **WHEN** the tool is called with `paths=["Agents/topics/rust.md", { "path": "Agents/diary/2026-06-10.md", "offset": 1, "limit": 5 }]`
- **THEN** the first entry carries the whole note with no `total_lines` field, and the second carries only the first 5 lines plus `total_lines` for the diary note

#### Scenario: Ranged entry past the end is empty, not an error
- **WHEN** an object entry requests `offset` beyond the note's last line
- **THEN** that entry succeeds with empty content and a truthful `total_lines`, and other entries are unaffected

#### Scenario: Partial failure does not void the batch
- **WHEN** the tool is called with three paths of which the second resolves to a non-existent file
- **THEN** entries one and three carry content, entry two carries `error: { code: "not_found", … }`, and the tool call itself succeeds

#### Scenario: Per-path policy and visibility parity
- **WHEN** a requested path would be denied by the single read (e.g. outside the agents folder under `scoped` policy, or a hidden/ignored target)
- **THEN** that entry carries the same error code the single read would return (`path_not_permitted`), without revealing whether the file exists

#### Scenario: Batch size limits
- **WHEN** the tool is called with an empty `paths` array or with more than 20 entries
- **THEN** the response is an MCP error with code `invalid_argument` and no notes are read

#### Scenario: Malformed entry is rejected at the call level
- **WHEN** the tool is called with an entry that is neither a string nor an object with a string `path` (e.g. a number, or `{ "offset": 3 }` with no path), or an object entry with `offset=0` or `limit=0`
- **THEN** the response is an MCP error with code `invalid_argument` and no notes are read

#### Scenario: Duplicate paths are answered positionally
- **WHEN** the tool is called with the same path twice
- **THEN** the `notes` array contains two entries for it, one per request position

### Requirement: `write_memory_note` tool
The system SHALL expose a `write_memory_note` tool that performs an atomic full-file write to a virtual path the active policy permits writing to. The `path` argument is a **vault-root-relative** virtual path; to target a location inside the agents folder the caller MUST include the agents-folder name as the leading segment (the dedicated wrapper tools do this automatically; the generic tools do not). Inside the agents folder, the target virtual path MUST be under a subfolder; a root-level path (a path with no subfolder segment beneath the per-scope root) is reserved for the dedicated wrapper tools (`evolve_core_persona`, `update_task_heartbeat`) and SHALL be rejected.

The tool SHALL accept an optional boolean `append` argument. When `append` is `true`, `content` SHALL be appended verbatim to the existing note — exact bytes, no implicit separator — under the same per-target lock as the diary append, so concurrent appends to one note serialise without loss; when the note does not exist it SHALL be created with `content` as its full body. The appended fragment SHALL pass through the write-side link transform (including the cross-scope leak guard) exactly like full-write content. All other guards (root-level reservation, policy gates, visibility filters) apply unchanged. The returned byte count SHALL be the note's total size after the write in both modes.

#### Scenario: Write succeeds inside agents folder
- **WHEN** policy permits writes inside the agents folder (any policy other than `readonly`) and the tool is called with a vault-root-relative virtual path inside a subfolder of it (e.g. `Agents/topics/auth/jwt.md`)
- **THEN** the file is created or replaced via the atomic-write procedure and the response is a success result containing the byte count written

#### Scenario: Write to a root core file is rejected
- **WHEN** the tool is called with an agents-folder root-level virtual path (e.g. `Agents/MEMORY.md`, `Agents/USER.md`, or `Agents/PERSONA.md`)
- **THEN** the response is an MCP error with code `path_not_permitted`, the file on disk is unchanged, and the message names the wrapper to use (`evolve_core_persona` for foundational files, `update_task_heartbeat` for the heartbeat)

#### Scenario: Write refused outside agents folder under namespaced policy
- **WHEN** policy is `namespaced` and the tool is called with virtual path `Actions/release.md`
- **THEN** the response is an MCP error with code `write_denied` and the file on disk is unchanged

#### Scenario: Write succeeds outside agents folder under readwrite policy
- **WHEN** policy is `readwrite` and the tool is called with virtual path `Scratch/team-notes.md`
- **THEN** the file is created or replaced at `<root>/Scratch/team-notes.md` without a suffix and the response is a success result containing the byte count written

#### Scenario: Write refused under readonly policy
- **WHEN** policy is `readonly` and any write tool is invoked
- **THEN** the response is an MCP error with code `write_denied`

#### Scenario: Write refused on hidden or ignored target
- **WHEN** the tool is called against a virtual path excluded by visibility filters
- **THEN** the response is an MCP error with code `path_not_permitted` and no file is created

#### Scenario: Append extends an existing note verbatim
- **WHEN** the tool is called with `append=true` and `content="- new fact\n"` against a note ending in `"- old fact\n"`
- **THEN** the note ends with `"- old fact\n- new fact\n"` — no separator inserted — and the response reports the note's total byte count

#### Scenario: Append to a missing note creates it
- **WHEN** the tool is called with `append=true` against a virtual path with no existing file
- **THEN** the note is created with `content` as its full body

#### Scenario: Concurrent appends are not lost
- **WHEN** multiple callers append to the same note concurrently
- **THEN** every appended fragment appears exactly once in the final note (appends serialise under the per-target lock)

#### Scenario: Appended links are transformed
- **WHEN** `append=true` content contains `[[rust]]` resolving to an own-scope note
- **THEN** the persisted fragment carries the expanded suffixed form and a subsequent read returns the clean form, identical to full-write behavior

#### Scenario: Append honors the same guards as full write
- **WHEN** the tool is called with `append=true` against a root-level core file, a policy-denied region, or a visibility-excluded path
- **THEN** the response is the same error the full-write mode would produce and nothing is written

### Requirement: `edit_memory_note` tool
The system SHALL expose an `edit_memory_note` tool that takes a virtual path, a `search_string`, and a `replace_string`; replaces the unique occurrence of the search string with the replacement; and persists the result atomically. The `path` argument is a **vault-root-relative** virtual path; to target a location inside the agents folder the caller MUST include the agents-folder name as the leading segment. The search string MUST appear exactly once in the target file. Inside the agents folder, the target virtual path MUST be under a subfolder; a root-level path is reserved for the dedicated wrapper tools and SHALL be rejected.

#### Scenario: Successful edit
- **WHEN** the tool is called and the search string appears exactly once in the target file
- **THEN** the server writes the modified file atomically and returns a success result indicating the number of characters replaced

#### Scenario: Edit of a root core file is rejected
- **WHEN** the tool is called with an agents-folder root-level virtual path (e.g. `Agents/MEMORY.md`)
- **THEN** the response is an MCP error with code `path_not_permitted`, the file is unchanged, and the message names the wrapper to use

#### Scenario: Edit refused on read-only target
- **WHEN** the active policy denies writes to the target's region (e.g. `namespaced` on a path outside the agents folder, or `readonly` anywhere)
- **THEN** the response is an MCP error with code `write_denied` and the file is unchanged

#### Scenario: Edit refused on missing search string
- **WHEN** the search string does not appear in the target file
- **THEN** the response is an MCP error with code `edit_search_not_found` and the file is unchanged

#### Scenario: Edit refused on ambiguous search string
- **WHEN** the search string appears two or more times in the target file
- **THEN** the response is an MCP error with code `edit_search_ambiguous`, the file is unchanged, and the message tells the agent to retry with a longer, more specific snippet

### Requirement: `delete_memory_note` tool
The system SHALL expose a `delete_memory_note` tool that removes a single file at the given virtual path, subject to the active policy and own-scope rules. The `path` argument is a **vault-root-relative** virtual path; to target a location inside the agents folder the caller MUST include the agents-folder name as the leading segment. The tool SHALL NOT remove directories, and SHALL leave a parent directory in place even if it becomes empty. Inside the agents folder, the target virtual path MUST be under a subfolder; a root-level core file SHALL NOT be deletable through this tool.

#### Scenario: Delete succeeds for own-scope file under writable policy
- **WHEN** policy permits writes in the target's region and the tool is called for an own-scope file under a subfolder that exists
- **THEN** the file is removed via `std::fs::remove_file` and the response is a success result

#### Scenario: Delete of a root core file is rejected
- **WHEN** the tool is called with an agents-folder root-level virtual path (e.g. `Agents/PERSONA.md`)
- **THEN** the response is an MCP error with code `path_not_permitted` and the file is unchanged

#### Scenario: Delete refused under readonly policy
- **WHEN** policy is `readonly`
- **THEN** the response is an MCP error with code `write_denied` and the file is unchanged

#### Scenario: Delete refused outside agents folder under namespaced policy
- **WHEN** policy is `namespaced` and the tool is called with a virtual path outside the agents folder
- **THEN** the response is an MCP error with code `write_denied`

#### Scenario: Delete refused outside agents folder under scoped policy
- **WHEN** policy is `scoped` and the tool is called with a virtual path outside the agents folder
- **THEN** the response is an MCP error with code `path_not_permitted`

#### Scenario: Delete of a missing file
- **WHEN** the tool is called for a path that resolves to a non-existent file
- **THEN** the response is an MCP error with code `not_found`

#### Scenario: Delete of another scope's file is unreachable
- **WHEN** the tool is called inside the agents folder for a virtual path whose own-scope resolution does not exist, even though another scope's file with a different suffix does exist at the same logical name
- **THEN** the response is `not_found` and the other scope's file is NOT removed

### Requirement: `rename_memory_note` tool
The system SHALL expose a `rename_memory_note` tool that moves a single note from `path` to `new_path` (both **vault-root-relative** virtual paths) and rewrites every visible incoming link to resolve to the new location. The destination MUST NOT already exist; a rename onto an existing note SHALL be rejected with a `destination_exists` error and no change on disk. Inside the agents folder, both `path` and `new_path` MUST be under a subfolder; an agents-folder root-level path on either end SHALL be rejected with `path_not_permitted` (core files are wrapper-managed). The active policy MUST permit writing both the source's and the destination's region, and the region of every referring note that requires rewriting; when any of these is not writable the tool SHALL refuse with the appropriate policy error before any mutation. The moved note's own content SHALL be re-run through the write-side link transform for the destination's region, including the cross-scope leak guard. All preconditions SHALL be validated before the first write; mutations are then applied in the order: write destination, rewrite referrers, delete source.

#### Scenario: Rename moves content and reports rewrites
- **WHEN** the tool is called with `path="Agents/topics/rust.md"`, `new_path="Agents/topics/rust-lang.md"` under a writable policy
- **THEN** the destination contains the source's content, the source no longer exists, and the response carries `{ renamed: true, path, new_path, notes_rewritten }`

#### Scenario: Incoming wikilinks are rewritten
- **WHEN** a visible note contains `[[rust]]`, `[[rust#install|the note]]`, and `![[rust]]` all forward-resolving to the source, and the source is renamed to `rust-lang.md`
- **THEN** after the rename the referring note's links resolve to the destination, with heading, alias, and embed decorations preserved (e.g. `[[rust-lang#install|the note]]`)

#### Scenario: Incoming markdown links are rewritten
- **WHEN** a visible note contains `[doc](topics/rust.md)` resolving to the source and the source is renamed within the same scope
- **THEN** the referring note's markdown link target is rewritten to the destination's persisted form and round-trips to the clean new path on read

#### Scenario: Self-references move with the note
- **WHEN** the source note's own content contains a link resolving to itself
- **THEN** the destination's content links to the destination (the old name neither dangles nor persists)

#### Scenario: Destination must not exist
- **WHEN** the tool is called with a `new_path` at which a visible note already exists
- **THEN** the response is an MCP error with code `destination_exists` and neither note is modified

#### Scenario: Root core files are not renamable
- **WHEN** the tool is called with `path` or `new_path` at the agents-folder root level (e.g. `Agents/MEMORY.md`)
- **THEN** the response is an MCP error with code `path_not_permitted` and nothing changes

#### Scenario: Policy gates both regions
- **WHEN** policy is `namespaced` and either `path` or `new_path` resolves outside the agents folder
- **THEN** the response is an MCP error with code `write_denied` and nothing changes

#### Scenario: Shared-to-scoped rename is refused when shared referrers exist
- **WHEN** policy is `readwrite`, a shared-region note links to shared note `Actions/release.md`, and the tool is asked to rename `Actions/release.md` to a path inside the agents folder
- **THEN** the response is an MCP error with code `write_denied` (the rewrite would persist the caller's scope suffix in a shared note) and nothing changes

#### Scenario: Leak guard applies to the moved content
- **WHEN** policy is `readwrite` and a scoped note whose content links to another of the caller's scoped notes is renamed to a destination outside the agents folder
- **THEN** the response is an MCP error with code `write_denied` and nothing changes

#### Scenario: Missing source
- **WHEN** the tool is called with a `path` that resolves to a non-existent file
- **THEN** the response is an MCP error with code `not_found`

#### Scenario: Recall reflects the rename immediately
- **WHEN** recall is enabled and a note is renamed
- **THEN** a subsequent recall in the same scope returns hits at the new path and none at the old path, without waiting for the filesystem watcher

### Requirement: `read_note_properties` tool
The system SHALL expose a `read_note_properties` tool, available on every build, that returns the frontmatter properties of the note at the given **vault-root-relative** virtual path as a JSON object in `{ properties }`. Parsing SHALL match the recall indexer's frontmatter interpretation: a leading `---` fenced YAML block is parsed to a JSON object; absent, unterminated, or malformed frontmatter yields an empty object and is never an error. The caller's own scope suffix SHALL be stripped from link targets in every string value of the returned properties, recursing into arrays and nested objects, applying the same read-path transform as `read_memory_note`; the agent SHALL never observe its own scope suffix in a returned property value. Read gating SHALL be identical to `read_memory_note` (policy, region, visibility filters), and root-level core files SHALL be readable.

#### Scenario: Properties returned as JSON
- **WHEN** the tool is called for a note beginning `---\ntags: [rust, async]\nstatus: draft\n---\n…`
- **THEN** the result is `{ properties: { "tags": ["rust", "async"], "status": "draft" } }`

#### Scenario: Suffixed link values are returned clean
- **WHEN** scope `{agent:"jarvis", user:"tony"}` calls the tool for a note whose persisted frontmatter contains `related: "[[rust.jarvis.tony]]"`
- **THEN** the result contains `related: "[[rust]]"`

#### Scenario: Nested string values are stripped
- **WHEN** the persisted frontmatter contains `links: ["[[a.jarvis.tony]]", { "see": "[[b.jarvis.tony]]" }]` for the caller's own scope
- **THEN** the returned values are `["[[a]]", { "see": "[[b]]" }]`

#### Scenario: No frontmatter yields an empty object
- **WHEN** the tool is called for a note with no leading `---` block (or a malformed one)
- **THEN** the result is `{ properties: {} }`

#### Scenario: Read gating parity
- **WHEN** the tool is called for a missing, hidden/ignored, or policy-denied path
- **THEN** the response is the same MCP error code `read_memory_note` would return

### Requirement: `update_note_properties` tool
The system SHALL expose an `update_note_properties` tool, available on every build, that merges a JSON object `properties` into the frontmatter of the note at the given **vault-root-relative** virtual path and persists atomically under the per-target lock. Each supplied key SHALL be upserted with its JSON value (strings, numbers, booleans, arrays, and objects round-trip); a key supplied with an explicit `null` SHALL be deleted. The write-side link transform SHALL be applied to every string value of the supplied properties, recursing into arrays and nested objects: link targets resolving into the caller's own scope are expanded to the suffixed physical form, shared targets are left clean, dangling targets are left verbatim, and a supplied property whose link target resolves into the caller's own scope while the note lives in the shared region SHALL be refused with the cross-scope leak-guard error, leaving the file unchanged. Non-string values SHALL NOT be transformed. The note body SHALL remain byte-identical. A note without a frontmatter block SHALL gain one; a merge whose result is an empty object SHALL remove the block entirely. When the existing leading block looks like frontmatter but is not valid YAML, the tool SHALL refuse with `invalid_argument` and leave the file unchanged. The frontmatter block is re-serialized in normalized form (stable key order; comments and YAML formatting are not preserved). Write gating SHALL match the generic write tools: agents-folder root-level paths are rejected as wrapper-reserved, policy and visibility guards apply, the target must exist (`not_found` otherwise), and the recall index SHALL be updated synchronously. The result SHALL return the full post-update `{ properties }` in the agent-facing clean form (own suffixes stripped).

#### Scenario: Upsert and delete in one call
- **WHEN** a note's frontmatter is `{ status: "draft", priority: 2 }` and the tool is called with `properties={ "status": "done", "reviewed": true, "priority": null }`
- **THEN** the persisted frontmatter parses as `{ status: "done", reviewed: true }`, the body is byte-identical, and the result echoes the merged set

#### Scenario: Own-scope link value is expanded on disk and returned clean
- **WHEN** scope renders to `jarvis.tony` and the tool sets `related: "[[rust]]"` where `rust` resolves to the caller's own `topics/rust.md`
- **THEN** the persisted frontmatter value is `"[[rust.jarvis.tony]]"`, the result echoes `related: "[[rust]]"`, and a subsequent `read_note_properties` returns `"[[rust]]"`

#### Scenario: Shared link value stays clean
- **WHEN** the tool sets `related: "[[release]]"` where `release` resolves to the shared `Actions/release.md`
- **THEN** the persisted value is `"[[release]]"` with no suffix

#### Scenario: Leak guard applies to property values
- **WHEN** policy permits writing the shared note `Actions/release.md` and the tool sets a property containing `[[rust]]` that resolves only into the caller's own scope
- **THEN** the call is refused with the `write_denied`-class cross-scope error naming the target and `Actions/release.md` is unchanged

#### Scenario: Dangling and non-string values are untouched
- **WHEN** the tool sets `related: "[[not-yet-created]]"` (resolving to nothing) and `priority: 2`
- **THEN** both are persisted verbatim

#### Scenario: Block created when absent
- **WHEN** the tool is called against a note with no frontmatter
- **THEN** a `---` fenced block containing the supplied properties is added above the unchanged body

#### Scenario: Emptied block is removed
- **WHEN** the merge deletes every remaining key
- **THEN** the persisted note has no frontmatter fences and the body is unchanged

#### Scenario: Malformed existing frontmatter is refused
- **WHEN** the note begins with a `---` fence whose contents do not parse as YAML
- **THEN** the response is an MCP error with code `invalid_argument` and the file is unchanged

#### Scenario: Updated properties are immediately recallable
- **WHEN** recall runs the tantivy backend and the tool sets `status: "done"` on a note
- **THEN** a subsequent `recall_memory_notes` call with filter `{ key: "status", op: "eq", value: "done" }` returns the note without waiting for the watcher

#### Scenario: Write gating parity
- **WHEN** the tool targets an agents-folder root-level core file, a policy-denied region, a visibility-excluded path, or a missing file
- **THEN** the response carries the same error code the generic write tools would return (`path_not_permitted` naming the wrapper, the policy error, or `not_found`)

### Requirement: `load_session_context` tool
The system SHALL expose a `load_session_context` tool that, in a single call for the active scope, returns the **rendered session-context** produced by the shared session-context renderer (see the *Session-context renderer* requirement). The tool SHALL accept only scope parameters. The response SHALL contain a `rendered` field holding the rendered markdown string and a `missing` list naming any of the five foundational files (`PERSONA.md`, `PROMPT.md`, `RULES.md`, `USER.md`, `MEMORY.md`) that did not exist for the scope at render time. The tool SHALL succeed even when no foundational files and no session-context template exist.

#### Scenario: Rendered context returned
- **WHEN** the tool is called for an active scope
- **THEN** the response contains a `rendered` markdown string produced by the renderer and a `missing` list naming any absent foundational file

#### Scenario: Some files missing
- **WHEN** only `PERSONA.md` and `RULES.md` exist for the scope
- **THEN** the `rendered` output substitutes the persona and rules contents, substitutes the missing sentinel for `PROMPT.md`, `USER.md`, and `MEMORY.md`, and `missing` names `PROMPT.md`, `USER.md`, `MEMORY.md`

#### Scenario: Empty vault still succeeds
- **WHEN** the tool is called for a scope with no foundational files and no session-context template present at any layer
- **THEN** the response is a success result whose `rendered` field is the compiled-in default template with all file slots showing the missing sentinel, and `missing` names all five foundational files (`PERSONA.md`, `PROMPT.md`, `RULES.md`, `USER.md`, `MEMORY.md`)

#### Scenario: No path argument
- **WHEN** a client attempts to pass a `path` or `which` argument
- **THEN** the call is rejected at schema validation because the input schema accepts only scope parameters

### Requirement: `evolve_core_persona` tool
The system SHALL expose an `evolve_core_persona` tool that performs atomic full-file writes to the five foundational session files, accepting exactly one of two argument forms per call. The **single form** takes a required `which` parameter whose value is one of `persona`, `prompt`, `rules`, `user`, `memory`, plus the new `content`; the **batch form** takes an `updates` array of 1 to 5 `{ which, content }` entries with the same `which` domain and no duplicate `which` values. Supplying neither form, both forms, an empty `updates` array, or a duplicate `which` SHALL be rejected with `invalid_argument`. The corresponding target file for each entry is the matching `.md` file (e.g. `which=persona` → `PERSONA.md`, `which=memory` → `MEMORY.md`) resolved relative to the agents folder for the active scope.

The tool SHALL enforce a hard line-count cap on the content for the capped files: `which=rules` content MUST NOT exceed 40 lines, `which=user` content MUST NOT exceed 100 lines, and `which=memory` content MUST NOT exceed 200 lines (counted as newline-separated lines). In the batch form, every entry SHALL be validated — `which` domain, duplicates, line caps, and the write-side link transform — before any file is written; a failing entry SHALL reject the whole call and leave every foundational file unchanged. After validation, each selected file SHALL be replaced atomically. The single-form response is unchanged (the byte count written); the batch-form response SHALL carry a `results` array of `{ which, bytes_written }` in request order. The batch is not transactional across files: a crash mid-apply may leave a prefix of the entries applied, but never a partially written single file.

#### Scenario: Persona update
- **WHEN** the tool is called with `which="persona"` and new content for the active scope
- **THEN** the scope's `PERSONA.md` is replaced atomically and the response is a success result containing the byte count written

#### Scenario: Prompt update
- **WHEN** the tool is called with `which="prompt"` and new content
- **THEN** the scope's `PROMPT.md` is replaced atomically

#### Scenario: Rules update within cap
- **WHEN** the tool is called with `which="rules"` and content of 40 lines or fewer
- **THEN** the scope's `RULES.md` is replaced atomically

#### Scenario: Rules over cap rejected
- **WHEN** the tool is called with `which="rules"` and content of 41 lines
- **THEN** the response is an MCP error with code `invalid_argument` naming the 40-line limit, and `RULES.md` is not changed

#### Scenario: User update within cap
- **WHEN** the tool is called with `which="user"` and content of 100 lines or fewer
- **THEN** the scope's `USER.md` is replaced atomically

#### Scenario: Memory update within cap
- **WHEN** the tool is called with `which="memory"` and content of 200 lines or fewer
- **THEN** the scope's `MEMORY.md` is replaced atomically

#### Scenario: Batch update writes several foundational files in one call
- **WHEN** the tool is called with `updates=[{which:"persona",…},{which:"user",…},{which:"memory",…}]`, every entry within its cap
- **THEN** `PERSONA.md`, `USER.md`, and `MEMORY.md` are each replaced atomically and the response carries `results` with one `{ which, bytes_written }` entry per update, in request order

#### Scenario: One over-cap entry rejects the whole batch
- **WHEN** the tool is called with `updates` containing a valid `persona` entry and a `rules` entry of 41 lines
- **THEN** the response is an MCP error with code `invalid_argument` naming the 40-line limit, and neither `PERSONA.md` nor `RULES.md` is changed

#### Scenario: Duplicate which in a batch is rejected
- **WHEN** the tool is called with `updates` containing two entries with `which="rules"`
- **THEN** the response is an MCP error with code `invalid_argument` and no file is changed

#### Scenario: Exactly one argument form
- **WHEN** the tool is called with both the single `which`/`content` pair and an `updates` array, or with neither
- **THEN** the response is an MCP error with code `invalid_argument` and no file is changed

#### Scenario: User content over the line cap is rejected
- **WHEN** the tool is called with `which="user"` and content exceeding 100 lines
- **THEN** the response is an MCP error with code `invalid_argument`, the message states the 100-line limit, and `USER.md` is unchanged

#### Scenario: Memory content over the line cap is rejected
- **WHEN** the tool is called with `which="memory"` and content exceeding 200 lines
- **THEN** the response is an MCP error with code `invalid_argument`, the message states the 200-line limit, and `MEMORY.md` is unchanged

#### Scenario: Invalid `which`
- **WHEN** the tool is called with `which` (in either form) set to any value other than the five accepted strings
- **THEN** the call is rejected at schema validation (the schema's `which` fields are enums)

#### Scenario: Path argument is rejected
- **WHEN** a client attempts to pass a `path` argument to override the hardcoded targets
- **THEN** the call is rejected at schema validation because the input schema does NOT include a path field

#### Scenario: Refused under readonly policy
- **WHEN** policy is `readonly` and `evolve_core_persona` is invoked with any valid form
- **THEN** the response is an MCP error with code `write_denied`

### Requirement: `update_task_heartbeat` tool
The system SHALL expose an `update_task_heartbeat` tool whose target is hardcoded to the conventional virtual path `HEARTBEAT.md` (resolved relative to the agents folder) for the active scope and which performs an atomic full-file write.

#### Scenario: Heartbeat is replaced atomically
- **WHEN** the tool is called with new heartbeat content for the active scope
- **THEN** the resolved physical `HEARTBEAT.md` file for that scope is replaced atomically and the response is a success result containing the byte count written

### Requirement: `append_diary_entry` tool
The system SHALL expose an `append_diary_entry` tool that appends a timestamped section to today's diary file at the virtual path `diary/<YYYY-MM-DD>.md` (resolved relative to the agents folder) for the active scope. The tool SHALL create the diary file (and its parent directories) if it does not exist, writing a `# <YYYY-MM-DD>` H1 title as the first line of a newly created file. The tool SHALL accept an optional `title` argument: when present, the entry heading is `## <HH:MM:SS> — <title>`; when absent, it is `## <HH:MM:SS>`. The append SHALL be implemented as a read-modify-write through the atomic-write procedure.

#### Scenario: Appends to existing diary
- **WHEN** the tool is called for scope `{agent:"jarvis", user:"tony"}` with `content="Picked up task #42."` and `title="Task pickup"` at local time `14:03:22` on `2026-05-25`, and the scope's diary file for that date already contains prior sections
- **THEN** the server resolves the path to the scope's physical diary file, reads its current contents, appends `\n## 14:03:22 — Task pickup\nPicked up task #42.\n`, and persists the result via the atomic-write procedure

#### Scenario: Appends without a title
- **WHEN** the tool is called with `content` but no `title` at local time `14:03:22`, and the diary file already exists
- **THEN** the server appends `\n## 14:03:22\n<content>\n` (a bare-time heading) and persists it

#### Scenario: Creates diary on first entry of the day
- **WHEN** the tool is called for an active scope with `content` and no `title` at local time `09:00:00` on `2026-05-25` and no file exists at the scope's `diary/2026-05-25.md` virtual path
- **THEN** the server creates any missing parent directories, writes a new file whose contents start with `# 2026-05-25\n\n## 09:00:00\n<content>\n`, and persists it via the atomic-write procedure

#### Scenario: Concurrent appends in the same process are serialised
- **WHEN** two concurrent `append_diary_entry` calls for the same scope target the same diary file
- **THEN** the per-target advisory lock serialises them so the final on-disk file contains both sections, each formatted correctly, with no interleaving

#### Scenario: Path argument is rejected
- **WHEN** a client attempts to pass a `path` argument to override the hardcoded target
- **THEN** the call is rejected at schema validation because the input schema does NOT accept a path field

#### Scenario: Empty content is refused
- **WHEN** the tool is called with an empty `content` string
- **THEN** the response is an MCP error with code `invalid_argument` and the diary file is unchanged

### Requirement: Common tool input contract
The system SHALL ensure every tool's input schema includes the scope parameters whose names are the placeholder idents of `MUNINN_VFS_SCHEME`, and SHALL reject calls whose scope arguments do not satisfy that contract.

#### Scenario: All scheme keys required
- **WHEN** scheme is `<agent>.<user>` and a tool is called with `agent` set but `user` missing
- **THEN** the call is rejected with code `missing_scope` and the message names `user`

#### Scenario: Unexpected scope parameter
- **WHEN** scheme is `<agent>` and a tool is called with both `agent` and `user`
- **THEN** the call is rejected at schema validation because the input schema does NOT include `user` under this scheme

#### Scenario: Custom scheme keys are honoured
- **WHEN** scheme is `<team>.<agent>.<env>.<user>` and a tool is called with exactly those four fields
- **THEN** the call proceeds to resolution with the rendered suffix `<team>.<agent>.<env>.<user>`

#### Scenario: Empty scheme requires no scope arguments
- **WHEN** scheme is the empty string and a tool is called with no scope fields
- **THEN** the call proceeds; if any scope field is supplied, the call is rejected at schema validation

### Requirement: Session-context renderer
The system SHALL provide a single shared renderer that produces session-context markdown for a given scope and a **render kind** (`context` for the full render, `bootstrap` for the lean render). The renderer SHALL resolve the template for that kind (see *Session-context template resolution* and *Session-bootstrap template resolution*), read the five foundational files for the scope, substitute every recognised placeholder, and return the resulting string together with the list of foundational files that were absent. The renderer SHALL be the single source of the `context`-kind output exposed by the `load_session_context` tool, the `session-context` resource, the `session-context` prompt, and `GET /v1/context`, and of the `bootstrap`-kind output exposed by the `session-bootstrap` resource and `GET /v1/bootstrap`. The renderer SHALL NOT produce a `{{tools_guide}}` value; that slot and its generator are removed. The renderer SHALL produce a server-generated `{{scope_directive}}` value: a prominent, single-line imperative directing the agent that every memory tool call must carry the active scope. For a non-empty scope the directive SHALL name the scope keys as `key=value` pairs in deterministic key order (covering exactly the keys the configured scheme defines); for an empty scope it SHALL fall back to generic phrasing that names no specific key. The renderer SHALL also produce a server-generated `{{onboarding_directive}}` value: the empty string when no foundational file is absent, and otherwise a directive instructing the agent to interview the user and commit the foundational files via `evolve_core_persona`.

#### Scenario: Recognised placeholders are substituted
- **WHEN** the active template contains `{{files.persona}}`, `{{files.user}}`, `{{scope.agent}}`, and `{{scope_directive}}`
- **THEN** the renderer replaces them respectively with the contents of `PERSONA.md`, the contents of `USER.md`, the rendered value of the `agent` scope key, and the server-generated scope directive

#### Scenario: Missing foundational file renders a sentinel
- **WHEN** a `{{files.*}}` placeholder names a foundational file that does not exist for the scope
- **THEN** the renderer substitutes a fixed missing sentinel (for example `(not yet recorded — set via evolve_core_persona)`) rather than omitting the placeholder or erroring, and records that file in the absent list

#### Scenario: Unknown placeholder is left literal
- **WHEN** the template contains a `{{…}}` token the renderer does not recognise — including the now-removed `{{tools_guide}}`
- **THEN** the token is left verbatim in the output and a single diagnostic is logged; rendering does not error

#### Scenario: Scope directive names the concrete active scope
- **WHEN** `{{scope_directive}}` is rendered for a non-empty scope such as `{agent: default, user: swag}`
- **THEN** the value is a single-line imperative stating that every memory tool call must carry the active scope, naming the keys as `key=value` pairs in deterministic key order (for example `agent=default, user=swag`), covering exactly the keys the configured scheme defines

#### Scenario: Scope directive falls back to generic phrasing for an empty scope
- **WHEN** `{{scope_directive}}` is rendered for an empty scope
- **THEN** the value keeps the imperative that every memory tool call must carry the scope keys defined by the server's VFS scheme, without naming any specific key

#### Scenario: Onboarding directive is empty in steady state
- **WHEN** the renderer runs for a scope whose five foundational files all exist (the absent list is empty)
- **THEN** `{{onboarding_directive}}` renders as the empty string, so a steady-state session pays nothing for it

#### Scenario: Onboarding directive appears when files are missing
- **WHEN** the renderer runs for a scope with one or more absent foundational files
- **THEN** `{{onboarding_directive}}` renders the directive to interview the user and commit the foundational files via `evolve_core_persona`, in both the `context` and `bootstrap` renders

### Requirement: Session-context template
The system SHALL treat the (full) session-context template as an operator-authored markdown document that may contain `{{files.<name>}}`, `{{scope.<key>}}`, `{{scope_directive}}`, and `{{onboarding_directive}}` placeholders, where `<name>` is one of `persona`, `prompt`, `rules`, `user`, `memory` and `<key>` is a scheme placeholder. It SHALL NOT define a `{{tools_guide}}` placeholder. The placeholder namespace SHALL keep file contents (`{{files.user}}`) distinct from scope values (`{{scope.user}}`). The system SHALL ship a compiled-in default `context` template that delimits each section with XML-style tags rather than `##` headings, so that embedded foundational-file markdown (which typically begins at H2) does not collide with the template's own structure. The default template SHALL lead, directly under the `# Session Context` heading and before the first tag, with the `{{scope_directive}}` placeholder rendered as a **bare banner** (not wrapped in any XML tag). The default template SHALL wrap the foundational (agent-owned) slots in bare tags — `<PERSONA>{{files.persona}}</PERSONA>`, `<RULES>{{files.rules}}</RULES>`, `<MEMORY>{{files.memory}}</MEMORY>`, `<USER>{{files.user}}</USER>`, `<PROMPT>{{files.prompt}}</PROMPT>` — in the section order `PERSONA`, `RULES`, `MEMORY`, `USER`, `PROMPT` after the leading scope banner, followed by the `{{onboarding_directive}}` slot and a single-line pointer directing the agent to the layout surface (`muninn://session-layout` / `GET /v1/layout`) for vault mechanics. The default `context` template SHALL NOT embed the `<MUNINN:TOOLS>` tools-guide section nor the `<MUNINN:LAYOUT>` prose; the layout guidance lives in the memory-layout capability. The default template SHALL leave the internal organization of `MEMORY.md` to the agent/user rather than prescribing a skeleton.

#### Scenario: Default context template leads with a bare scope banner
- **WHEN** the compiled-in default `context` template is rendered
- **THEN** the `{{scope_directive}}` banner appears directly under the `# Session Context` heading and before the `<PERSONA>` tag, rendered as bare markdown that is not enclosed in any XML tag

#### Scenario: Default context template no longer embeds tools guide or layout
- **WHEN** the compiled-in default `context` template is rendered
- **THEN** the output delimits its foundational sections with XML-style tags in the order `<PERSONA>`, `<RULES>`, `<MEMORY>`, `<USER>`, `<PROMPT>`, includes the `{{onboarding_directive}}` slot and a one-line pointer to the layout surface, and does NOT contain an `<MUNINN:TOOLS>` section, a `{{tools_guide}}` value, or the `<MUNINN:LAYOUT>` prose

#### Scenario: Sections are delimited by tags, not H2 headings
- **WHEN** the compiled-in default `context` template is rendered
- **THEN** each foundational file's contents are wrapped in a bare tag (`<PERSONA>…</PERSONA>`, `<RULES>…</RULES>`, `<MEMORY>…</MEMORY>`, `<USER>…</USER>`, `<PROMPT>…</PROMPT>`), so embedded H2 markdown in a foundational file does not collide with the template's section delimiters

#### Scenario: File and scope placeholders are distinct
- **WHEN** a template uses both `{{files.user}}` and `{{scope.user}}`
- **THEN** the former renders the contents of `USER.md` and the latter renders the `user` scope key value

#### Scenario: Memory file placeholder is recognised
- **WHEN** a template uses `{{files.memory}}`
- **THEN** the renderer substitutes the contents of `MEMORY.md` (or the missing sentinel when absent)

### Requirement: Session-bootstrap render and default template
The system SHALL provide a lean `bootstrap` render whose compiled-in default template is compact and ordered server-owned-content-first, containing, in order: a `# Session Bootstrap` heading (distinct from the full `context` render's `# Session Context` heading, so the two surfaces are visibly different documents); the bare `{{scope_directive}}` banner; a single-line pointer directing the agent to call `load_session_context` for the rest of the foundational context (persona, working memory, user profile, workflow prompt) and to read the layout surface (`muninn://session-layout` / `GET /v1/layout`) for vault mechanics; the `{{onboarding_directive}}` slot; and finally the `{{files.rules}}` slot rendered without any wrapping tag, as the last content in the document. The default `bootstrap` template SHALL NOT include the `{{files.persona}}`, `{{files.memory}}`, `{{files.user}}`, or `{{files.prompt}}` slots, any `<PERSONA>`/`<RULES>` wrapper tag, the tools guide, the layout prose, or any server-defined memory-loop or recall/diary directive. The `bootstrap` render SHALL report the same absent-foundational-files list as the `context` render for the same scope.

#### Scenario: Bootstrap render carries the compact core in order
- **WHEN** the `bootstrap` render is produced for a scope whose `RULES.md` exists
- **THEN** the output begins with the `# Session Bootstrap` heading, followed by the bare scope banner and a single-line pointer to `load_session_context` and the layout surface, and the `RULES.md` contents appear last with no wrapping tag

#### Scenario: Bootstrap render omits persona and the heavier sections
- **WHEN** the compiled-in default `bootstrap` template is rendered
- **THEN** the output does NOT contain a `<PERSONA>`, `<RULES>`, `<MEMORY>`, `<USER>`, or `<PROMPT>` tag, does NOT contain the persona contents, and does NOT contain an `<MUNINN:TOOLS>` section or the layout prose

#### Scenario: Bootstrap heading is distinct from the full context render
- **WHEN** the compiled-in default `bootstrap` template and the compiled-in default `context` template are each rendered for the same scope
- **THEN** the `bootstrap` render leads with `# Session Bootstrap` and the `context` render leads with `# Session Context`

#### Scenario: Rules are the final content
- **WHEN** the `bootstrap` render is produced for a scope whose `RULES.md` exists
- **THEN** the `RULES.md` contents appear after the scope banner, the `load_session_context`/layout pointer, and the `{{onboarding_directive}}` slot, with no template content following them

#### Scenario: Bootstrap carries no server-defined memory loop
- **WHEN** the compiled-in default `bootstrap` template is rendered for any scope
- **THEN** the output contains no server-authored recall/capture/diary directive — any such memory-discipline guidance present comes solely from the inlined `RULES.md` contents

#### Scenario: Bootstrap render surfaces the onboarding directive
- **WHEN** the `bootstrap` render is produced for a scope with one or more absent foundational files
- **THEN** the `{{onboarding_directive}}` slot renders the interview-and-`evolve_core_persona` directive; for a scope whose files all exist it renders empty

### Requirement: Session-bootstrap template resolution
The system SHALL resolve the active `bootstrap` template for a scope using a layered lookup, returning the first layer that exists: (1) a per-scope template file `AGENT_SESSION_BOOTSTRAP.md` resolved through the scope suffix mechanism inside the agents folder; (2) the global template file at the path configured by `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE` (default `<root>/AGENT_SESSION_BOOTSTRAP.md`); (3) the compiled-in default `bootstrap` template. Absence of any layer SHALL never be an error.

#### Scenario: Per-scope bootstrap template overrides global
- **WHEN** both a per-scope `AGENT_SESSION_BOOTSTRAP.md` for the scope and a global bootstrap template file exist
- **THEN** the renderer uses the per-scope template

#### Scenario: Default bootstrap template when nothing exists
- **WHEN** neither a per-scope bootstrap template nor the global bootstrap template file exists
- **THEN** the renderer uses the compiled-in default `bootstrap` template

### Requirement: Memory-layout render and default content
The system SHALL provide a layout render for a scope that resolves the layout template (see *Memory-layout template resolution*) and renders it through the template engine with the scope context, so `{{scope.<key>}}` placeholders in an operator-supplied layout resolve (the compiled-in default contains none). The default layout content SHALL carry the vault-mechanics guidance formerly embedded in the session-context `<MUNINN:LAYOUT>` section: the suggested (non-enforced) memory layout with each entry's purpose (root core files `MEMORY.md`, `RULES.md`, `PERSONA.md`, `PROMPT.md`, `USER.md`, `HEARTBEAT.md`; and subfolders `diary/<YYYY-MM-DD>.md`, `workspaces/INDEX.md` + `workspaces/<project>/<item>.md`, `topics/INDEX.md` + `topics/LOG.md` + `topics/<topic>/<fact>.md`, `skills/<skill>/SKILL.md` + `skills/<skill>/references/<name>.md`, `agents/<subagent>/PROMPT.md` + `agents/<subagent>/<context>.md`); the distinction between **core files** (changed only through the dedicated wrapper tools and subject to the documented caps) and all other paths (an ordinary filesystem the agent reads, writes, and organizes freely), without exposing any internal per-scope filename-suffix mechanism; the path-addressing rule that wrapper tools prepend the agents-folder name automatically while the generic note tools require the agents-folder name as the leading segment of a vault-root-relative path, conveyed without hardcoding a specific agents-folder name (so it stays correct under any `MUNINN_AGENTS_DIR`); the tool-managed-file instructions; and the documented caps `USER.md` ≤ 100 lines and `MEMORY.md` ≤ 200 lines. The default layout content SHALL NOT include the missing-files onboarding guidance — that is the renderer's `{{onboarding_directive}}`.

#### Scenario: Layout presents the suggested layout with purposes
- **WHEN** the compiled-in default layout is rendered
- **THEN** it lists the suggested core files and subfolders with each entry's purpose, as non-enforced guidance

#### Scenario: Layout distinguishes core files from a free-form filesystem
- **WHEN** the compiled-in default layout is rendered
- **THEN** it states that core files are changed only through their dedicated wrapper tools and are subject to the documented caps, while every other path behaves like an ordinary filesystem, and it does NOT mention or rely on any internal per-scope filename suffix

#### Scenario: Layout documents the path-addressing rule without hardcoding the agents folder
- **WHEN** the compiled-in default layout is rendered
- **THEN** it states that wrapper tools add the agents-folder prefix automatically and that the generic note tools require the agents-folder name as the leading segment of a vault-root-relative path, with a worked example, and it conveys this without hardcoding a specific agents-folder name so it stays correct under any `MUNINN_AGENTS_DIR`

#### Scenario: Layout omits the onboarding guidance
- **WHEN** the compiled-in default layout is rendered
- **THEN** it does NOT contain the missing-files interview/`evolve_core_persona` guidance, which is rendered instead by the `{{onboarding_directive}}` in the context and bootstrap renders

### Requirement: Memory-layout template resolution
The system SHALL resolve the active layout template for a scope using a layered lookup, returning the first layer that exists: (1) a per-scope template file `AGENT_MEMORY_LAYOUT.md` resolved through the scope suffix mechanism inside the agents folder; (2) the global template file at the path configured by `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE` (default `<root>/AGENT_MEMORY_LAYOUT.md`); (3) the compiled-in default layout content. Absence of any layer SHALL never be an error.

#### Scenario: Per-scope layout overrides global
- **WHEN** both a per-scope `AGENT_MEMORY_LAYOUT.md` for the scope and a global layout template file exist
- **THEN** the renderer uses the per-scope layout

#### Scenario: Default layout when nothing exists
- **WHEN** neither a per-scope layout nor the global layout template file exists
- **THEN** the renderer uses the compiled-in default layout content

### Requirement: Session-context template resolution
The system SHALL resolve the active session-context template for a scope using a layered lookup, returning the first layer that exists: (1) a per-scope template file `AGENT_SESSION_CONTEXT.md` resolved through the scope suffix mechanism inside the agents folder; (2) the global template file at the path configured by `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE` (default `<root>/AGENT_SESSION_CONTEXT.md`); (3) the compiled-in default template. Absence of any layer SHALL never be an error.

#### Scenario: Per-scope template overrides global
- **WHEN** both a per-scope `AGENT_SESSION_CONTEXT.md` for the scope and a global template file exist
- **THEN** the renderer uses the per-scope template

#### Scenario: Global template used when no per-scope template
- **WHEN** no per-scope template exists for the scope but the global template file exists
- **THEN** the renderer uses the global template file

#### Scenario: Default used when nothing exists
- **WHEN** neither a per-scope template nor the global template file exists
- **THEN** the renderer uses the compiled-in default template
### Requirement: Note tools apply the link transform to content

`read_memory_note` SHALL strip the caller's own scope suffix from link targets in
returned content. `write_memory_note` and `append_diary_entry` SHALL expand link
targets in supplied content to their physical form before persisting, subject to
the cross-scope leak guard. These transforms SHALL apply the `wikilink-references`
rules and SHALL be transparent to a caller that uses only clean shortest names.

#### Scenario: Read strips the suffix from link targets
- **WHEN** `read_memory_note` returns an own-scope note whose persisted content
  contains `[[rust.jarvis.tony]]` for scope `{agent:"jarvis", user:"tony"}`
- **THEN** the returned `content` contains `[[rust]]`

#### Scenario: Write expands own-scope link targets
- **WHEN** `write_memory_note` is called for scope rendering to `jarvis.tony` with
  content containing `[[rust]]` resolving to the caller's own `rust.md`
- **THEN** the persisted file content contains `[[rust.jarvis.tony]]`

#### Scenario: Diary append expands link targets
- **WHEN** `append_diary_entry` is called with content containing `[[rust]]`
  resolving to the caller's own note
- **THEN** the appended diary content is persisted with the suffixed link form

### Requirement: Edit matches the persisted link form

`edit_memory_note` SHALL apply the write-side link transform to its
`search_string` and `replace_string` before matching against and writing the
physical file, so that a search containing a clean link target matches the
suffixed form stored on disk.

#### Scenario: Edit search containing a link matches on disk
- **WHEN** the persisted note contains `[[rust.jarvis.tony]]` and
  `edit_memory_note` is called for scope `jarvis.tony` with `search_string`
  containing `[[rust]]`
- **THEN** the search matches the stored line and the edit is applied (it does NOT
  fail with `edit_search_not_found`)

#### Scenario: Edit replacement is expanded
- **WHEN** `edit_memory_note` replaces a line with a `replace_string` containing
  `[[guide]]` resolving to the caller's own `guide.md`
- **THEN** the persisted content contains `[[guide.jarvis.tony]]`

### Requirement: Core-file tools apply the link transform

The core-file wrappers SHALL apply the same link transform as the generic note
tools. `evolve_core_persona` (PERSONA/PROMPT/RULES/USER/MEMORY) and
`update_task_heartbeat` (HEARTBEAT.md) SHALL expand link targets on write, and
`load_session_context` SHALL strip the caller's own suffix from the foundational
files it renders. Line caps SHALL be evaluated against the agent-facing content
(expansion does not change the line count).

#### Scenario: MEMORY.md index expands and renders clean
- **WHEN** `evolve_core_persona` writes `memory` content containing `[[rust]]`
  resolving to the caller's own note, for scope rendering to `jarvis.tony`
- **THEN** the persisted `MEMORY.md` contains `[[rust.jarvis.tony]]`, and a
  subsequent `load_session_context` renders the memory section with `[[rust]]`

#### Scenario: Heartbeat link expands on write
- **WHEN** `update_task_heartbeat` is called with content containing `[[rust]]`
  resolving to the caller's own note
- **THEN** the persisted `HEARTBEAT.md` contains the suffixed link form

### Requirement: `recall_memory_notes` tool registration
The system SHALL register a `recall_memory_notes` tool alongside the existing memory
tools whenever the recall backend is not `off`. Its scope extraction and visibility
semantics SHALL match `list_memory_notes` exactly: it returns only results the caller
could otherwise reach via `list_memory_notes` + `read_memory_note`, and never results
from another scope or from an ignored/hidden note.

#### Scenario: Tool is listed when recall is enabled
- **WHEN** the server starts with a recall backend other than `off`
- **THEN** `recall_memory_notes` appears in the tool listing alongside the existing
  memory tools, taking the same scope keys

#### Scenario: Visibility matches list_memory_notes
- **WHEN** `recall_memory_notes` and `list_memory_notes` are invoked for the same scope
  and policy
- **THEN** every path returned by `recall_memory_notes` is one that `list_memory_notes`
  would also return for that scope; no path outside that visible set ever appears

### Requirement: `write_memory_notes` tool
The system SHALL expose a `write_memory_notes` tool that writes multiple notes in one call. The `notes` argument SHALL be an array of 1 to 20 entries, each an object `{ path, content, append? }` with the same per-entry semantics as `write_memory_note`: `path` is a **vault-root-relative** virtual path, `content` the full new contents (or, with `append: true`, the bytes to append verbatim with a missing note created from `content`). An empty array, an array exceeding 20 entries, a malformed entry, or two entries resolving to the same virtual path SHALL be rejected with `invalid_argument`.

The tool SHALL validate every entry before modifying any file: virtual-path validity, the wrapper-reserved rejection for agents-folder root-level core files, policy write gating by region, visibility filtering, and the write-side link transform including the cross-scope leak guard. Link expansion SHALL resolve against the caller's visible set extended with the batch's own paths, so a link whose target is created by another entry of the same batch resolves exactly as it would after the batch lands. Any validation failure SHALL reject the whole call with the error code the failing entry would have received from `write_memory_note`, naming the offending path, and SHALL leave every file unchanged.

After validation, entries SHALL be applied in request order: a full replace persists through the atomic-write procedure, an append through the locked read-modify-write, and each applied entry SHALL update the recall index synchronously. The result SHALL contain a `results` array with exactly one `{ path, bytes_written }` entry per requested note, in request order, where `path` echoes the requested path verbatim. The batch is not transactional across entries: a crash mid-apply may leave a prefix of the batch applied, but never a partially written single file.

#### Scenario: Batch write lands all entries in order
- **WHEN** the tool is called with three valid entries for the active scope
- **THEN** all three notes are persisted, the recall index reflects each, and the result carries three `{ path, bytes_written }` entries in request order

#### Scenario: Intra-batch link resolves to a note created in the same batch
- **WHEN** scope renders to `jarvis.tony` and one batch creates `Agents/topics/rust/facts.md` and a second entry whose content contains `[[facts]]` resolving to it
- **THEN** the second entry's persisted content contains `[[facts.jarvis.tony]]`, exactly as if the notes had been written sequentially

#### Scenario: Any validation failure leaves the vault untouched
- **WHEN** the tool is called with three entries of which the third targets the wrapper-reserved `Agents/MEMORY.md`
- **THEN** the response is an MCP error with code `path_not_permitted` naming the reserved path and the wrapper tool, and none of the three notes — including the two valid ones — is created or modified

#### Scenario: Cross-scope leak guard rejects the whole batch
- **WHEN** one entry targets a shared-region file and its content contains a `[[wikilink]]` resolving into the caller's own scope
- **THEN** the whole call is refused with the `write_denied`-class cross-scope error naming the offending target and no file is changed

#### Scenario: Append entries append
- **WHEN** an entry carries `append: true` for an existing note
- **THEN** its `content` is appended verbatim (no implicit separator) under the per-target lock, while full-replace entries in the same batch replace their targets

#### Scenario: Duplicate target paths are rejected
- **WHEN** two entries resolve to the same virtual path
- **THEN** the response is an MCP error with code `invalid_argument` and no file is changed

#### Scenario: Batch size limits
- **WHEN** the tool is called with an empty `notes` array or with more than 20 entries
- **THEN** the response is an MCP error with code `invalid_argument` and no file is changed

#### Scenario: Per-entry semantics match the single write
- **WHEN** an entry targets a policy-denied region or a visibility-excluded path
- **THEN** the call fails with the same error code `write_memory_note` would return for that entry

#### Scenario: Writes are immediately recallable
- **WHEN** a batch creates notes in the caller's scope and recall is enabled
- **THEN** a subsequent `recall_memory_notes` call matches the new content without waiting for the watcher

### Requirement: Persona interview guidance
The rendered session-context (the server-generated memory-tools guide and the missing-file sentinel text) SHALL direct an agent whose foundational files are missing to follow an interview-then-commit flow: ask the user as many questions as needed to understand identity, role, working style, and boundaries **before** writing; then distill the answers into the agent's own concise wording — phrased for fast comprehension by future agent sessions rather than as a verbatim transcript of the user's input — and commit all affected foundational files in a single batch `evolve_core_persona` call.

#### Scenario: Rendered guide describes the batch interview flow
- **WHEN** `load_session_context` renders for a scope with missing foundational files
- **THEN** the rendered output instructs the agent to gather answers first and write the missing files in one `evolve_core_persona` call with multiple `updates`, distilled into the agent's own words rather than the user's verbatim phrasing

#### Scenario: Tool description advertises the batch form
- **WHEN** the tool listing is requested
- **THEN** `evolve_core_persona`'s description names both the single `which`/`content` form and the `updates` batch form

