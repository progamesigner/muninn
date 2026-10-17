# Design

## Context

Existing wrappers reserve all agents-root files and atomically replace targets, but storage mutexes serialize only one Storage instance. MCP reads include human text and structured content; Huginn currently loses metadata by preferring text. See proposal.md for motivation.

## Goals / Non-Goals

Ensure each accepted brief update compares against the exact stored bytes and preserves the winner under concurrent Muninn processes. Keep default read and foundational bootstrap behavior compatible. This does not make arbitrary external editors participate in advisory locking, change heartbeat semantics, or deploy either application.

## Decisions

- SHA-256 over exact UTF-8 stored bytes produces an opaque `sha256:` token. Unlike mtime/length, it cannot miss same-length edits; unlike random persisted counters it needs no metadata transaction or migration. Identical bytes intentionally have identical versions (content identity, not an ordering or revision counter).
- A persistent hidden sidecar adjacent to the resolved brief provides a stable lock inode across atomic target renames. Rust's standard file lock is held across read, comparison, and temp-file/fsync/rename. Never unlink the sidecar after unlocking: that would allow different writers to lock different inodes. The existing process mutex also surrounds this operation. Locks are scoped by resolved physical target, and operating-system close releases them on failures or crashes.
- Compare raw stored bytes from a single read; compute read versions from that same snapshot before stripping links or applying line ranges. Return metadata only for opt-in single reads.
- Require one precondition, including expected absence for creation; return a stable structured `conflict` error for stale or mismatched state. Never retry with an unconditional write.
- Reuse root-wrapper policy/visibility guards and index notifications. Reject rename when the managed brief is an incoming referrer so it cannot bypass conditional writes or overwrite a concurrent edit.
- Enforce agent-facing line/byte bounds and post-expansion byte bounds. Keep AGENTS.md out of FOUNDATIONAL; document it and commands in layout only.

## Risks / Trade-offs

- Advisory lock semantics on shared network storage depend on the mount supporting inter-process/host locks. Validate the actual shared filesystem before future deployment; local tests cover independent processes sharing a directory.
- External filesystem editors do not honor Muninn locks. Their edits are detected by later version comparisons but cannot be made transactional with wrapper writes. Supported concurrent writers use the wrapper.
- Persistent lock sidecars are implementation metadata and must not be removed while any writer may run. They contain no note content. Hidden files retain normal visibility rules.
- Content versions intentionally permit an A-to-B-to-A sequence to reuse A's version; an unchanged content snapshot is valid for replacement.

## Migration Plan

No vault migration is needed. Existing briefs obtain versions on read. Huginn requests `include_version` and preserves structured success/error metadata. Verification launches disposable loopback-only Muninn processes with a temporary root and no production endpoint or vault credentials. Both branches remain unpublished features without release tags or deployment.
