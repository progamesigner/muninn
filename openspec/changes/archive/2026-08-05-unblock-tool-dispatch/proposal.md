# Proposal: unblock-tool-dispatch

## Why

Every MCP tool call is dispatched synchronously inside async handlers (`toolbox.call` in `call_tool`), so blocking work — recall queries, stat-diff reconciles, evicted-scope rebuilds, and tantivy commits, all serialized behind the recall engine's mutex — runs directly on tokio runtime worker threads. Under a Kubernetes CPU limit of 1 the runtime gets a single worker, so one long-held lock (e.g. the minutes-long eager index build) starves the `GET /healthz` future and the kubelet kills a healthy pod. This caused the 2026-08-05 production crash loop.

## What Changes

- Wrap the synchronous toolbox dispatch in `tokio::task::spawn_blocking` for the MCP `call_tool`, `read_resource`, and `get_prompt` handlers, so blocking work never pins a runtime worker and liveness stays responsive regardless of how long the recall engine's lock is held.
- Set an explicit floor on tokio worker threads in `main.rs` so a cgroup CPU limit of 1 cannot reduce the runtime to a single worker.
- Log the eager recall index build: an INFO line when the build starts and an INFO line on completion carrying the elapsed duration (extending the existing `recall index ready` line), so operators can watch build time trend against the startup-probe budget.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `mcp-server`: new requirement — blocking tool/resource/prompt dispatch runs off the async runtime, and the liveness endpoint remains responsive while any tool call is blocked (including during the eager index build).
- `recall-search`: new requirement — the eager index build emits start and completion log lines, the completion line carrying the build duration.

## Impact

- `src/mcp.rs`: `call_tool`, `read_resource`, `get_prompt` handlers move dispatch into `spawn_blocking` (toolbox is already `Arc`; arguments become owned).
- `src/main.rs`: explicit `worker_threads` floor on the runtime builder.
- `src/recall/mod.rs`: build start/duration logging around `ensure_built`.
- No API, schema, or configuration surface changes; no new dependencies.
