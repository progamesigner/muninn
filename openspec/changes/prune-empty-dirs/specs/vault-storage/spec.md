# Spec Delta

## MODIFIED Requirements

### Requirement: Auto-create parent directories on writes
The system SHALL create any missing parent directories along the physical path during a write inside the agents folder before opening the temp file. For writes outside the agents folder (only possible under `readwrite` policy), parent directories SHALL likewise be auto-created. If a parent directory is removed by a concurrent operation after it was created and before the temp file is opened, the system SHALL recreate it and retry, so the write succeeds; it SHALL give up with an IO error only after a small bounded number of attempts.

#### Scenario: First write into a new scope
- **WHEN** `write_workspace_file` is called for a virtual path inside the agents folder and no directory for the caller's rendered suffix yet exists on disk
- **THEN** the server creates the per-scope directory tree and then performs the atomic write

#### Scenario: First write outside the agents folder under readwrite
- **WHEN** policy is `readwrite` and `write_workspace_file` is called for `Scratch/team/notes.md` where `Scratch/team/` does not yet exist
- **THEN** the server creates the directory tree and then performs the atomic write

#### Scenario: Concurrent folder cleanup does not fail a write
- **WHEN** a write to `Agents/projects/new.md` creates `projects/` and a concurrent delete removes `projects/` as empty before the temp file is opened
- **THEN** the write recreates `projects/` and succeeds with the caller's exact bytes on disk
