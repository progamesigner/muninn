# vault-storage Specification

## Purpose
Store agent memory in a plain-Markdown vault through root-contained virtual paths,
mapping each scope to its own directory and filename suffix. Enforce access policy
and visibility across notes, links, and recall results, and preserve file integrity
with atomic writes and unambiguous edits.

## Requirements
### Requirement: Vault root containment
The system SHALL canonicalise every virtual path against the configured vault root and SHALL reject any resolution whose canonical absolute path is not a descendant of that root.

#### Scenario: Traversal attempt is rejected
- **WHEN** a tool is called with virtual path `../../etc/passwd`
- **THEN** the operation is refused with a structured error of code `path_escapes_root` before any filesystem call is issued

#### Scenario: Symlink escape is rejected
- **WHEN** a symlink inside the vault points to a path outside the vault root, and a tool resolves to that symlink
- **THEN** the operation is refused with code `path_escapes_root`

#### Scenario: Legitimate path inside root is accepted
- **WHEN** a tool is called with a virtual path that resolves under `MUNINN_ROOT_DIR`
- **THEN** the operation proceeds to scheme resolution and policy enforcement

### Requirement: VFS scheme resolution
The system SHALL, on every tool call, validate that the supplied scope arguments exactly match the placeholder idents of the configured `MUNINN_VFS_SCHEME`, and SHALL render the scheme into a single string used as both the per-scope directory segment under the agents folder and the dotted suffix appended to the file stem inside the agents folder.

#### Scenario: Default scheme resolves agent and user
- **WHEN** scheme is `<agent>.<user>`, scope is `{agent:"jarvis", user:"tony"}`, agents folder is `Agents`, and virtual path is `tasks/plan.md`
- **THEN** the resolved physical path is `<root>/Agents/jarvis.tony/tasks/plan.jarvis.tony.md`

#### Scenario: Single-key scheme
- **WHEN** scheme is `<agent>`, scope is `{agent:"jarvis"}`, agents folder is `Agents`, and virtual path is `HEARTBEAT-STATE.md`
- **THEN** the resolved physical path is `<root>/Agents/jarvis/HEARTBEAT-STATE.jarvis.md`

#### Scenario: Multi-key scheme
- **WHEN** scheme is `<team>.<agent>.<env>.<user>`, scope is `{team:"platform", agent:"jarvis", env:"prod", user:"tony"}`, agents folder is `Agents`, and virtual path is `tasks/plan.md`
- **THEN** the resolved physical path is `<root>/Agents/platform.jarvis.prod.tony/tasks/plan.platform.jarvis.prod.tony.md`

#### Scenario: Scheme with literal segment
- **WHEN** scheme is `v1.<agent>.<user>`, scope is `{agent:"jarvis", user:"tony"}`, agents folder is `Agents`, and virtual path is `tasks/plan.md`
- **THEN** the resolved physical path is `<root>/Agents/v1.jarvis.tony/tasks/plan.v1.jarvis.tony.md`

#### Scenario: Empty scheme applies no suffix
- **WHEN** scheme is the empty string and virtual path is `notes.md`
- **THEN** the resolved physical path is `<root>/<agents_dir>/notes.md` with no per-scope directory and no suffix

#### Scenario: Vault root as agents folder
- **WHEN** `MUNINN_AGENTS_DIR=.`, scheme is `<agent>.<user>`, scope is `{agent:"jarvis", user:"tony"}`, virtual path is `tasks/plan.md`
- **THEN** the resolved physical path is `<root>/jarvis.tony/tasks/plan.jarvis.tony.md` and the "outside the agents folder" region is empty

#### Scenario: Missing required scope key
- **WHEN** scheme is `<agent>.<user>` and a tool is called with `agent` set but `user` missing
- **THEN** the call is rejected with code `missing_scope` and a message naming `user`

#### Scenario: Extra scope key
- **WHEN** scheme is `<agent>` and a tool is called with both `agent` and `user`
- **THEN** the call is rejected at schema validation because the input schema does NOT include `user` under this scheme

### Requirement: Region detection
The system SHALL, for every virtual path that passes vault-root containment, classify it as either *inside the agents folder* or *outside the agents folder but inside the vault root*. The agents folder is determined entirely by `MUNINN_AGENTS_DIR`; no globs are involved.

#### Scenario: Path under agents folder
- **WHEN** `MUNINN_AGENTS_DIR=Agents` and virtual path is `Agents/topics/rust.md`
- **THEN** the region is `inside-agents-folder`

#### Scenario: Path outside agents folder
- **WHEN** `MUNINN_AGENTS_DIR=Agents` and virtual path is `Actions/release.md`
- **THEN** the region is `outside-agents-folder`

#### Scenario: Vault root is agents folder
- **WHEN** `MUNINN_AGENTS_DIR=.` and virtual path is `anything.md`
- **THEN** the region is `inside-agents-folder` and the `outside-agents-folder` region is empty

### Requirement: Policy enforcement
The system SHALL enforce permissions according to `MUNINN_POLICY` and the region classification, as follows:

| Policy | Inside agents folder | Outside agents folder |
|---|---|---|
| `scoped` | own-scope read & write (suffix applied) | denied |
| `namespaced` | own-scope read & write (suffix applied) | read-only (no suffix) |
| `readonly` | own-scope read-only (suffix applied) | read-only (no suffix) |
| `readwrite` | own-scope read & write (suffix applied) | read & write (no suffix) |

#### Scenario: scoped denies outside region
- **WHEN** policy is `scoped` and any tool targets a path outside the agents folder
- **THEN** the operation is refused with code `path_not_permitted`

#### Scenario: namespaced permits reads outside
- **WHEN** policy is `namespaced` and an agent reads `Actions/release.md`
- **THEN** the read succeeds, the same physical file `<root>/Actions/release.md` is served to every scope, and no VFS suffix is applied

#### Scenario: namespaced denies writes outside
- **WHEN** policy is `namespaced` and an agent writes to `Actions/release.md`
- **THEN** the write is refused with code `write_denied` and the file is unchanged

#### Scenario: readonly denies writes inside agents folder
- **WHEN** policy is `readonly` and an agent writes to its own scope's file inside the agents folder
- **THEN** the write is refused with code `write_denied` and the file is unchanged

#### Scenario: readwrite permits writes outside
- **WHEN** policy is `readwrite` and an agent writes to `Scratch/team-notes.md`
- **THEN** the write succeeds, the file is created or replaced at `<root>/Scratch/team-notes.md` without a suffix, and every other agent can read it at the same virtual path

### Requirement: Own-scope strictness inside the agents folder
Inside the agents folder, when the scheme is non-empty, the system SHALL only allow read, write, edit, and list operations on files whose physical path's rendered suffix matches the caller's rendered suffix. Files belonging to other scopes SHALL be invisible (absent from listings) AND inaccessible (any direct attempt to address them resolves to `not_found`).

#### Scenario: Other scope's file is unreachable
- **WHEN** the resolver is invoked for scope `{agent:"jarvis", user:"tony"}` on virtual path `tasks/plan.md` and the only file on disk in that directory is `plan.jarvis.sam.md`
- **THEN** the operation is refused with code `not_found` (the resolved file for the caller is `plan.jarvis.tony.md`, which does not exist) and `plan.jarvis.sam.md` is NOT read

#### Scenario: Crafted virtual path cannot reach other scope
- **WHEN** an agent in scope `{agent:"jarvis", user:"tony"}` attempts to address another scope's physical file by passing a virtual path that includes the other scope's suffix in the stem (e.g. `tasks/plan.jarvis.sam.md`)
- **THEN** the resolver applies the caller's own suffix on top, producing `plan.jarvis.sam.md.jarvis.tony.md` which does not exist and is reported as `not_found`; under no input does the resolver ever open another scope's file

#### Scenario: Listing only shows own scope
- **WHEN** `list_workspace_files` is called for scope `{agent:"jarvis", user:"tony"}` and the disk contains files for `jarvis.tony`, `jarvis.sam`, and `friday.tony` under the agents folder
- **THEN** only the `jarvis.tony` files appear in the result, with suffixes stripped

#### Scenario: Empty scheme removes own-scope filtering
- **WHEN** scheme is the empty string, policy is `namespaced`, and `list_workspace_files` is called
- **THEN** all files inside the agents folder are listed (there are no per-scope subdirectories or suffixes to filter by)

### Requirement: Visibility filters
The system SHALL, on every list / read / write / edit / delete operation, apply visibility filters that exclude (a) any path whose any segment begins with `.` when `MUNINN_INCLUDE_HIDDEN=false` (the default) AND the path is not exempted by the include-hidden glob list, and (b) any path matched by an applicable `.ignore`, `.gitignore`, or `.obsidianignore` rule inside the vault when `MUNINN_HONOR_IGNORE_FILES=true` (the default). Ignore files SHALL be honoured **per-directory and nested**, exactly as `git` treats `.gitignore`: a file in any subfolder applies to that subtree and composes with files in ancestor directories, with the rules assembled from the vault root down to the target's parent directory. This composition SHALL apply to all three ignore-file kinds on both the listing path and the direct-access path. The walker semantics SHALL match the `ignore` crate's `WalkBuilder` so per-directory ignore files compose as in `ripgrep` and Obsidian's own search. The set of files excluded by direct read/write/edit/delete checks SHALL be identical to the set the walker hides from listings (the visible set and the addressable set agree for all three ignore-file kinds).

An include-hidden glob list (configured via `MUNINN_INCLUDE_HIDDEN_GLOBS`) SHALL exempt matching dot-paths from hidden filtering. A path is exempt when the path itself OR any of its parent directories (relative to the vault root) matches an include glob; thus matching a directory un-hides that directory and its entire subtree, including nested dot-segments. The list is empty by default, in which case no exemption applies and all dot-segments are excluded as before. Ignore-file rules continue to apply to exempted paths unless `MUNINN_HONOR_IGNORE_FILES=false`. The agents-folder exemption (below) is independent of and unaffected by this glob list.

#### Scenario: Hidden file excluded from listing
- **WHEN** defaults are in effect and the vault contains `Agents/<scope>/notes.md` and `Agents/<scope>/.tmp.md`
- **THEN** `list_memory_notes` returns only `notes.md`; `.tmp.md` is absent

#### Scenario: Hidden file inaccessible by direct read
- **WHEN** defaults are in effect and `read_memory_note` is called with virtual path `Agents/<scope>/.tmp.md`
- **THEN** the response is an MCP error with code `path_not_permitted`

#### Scenario: gitignore-matched file excluded
- **WHEN** `MUNINN_HONOR_IGNORE_FILES=true` and the vault contains a `.gitignore` line `drafts/*.md` plus the file `Agents/<scope>/drafts/wip.md`
- **THEN** `list_memory_notes` does not include `drafts/wip.md` and a direct `read_memory_note` for it returns `path_not_permitted`

#### Scenario: generic .ignore file excludes consistently across listing and direct access
- **WHEN** `MUNINN_HONOR_IGNORE_FILES=true` and the vault contains a `.ignore` line `scratch/*.md` plus the file `Agents/<scope>/scratch/wip.md`, with no matching `.gitignore` or `.obsidianignore` rule
- **THEN** `list_memory_notes` does not include `scratch/wip.md` AND a direct `read_memory_note`, `write_memory_note`, `edit`, or `delete` targeting it returns `path_not_permitted`

#### Scenario: Nested ignore file in a subfolder is honoured
- **WHEN** `MUNINN_HONOR_IGNORE_FILES=true` and the vault contains `Agents/<scope>/drafts/.gitignore` with the line `*.tmp.md`, plus the files `Agents/<scope>/drafts/wip.tmp.md` and `Agents/<scope>/keep.tmp.md`
- **THEN** `list_memory_notes` excludes `drafts/wip.tmp.md` (the nested rule applies to its own subtree) and a direct access to it returns `path_not_permitted`, while `keep.tmp.md` outside that subtree remains visible and accessible
- **AND** the same exclusion holds when the nested file is `.ignore` or `.obsidianignore` instead of `.gitignore`

#### Scenario: Including hidden files globally
- **WHEN** `MUNINN_INCLUDE_HIDDEN=true`
- **THEN** dotfiles appear in listings and are directly readable (still subject to ignore-file rules unless also disabled), and the include-hidden glob list has no further effect

#### Scenario: Include-glob un-hides a dot-directory subtree
- **WHEN** `MUNINN_INCLUDE_HIDDEN=false`, `MUNINN_INCLUDE_HIDDEN_GLOBS=.obsidian/**`, and the vault contains `.obsidian/app.json`, `.obsidian/plugins/x/data.json`, and an unrelated `.cache/tmp.md`
- **THEN** `list_memory_notes` includes `.obsidian/app.json` and `.obsidian/plugins/x/data.json` and they are directly readable/writable, while `.cache/tmp.md` remains hidden and returns `path_not_permitted` on direct access

#### Scenario: Include-glob does not widen beyond its match
- **WHEN** `MUNINN_INCLUDE_HIDDEN=false`, `MUNINN_INCLUDE_HIDDEN_GLOBS=.obsidian/**`, and the vault contains `.obsidian/app.json` and a sibling `.git/config`
- **THEN** `.obsidian/app.json` is visible while `.git/config` remains excluded and returns `path_not_permitted` on direct access

#### Scenario: Disabling ignore-file enforcement
- **WHEN** `MUNINN_HONOR_IGNORE_FILES=false`
- **THEN** `.ignore`, `.gitignore`, and `.obsidianignore` patterns are not consulted; the visible set is widened accordingly

#### Scenario: Agents folder itself never filtered out
- **WHEN** `MUNINN_AGENTS_DIR=.agents` (begins with `.`) and `MUNINN_INCLUDE_HIDDEN=false`
- **THEN** the agents folder is still recognised as the scoped/suffixed region and its contents remain visible to and writable by the owning scope; hidden filtering does NOT exclude the agents folder, independent of any include-hidden glob list
### Requirement: Atomic full-file writes
The system SHALL implement every full-file write as: create a temp file in the same directory as the target, write contents to the temp file, fsync, then rename the temp file over the target.

#### Scenario: Crash during write leaves target intact
- **WHEN** the server is killed after writing the temp file but before the rename completes
- **THEN** the target file on disk is unchanged from its prior contents (or absent if it never existed)

#### Scenario: Successful write replaces target atomically
- **WHEN** `write_workspace_file` succeeds
- **THEN** the target file at the resolved physical path contains exactly the bytes supplied by the caller and no other file is created in the parent directory

### Requirement: Edit precondition uniqueness
The system SHALL refuse an `edit_workspace_file` call whose `search_string` occurs zero times or more than once in the current target file.

#### Scenario: Search string occurs once
- **WHEN** the search string appears exactly once in the file
- **THEN** the server replaces that single occurrence and persists the result via the atomic write procedure

#### Scenario: Search string is missing
- **WHEN** the search string does not appear in the file
- **THEN** the call is rejected with code `edit_search_not_found` and the file is unchanged

#### Scenario: Search string is ambiguous
- **WHEN** the search string appears two or more times in the file
- **THEN** the call is rejected with code `edit_search_ambiguous`, the file is unchanged, and the error message advises the client to provide a longer, unique snippet

### Requirement: Auto-create parent directories on writes
The system SHALL create any missing parent directories along the physical path during a write inside the agents folder before opening the temp file. For writes outside the agents folder (only possible under `readwrite` policy), parent directories SHALL likewise be auto-created.

#### Scenario: First write into a new scope
- **WHEN** `write_workspace_file` is called for a virtual path inside the agents folder and no directory for the caller's rendered suffix yet exists on disk
- **THEN** the server creates the per-scope directory tree and then performs the atomic write

#### Scenario: First write outside the agents folder under readwrite
- **WHEN** policy is `readwrite` and `write_workspace_file` is called for `Scratch/team/notes.md` where `Scratch/team/` does not yet exist
- **THEN** the server creates the directory tree and then performs the atomic write

### Requirement: Own-scope strictness extends to link targets in content

The own-scope strictness guarantee SHALL extend from filenames to link targets
embedded in note content. Inside the agents folder with a non-empty scheme, a
suffixed link target persisted in a note SHALL carry only the owning scope's
rendered suffix, and the system SHALL never persist a link bearing another scope's
suffix in a file readable by a different scope.

#### Scenario: Persisted own-scope link carries only the owner's suffix
- **WHEN** scope `{agent:"jarvis", user:"tony"}` writes an own-scope note linking
  to its own `rust.md`
- **THEN** the persisted link target is `rust.jarvis.tony` and contains no other
  scope's suffix

#### Scenario: A scoped suffix is never persisted in a shared file
- **WHEN** any caller writes a file in the shared region whose content links to a
  note in that caller's own scope
- **THEN** the write is refused before any bytes are written, so no scoped suffix
  is ever persisted in a shared file

### Requirement: Link transform respects visibility filters

Link resolution SHALL only consider notes that pass the existing visibility
filters (hidden-segment and ignore-file rules). A note excluded by those filters
SHALL NOT be a resolution candidate and SHALL be treated as outside the caller's
visible set.

#### Scenario: Ignored note is not a link target
- **WHEN** a `.gitignore` rule excludes `drafts/wip.md` and the caller writes a
  note containing `[[wip]]` with no other matching note
- **THEN** `wip` does not resolve (the excluded note is not a candidate) and the
  link is left verbatim as dangling

### Requirement: Own-scope strictness extends to recall results
The own-scope strictness guarantee SHALL extend to content recall: a
`recall_memory_notes` result — its hit paths, scores, and snippets — SHALL only ever
derive from notes in the caller's own scope or the shared region the active policy
permits. Because recall is backed by per-scope in-memory indexes, content from
another scope SHALL be structurally unreachable by a recall query, not merely
filtered out, and ignored/hidden notes SHALL never enter any index.

#### Scenario: Recall cannot cross a scope boundary
- **WHEN** a recall is issued for scope `{agent:"jarvis", user:"tony"}` and the vault
  contains matching notes for `jarvis.sam`
- **THEN** the query opens only tony's index (and the shared index when permitted),
  so no byte of `jarvis.sam`'s content can appear in any hit, path, or snippet

#### Scenario: Ignored content stays out of the index
- **WHEN** a note is excluded by an active `.gitignore`/`.obsidianignore`/`.ignore`
  rule or by hidden filtering
- **THEN** it is never indexed and never returned by recall, consistent with how it is
  hidden from `list_memory_notes` and `read_memory_note`
