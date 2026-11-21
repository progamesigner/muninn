# Spec Delta

## MODIFIED Requirements

### Requirement: `delete_memory_note` tool
The system SHALL expose a `delete_memory_note` tool that removes a single file at the given virtual path, subject to the active policy and own-scope rules. The `path` argument is a **vault-root-relative** virtual path; to target a location inside the agents folder the caller MUST include the agents-folder name as the leading segment. The tool SHALL NOT remove a non-empty directory. When `MUNINN_PRUNE_EMPTY_DIRS` is `true` (the default), after the file is removed the system SHALL remove the target's parent directory if it is now empty, and SHALL repeat for each ancestor until it reaches a directory that is not empty or a protected boundary. The protected boundaries are the vault root, the agents folder, and (when the scheme is non-empty) the caller's per-scope root directory; a boundary directory SHALL never be removed. A directory that still contains any entry, including a hidden file or a file excluded by ignore rules, SHALL be kept. A failure to remove a directory SHALL stop the cleanup and SHALL NOT turn a successful delete into an error. The success result SHALL be `{ deleted: true, removed_dirs }`, where `removed_dirs` lists each removed directory as a clean vault-root-relative virtual path (the caller's per-scope segment removed), deepest first, and is empty when nothing was removed. When `MUNINN_PRUNE_EMPTY_DIRS` is `false`, the tool SHALL leave every parent directory in place and `removed_dirs` SHALL be empty. Inside the agents folder, the target virtual path MUST be under a subfolder; a root-level core file SHALL NOT be deletable through this tool.

#### Scenario: Delete succeeds for own-scope file under writable policy
- **WHEN** policy permits writes in the target's region and the tool is called for an own-scope file under a subfolder that exists
- **THEN** the file is removed via `std::fs::remove_file` and the response is a success result carrying `deleted: true` and a `removed_dirs` array

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

#### Scenario: Deleting the last note removes emptied folders
- **WHEN** `Agents/projects/old/notes.md` is the only file under `Agents/projects/` for the caller's scope and the tool deletes it with the default configuration
- **THEN** the caller's `projects/old/` and `projects/` directories are removed, the per-scope root and the agents folder remain, and the result is `{ deleted: true, removed_dirs: ["Agents/projects/old", "Agents/projects"] }`

#### Scenario: Cleanup stops at a non-empty ancestor
- **WHEN** `Agents/projects/old/notes.md` is deleted and `Agents/projects/` still contains `todo.md`
- **THEN** only `projects/old/` is removed and `removed_dirs` is `["Agents/projects/old"]`

#### Scenario: Hidden or ignored leftovers keep the folder
- **WHEN** the deleted note's folder still contains a hidden file such as `.DS_Store` or a file matched by an ignore rule
- **THEN** that folder and its ancestors are kept and `removed_dirs` is empty

#### Scenario: Boundaries are never removed
- **WHEN** the scheme is empty, the agents folder is the vault root, and the tool deletes the last note under `topics/`
- **THEN** `topics/` is removed and the vault root remains

#### Scenario: Shared-region cleanup stops at the vault root
- **WHEN** policy is `readwrite` and the tool deletes the last file under `Scratch/team/`
- **THEN** `Scratch/team/` and `Scratch/` are removed, the vault root remains, and `removed_dirs` is `["Scratch/team", "Scratch"]`

#### Scenario: Cleanup disabled
- **WHEN** `MUNINN_PRUNE_EMPTY_DIRS=false` and the tool deletes the last note in a folder
- **THEN** the folder remains on disk and `removed_dirs` is empty

### Requirement: `rename_memory_note` tool
The system SHALL expose a `rename_memory_note` tool that moves a single note from `path` to `new_path` (both **vault-root-relative** virtual paths) and rewrites every visible incoming link to resolve to the new location. The destination MUST NOT already exist; a rename onto an existing note SHALL be rejected with a `destination_exists` error and no change on disk. Inside the agents folder, both `path` and `new_path` MUST be under a subfolder; an agents-folder root-level path on either end SHALL be rejected with `path_not_permitted` (core files are wrapper-managed). The active policy MUST permit writing both the source's and the destination's region, and the region of every referring note that requires rewriting; when any of these is not writable the tool SHALL refuse with the appropriate policy error before any mutation. The moved note's own content SHALL be re-run through the write-side link transform for the destination's region, including the cross-scope leak guard. All preconditions SHALL be validated before the first write; mutations are then applied in the order: write destination, rewrite referrers, delete source. After the source is deleted, the source's emptied parent directories SHALL be removed under the same rules, boundaries, and `MUNINN_PRUNE_EMPTY_DIRS` setting as `delete_memory_note`, and a cleanup failure SHALL NOT fail the rename.

#### Scenario: Rename moves content and reports rewrites
- **WHEN** the tool is called with `path="Agents/topics/rust.md"`, `new_path="Agents/topics/rust-lang.md"` under a writable policy
- **THEN** the destination contains the source's content, the source no longer exists, and the response carries `{ renamed: true, path, new_path, notes_rewritten, removed_dirs }`

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

#### Scenario: Moving the last note out of a folder removes the folder
- **WHEN** `Agents/inbox/idea.md` is the only file under the caller's `inbox/` and it is renamed to `Agents/topics/idea.md` with the default configuration
- **THEN** the caller's `inbox/` directory is removed and `removed_dirs` is `["Agents/inbox"]`

#### Scenario: Moving into a subfolder of the source folder keeps it
- **WHEN** `Agents/topics/rust.md` is the only file under `topics/` and it is renamed to `Agents/topics/lang/rust.md`
- **THEN** `topics/` is kept because it contains `lang/`, and `removed_dirs` is empty
