# Spec Delta

## ADDED Requirements

### Requirement: Empty-folder cleanup variable
The system SHALL honour `MUNINN_PRUNE_EMPTY_DIRS` as a strict boolean (`true`/`false`) that controls whether `delete_memory_note` and `rename_memory_note` remove the parent directories they leave empty. The default SHALL be `true`. The variable SHALL be overridable by a mirroring CLI flag `--prune-empty-dirs <true|false>`, with the CLI flag taking precedence over the environment variable. The effective value SHALL appear in the startup configuration summary alongside the visibility filter settings.

#### Scenario: Cleanup is on by default
- **WHEN** `MUNINN_PRUNE_EMPTY_DIRS` is unset and no CLI flag is given
- **THEN** the effective setting is `true` and deletes and renames remove emptied parent directories

#### Scenario: Disabling cleanup
- **WHEN** `MUNINN_PRUNE_EMPTY_DIRS=false`
- **THEN** deletes and renames leave every parent directory in place

#### Scenario: CLI flag overrides environment
- **WHEN** `MUNINN_PRUNE_EMPTY_DIRS=true` is set in the environment and the process is started with `--prune-empty-dirs false`
- **THEN** the effective setting is `false`

#### Scenario: Invalid boolean
- **WHEN** `MUNINN_PRUNE_EMPTY_DIRS` is set to a value other than `true` or `false`
- **THEN** the process exits non-zero with a stderr message naming the variable and the offending value
