# Collaboration Brief Specification

## Purpose

Provide a small collaboration brief for each memory scope, with explicit conflict detection so cooperating agents cannot silently overwrite newer instructions.

## Requirements

### Requirement: Versioned note reads
The `read_memory_note` tool SHALL accept an optional boolean `include_version`. When true, its structured result SHALL include an opaque `version` identifying the complete stored content read in that same snapshot, before link stripping or slicing. Versions SHALL remain stable for identical stored bytes. Default responses SHALL remain unchanged. Version metadata SHALL compose with ranges and backlinks and SHALL obey existing policy, visibility, and scope rules.

#### Scenario: Read a sliced brief with a version
- **WHEN** a caller requests one line with `include_version=true`
- **THEN** the result includes that line, `total_lines`, and the same version as a full read of unchanged stored content

#### Scenario: Default read remains compatible
- **WHEN** `include_version` is absent or false
- **THEN** no version field is returned

### Requirement: Scoped conditional brief wrapper
The system SHALL expose `update_agents_file`, targeting only `AGENTS.md` relative to the configured agents folder and active scope. It SHALL accept `content` and exactly one active precondition: a nonempty `expected_version`, or `expected_absent=true`. Null or absent version with absent or false expected absence SHALL be invalid. Both active preconditions SHALL be invalid. The wrapper SHALL apply existing scope validation, grants, policy, visibility, wikilink expansion, recall, and backlink maintenance. It SHALL reject content exceeding 200 lines or 65536 UTF-8 bytes, and reject expanded stored content exceeding 65536 bytes, before writing. Success SHALL return `version` and `bytes_written` for the persisted content.

#### Scenario: Create and update a brief
- **WHEN** a scoped caller creates an absent brief with `expected_absent=true` and later updates with its returned version
- **THEN** each write replaces the whole file atomically and returns a version accepted by a subsequent versioned read

#### Scenario: Invalid preconditions or bounds
- **WHEN** both preconditions, neither precondition, an empty version, or over-limit content is supplied
- **THEN** the call returns `invalid_argument` and leaves the brief unchanged

#### Scenario: Scope and policy remain enforced
- **WHEN** a caller supplies an unauthorized scope, ignored target, or read-only policy
- **THEN** the existing structured scope, visibility, or write denial applies without changing any brief

### Requirement: Atomic conflict detection
Comparison of expected state and replacement SHALL be serialized across independent cooperating Muninn processes sharing the same vault on a filesystem supporting advisory locks and atomic rename. A stale version, absent target with an expected version, or existing target with expected absence SHALL return an error result with structured code `conflict` and SHALL leave the current brief unchanged. There SHALL be no unconditional overwrite fallback.

#### Scenario: Competing updates
- **WHEN** independent processes concurrently submit distinct updates using the same expected version
- **THEN** exactly one succeeds, the other returns `conflict`, and the stored file contains the successful update

#### Scenario: Competing initial creations
- **WHEN** independent processes concurrently create the same missing brief using expected absence
- **THEN** exactly one succeeds and the other returns `conflict`

### Requirement: Managed brief boundaries
Generic write, append, batch write, edit, delete, frontmatter update, and rename SHALL NOT mutate the managed brief, including through an in-vault directory alias. A rename requiring an incoming-link rewrite of the brief SHALL fail before any mutation. Root reservation errors for the brief SHALL name `update_agents_file`. The brief SHALL NOT become a sixth foundational context file, missing-file requirement, or onboarding prompt. The default layout SHALL document its bounds, conditional wrapper, distinction from heartbeat, and `commands/<name>.md` convention.

#### Scenario: Generic mutation cannot bypass the wrapper
- **WHEN** a caller targets the brief through a generic mutation or renames a note referenced by the brief
- **THEN** the entire operation fails with `path_not_permitted` before any file is changed

#### Scenario: Brief is optional session content
- **WHEN** the session context is loaded with or without a brief
- **THEN** foundational content and its five-file missing list remain independent of the brief
