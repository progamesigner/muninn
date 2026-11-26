# Spec Delta

## ADDED Requirements

### Requirement: Memory-conventions file configuration
The system SHALL honour `MUNINN_MEMORY_CONVENTIONS_FILE`, and the `--memory-conventions-file` CLI flag that overrides it, as the filesystem path to the global memory-conventions document appended after the layout. The default value SHALL be `<root>/AGENT_MEMORY_CONVENTIONS.md`. A relative value SHALL be interpreted relative to the vault root. The configured file need not exist; when it is absent, nothing is appended (subject to the layered resolution defined in the memory-tools capability).

#### Scenario: Default global conventions path
- **WHEN** `MUNINN_MEMORY_CONVENTIONS_FILE` is unset
- **THEN** the global conventions path resolves to `<root>/AGENT_MEMORY_CONVENTIONS.md`

#### Scenario: Custom global conventions path
- **WHEN** `MUNINN_MEMORY_CONVENTIONS_FILE=/etc/muninn/conventions.md`
- **THEN** the server reads the global conventions from that path

#### Scenario: Configured conventions file absent is not an error
- **WHEN** `MUNINN_MEMORY_CONVENTIONS_FILE` points to a path that does not exist
- **THEN** the server starts successfully and the layout render appends nothing
