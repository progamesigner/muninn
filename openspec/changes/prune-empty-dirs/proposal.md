# Proposal

## Why

When an agent deletes or renames away the last note in a folder, the folder stays on disk. Agents cannot see empty folders, so nobody notices until a person browses the vault (for example in Obsidian) and removes them by hand.

## What Changes

- `delete_memory_note` removes the parent folders that become empty after the delete, walking upward and stopping at the first non-empty folder or at a protected boundary (vault root, agents folder, per-scope root).
- `rename_memory_note` applies the same cleanup to the source's parent folders after the source is deleted.
- Both tools report the removed folders as clean virtual paths in a new `removed_dirs` array (empty when nothing was removed).
- A folder that still holds anything, including hidden or ignored files, is kept. Cleanup failures never fail the delete or rename.
- Add `MUNINN_PRUNE_EMPTY_DIRS` (strict boolean, default `true`) and the mirroring `--prune-empty-dirs` CLI flag. Setting it to `false` restores today's behavior.
- Atomic writes recreate a parent folder that disappears between creating it and creating the temp file, so a concurrent cleanup cannot fail a write.
- **BREAKING** (behavioral): by default, deletes and renames now remove emptied folders. The current spec says the parent stays in place. Operators who rely on keeping empty folders set `MUNINN_PRUNE_EMPTY_DIRS=false`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `memory-tools`: `delete_memory_note` and `rename_memory_note` prune emptied parent folders and return `removed_dirs`.
- `configuration`: new `MUNINN_PRUNE_EMPTY_DIRS` variable and `--prune-empty-dirs` flag.
- `vault-storage`: auto-created parent folders are recreated when a concurrent cleanup removes them mid-write.

## Impact

Changes `src/storage.rs` (cleanup and write retry), `src/path.rs` (boundary detection and folder-to-virtual-path mapping), `src/tools.rs` (delete and rename results), `src/config.rs` (new variable and flag), README configuration table, tool schema snapshots, and tests. No new dependencies, no vault migration. Existing empty folders are not cleaned retroactively.
