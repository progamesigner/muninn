# Tasks

## 1. Conditional storage

- [x] 1.1 Add content versions, structured conflicts, and cross-process locked atomic replacement; verify independent-process update and creation races and unchanged content on conflicts.
- [x] 1.2 Document content identity, lock lifetime, and supported filesystem assumptions next to storage and in operator docs; verify no unconditional CAS fallback exists.

## 2. Managed brief surface

- [x] 2.1 Add versioned single reads and scoped update_agents_file with bounds, preconditions, existing transforms, and index notifications; verify tool, schema, policy, scope, cap, and wikilink tests.
- [x] 2.2 Protect generic mutations and indirect rename writes, document brief/heartbeat/commands layout, and preserve five-file bootstrap; verify mutation, session-context, and updated schema snapshots.

## 3. Huginn integration and completion

- [x] 3.1 Preserve structured MCP metadata/errors and request read versions in Huginn; verify regression tests and disposable local-server E2E including conflicting writers.
- [x] 3.2 Run repository format, lint, compile/type, architectural, and test checks in both worktrees; record results, complete and archive OpenSpec, and verify signed commits.
- [x] 3.3 Push only feat/m7-briefs and feat/m7 and update Agents M7 progress with commits, checks, and deployment limitations; verify remote branch heads and note contents.
