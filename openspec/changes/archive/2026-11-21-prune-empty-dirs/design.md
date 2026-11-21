# Design

## Context

`Storage::delete` calls `std::fs::remove_file` and nothing else. `rename_memory_note` finishes with the same call on the source. Write paths create parents with `create_dir_all` inside `write_atomic_locked`, then open a `NamedTempFile` in that parent.

Per-target locking (`with_target_lock`) keys on the file path only. Two operations on different files in the same folder never serialize, so folder cleanup and folder creation can interleave.

With a non-empty scheme, an inside-agents virtual path `Agents/topics/a.md` resolves to `<root>/Agents/<scope>/topics/a.<scope>.md`. With an empty scheme there is no per-scope segment, and the agents folder may be the vault root itself.

## Goals / Non-Goals

**Goals:**

- Remove folders emptied by `delete_memory_note` and `rename_memory_note`, safely and without new locks.
- Tell the agent which folders were removed, in its own virtual-path terms.
- Keep writes correct when cleanup races with folder creation.

**Non-Goals:**

- Retroactive cleanup of empty folders that already exist.
- Cleanup after operations other than delete and rename (writes never empty a folder).
- Coordinating with external editors beyond what `rmdir` semantics already give.

## Decisions

### Use non-recursive `std::fs::remove_dir` and walk upward

After a successful `remove_file`, start at the file's parent and call `remove_dir` on each ancestor until one fails or a boundary is reached. `remove_dir` only succeeds on an empty directory, so the operation cannot delete content even if the walk logic has a bug. Any error (`DirectoryNotEmpty`, `NotFound`, permission) ends the walk silently, logged at `debug`.

Alternative: list the directory first and decide based on visible entries. Rejected because it would delete folders that hold hidden or ignored files, which the agent cannot see and should not decide about, and it has a check-then-act race.

### Boundaries come from the resolver

Add a resolver method returning the stop directories for a physical path: the vault root, the agents folder, and for inside-agents paths with a non-empty scheme, the per-scope root `<agents>/<scope>`. The walk stops before touching any of them. It also stops if an ancestor is not under the vault root, which can only happen from a resolver bug.

### Map removed folders back to virtual paths

Add a resolver method that converts a removed physical directory to its clean virtual path by stripping the vault root and, inside the agents folder, the per-scope segment. Folder names carry no suffix (only file stems do), so no further stripping is needed. Results are listed deepest first, matching the removal order.

### Cleanup lives in `Storage`, called from the tool layer

`Storage::delete` keeps its current contract. A new `Storage::prune_empty_parents(&PhysicalPath) -> Vec<PathBuf>` does the walk and returns removed physical dirs; it is a no-op when the setting is off. The tool layer calls it after `delete` (and after the source delete in rename), converts the result with the resolver, and adds `removed_dirs` to the result. Keeping it separate means the rename's internal delete and any future callers can opt in explicitly.

The setting is stored on `Storage` as `prune_empty_dirs: bool`, defaulting to `true` and set with a `with_prune_empty_dirs` builder method. `Storage::new` has dozens of call sites in tests and engines that never delete, so only the server's tool-facing storage passes the configured value.

### Retry folder creation in the write path instead of a directory lock

In `write_atomic_locked`, wrap `mkdirs_for` and `NamedTempFile::new_in` in a loop (`open_temp_for`, which takes a hook that tests use to remove the parent mid-write). If creating the temp file fails with `NotFound`, call `mkdirs_for` again and retry, up to 3 attempts, then return the IO error. The temp file's existence keeps the folder non-empty from then on, so `remove_dir` by a concurrent cleanup fails and the rename into place succeeds.

Alternative: a vault-wide `RwLock`, shared for writers and exclusive for cleanup. Rejected because every write would take an extra lock to protect a rare race, and it would still not cover external processes.

`write_conditional` creates its `.cas-lock` sidecar after `mkdirs_for`; the sidecar lives at the agents root level, which is a boundary, so cleanup never removes its folder. It needs no retry.

### `removed_dirs` is always present

The field is an empty array when the setting is off or nothing was removed, so clients can read it without checking the configuration.

## Risks / Trade-offs

- [A folder an operator created on purpose disappears once its last note is deleted] → `MUNINN_PRUNE_EMPTY_DIRS=false` restores the old behavior; a hidden placeholder such as `.keep` also keeps the folder.
- [External editor creates a file in a folder at the moment cleanup removes it] → `remove_dir` fails on any entry, so the worst case is the editor seeing its folder vanish just before its own create; editors recreate parents or report an error, and no data is lost.
- [Recall watcher or backlink index sees directory removal events] → those components track files, not folders; verify the watcher ignores directory-removal events with a test.
- [Retry loop masks a real persistent `NotFound`] → the bound of 3 attempts returns the original error class.

## Migration Plan

No data migration. Deploying the new version turns cleanup on by default; set `MUNINN_PRUNE_EMPTY_DIRS=false` before upgrading to keep the old behavior. Rollback is the previous binary; folders already removed are not restored, and nothing depended on them.
