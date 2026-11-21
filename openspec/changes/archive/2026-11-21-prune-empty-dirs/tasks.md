# Tasks

## 1. Configuration

- [x] 1.1 Add `MUNINN_PRUNE_EMPTY_DIRS` (strict boolean, default `true`) and the `--prune-empty-dirs` CLI flag in `src/config.rs`, include it in the startup summary, and verify with config unit tests for default, `false`, CLI-over-env precedence, and invalid value
- [x] 1.2 Add a `prune_empty_dirs` setting to `Storage` (default `true`, set via `with_prune_empty_dirs`) and pass the configured value in `src/mcp.rs`; verify `cargo build --all-targets` succeeds
- [x] 1.3 Add the variable to the README configuration table next to the visibility variables and verify the row matches the spec wording

## 2. Resolver helpers

- [x] 2.1 Add a resolver method returning the cleanup boundaries (vault root, agents folder, per-scope root when the scheme is non-empty) and verify unit tests for scheme, empty scheme, and agents-folder-is-vault-root layouts
- [x] 2.2 Add a resolver method mapping a physical directory to its clean virtual path, stripping the per-scope segment inside the agents folder, and verify unit tests for inside and outside the agents folder

## 3. Storage cleanup and write retry

- [x] 3.1 Implement `Storage::prune_empty_parents` with non-recursive `remove_dir`, stopping at the first error or boundary and returning removed dirs deepest first; verify storage tests for full cleanup, stop at non-empty ancestor, hidden leftover, boundaries kept, and setting off
- [x] 3.2 Add the bounded retry around folder creation and temp-file creation in `write_atomic_locked`; verify with a test that removes the parent between `mkdirs_for` and temp-file creation (via a test hook) and still writes the exact bytes
- [x] 3.3 Update the `Storage::delete` doc comment so it no longer says the parent is always left in place, and verify `cargo doc` builds without warnings for `storage.rs`

## 4. Tool integration

- [x] 4.1 Call the cleanup after `delete_memory_note` and return `removed_dirs`; verify integration tests in `tests/tools.rs` for the delete scenarios in the spec, including shared-region `readwrite` cleanup stopping at the vault root
- [x] 4.2 Call the cleanup after the source delete in `rename_memory_note` and return `removed_dirs`; verify integration tests for moving the last note out of a folder and moving into a subfolder of the source folder
- [x] 4.3 Update the delete and rename tool descriptions to mention folder cleanup and `removed_dirs`, then refresh schema snapshots with `INSTA_UPDATE=always cargo test --test schema_snapshots` and review the diff
- [x] 4.4 Verify recall and backlink indexes stay consistent after a delete that removes folders: a recall and a backlink lookup in the same scope return no hits for the deleted note and no errors

## 5. Integration checks

- [x] 5.1 Run `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo test` and verify all pass
- [x] 5.2 Run `openspec validate prune-empty-dirs --strict` and verify it reports the change as valid
