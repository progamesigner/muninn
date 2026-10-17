# Proposal

## Why

Huginn M7 needs a small, scoped collaboration brief separate from task heartbeat and foundational session context. Concurrent agents need conditional updates so a stale writer cannot silently overwrite a newer brief.

## What Changes

- Add `update_agents_file`, a scoped wrapper for `AGENTS.md`, with explicit expected-version or expected-absence preconditions and structured conflicts.
- Offer optional version metadata on single-note reads without changing default responses.
- Serialize brief comparison and atomic replacement across cooperating Muninn processes sharing a vault.
- Preserve wrapper-only protection, policy, visibility, wikilinks, index updates, and bounded content; document briefs and personal commands in the default layout.
- Verify actual Huginn compatibility against disposable local Muninn servers.

## Capabilities

### New Capabilities

- `collaboration-brief`: Scoped, bounded collaboration briefs with versioned reads and atomic conditional updates.

### Modified Capabilities

None. Existing generic-note and default read behavior is preserved; the new capability specifies opt-in read metadata and the additional managed root file.

## Impact

Changes storage, tool schemas and routing, structured errors, layout documentation, schema snapshots, and integration tests. Adds SHA-256 content versions and uses OS advisory locks on stable sidecar files. Huginn must consume structured MCP results and explicitly request read versions. No production configuration, vault migration, release, or deployment is included.
