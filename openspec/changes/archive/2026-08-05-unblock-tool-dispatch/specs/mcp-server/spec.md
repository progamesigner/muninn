# mcp-server Delta

## ADDED Requirements

### Requirement: Blocking dispatch stays off the async runtime
The system SHALL execute the synchronous work behind MCP tool calls, resource
reads, and prompt renders on threads outside the async runtime's worker pool, so
that no single request — however long it blocks (filesystem I/O, index
reconciliation, or waiting on the recall engine's lock) — can prevent the
runtime from serving other futures. The `GET /healthz` liveness endpoint SHALL
respond within its probe timeout while any number of tool calls are blocked,
including for the whole duration of the eager recall index build.

#### Scenario: Liveness stays green during the eager index build
- **WHEN** the server starts against a large vault, the eager recall index build
  is still running, and a client issues a `recall_memory_notes` call that blocks
  waiting for the build to finish
- **THEN** `GET /healthz` continues to respond `200 OK` within the probe timeout
  for the entire build, and the blocked tool call completes (or times out at the
  client) without the process being restarted by its orchestrator

#### Scenario: A slow tool call does not starve concurrent requests
- **WHEN** one tool call is executing long-running blocking work (for example a
  stat-diff reconcile of a large scope) and a second, independent request
  arrives (another tool call, a resource read, or a health probe)
- **THEN** the second request is scheduled and answered without waiting for the
  first call's blocking work to yield the runtime

#### Scenario: Runtime keeps multiple workers under a CPU limit of 1
- **WHEN** the server runs in a container whose cgroup CPU quota resolves the
  detected parallelism to 1
- **THEN** the async runtime is still built with more than one worker thread, so
  a single stalled future cannot halt the scheduler
