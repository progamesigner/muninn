## MODIFIED Requirements

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
