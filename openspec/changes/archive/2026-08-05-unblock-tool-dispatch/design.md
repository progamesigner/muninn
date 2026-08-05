# Design: unblock-tool-dispatch

## Context

See proposal.md — Why. The relevant mechanics:

- `Toolbox` is fully synchronous and shared behind an `Arc` on `MuninnServer`
  (`src/mcp.rs`). The async `call_tool` / `read_resource` / `get_prompt`
  handlers call into it inline, so every blocking operation (vault I/O, the
  recall engine's `std::sync::Mutex`, tantivy commits) runs on a tokio worker.
- The eager index build already runs in `spawn_blocking` (`spawn_recall_warmup`)
  and `GET /healthz` is index-independent by design — the remaining hole is the
  dispatch path pinning workers while it waits on the same lock.
- `main.rs` builds the runtime with `Builder::new_multi_thread()` and default
  worker count, which follows the cgroup CPU quota — 1 worker under a k8s CPU
  limit of 1.

## Goals / Non-Goals

**Goals:**
- Liveness (`/healthz`) responds within probe timeouts no matter what the
  toolbox is doing.
- No change to tool semantics, ordering guarantees, or the recall engine's
  locking model.
- Operators can read the eager build's duration from the logs.

**Non-Goals:**
- Making the recall engine's lock finer-grained (queries will still queue behind
  a running build/reconcile — they just queue in the blocking pool now).
- Persisting the index (separate change: `persistent-recall-index`).
- Async-ifying `Toolbox` internals.

## Decisions

### D1: `spawn_blocking` around dispatch, not around individual hot spots

Wrap the whole `toolbox.call(...)` (and the sync bodies of `read_resource` /
`get_prompt`) in `tokio::task::spawn_blocking`.

- Why not `block_in_place`: semantics are subtle on small runtimes and it
  degrades to blocking on a current-thread runtime; `spawn_blocking` is
  unconditional and the blocking pool (default cap 512) comfortably absorbs
  calls queuing on the engine lock.
- Why not per-call-site wrapping inside the toolbox (e.g. only around tantivy
  commits): the lock-wait itself is the blocker — a caller waiting on the mutex
  held by a 3-minute build blocks just as hard as the build. Wrapping dispatch
  covers every current and future blocking path at one seam.
- Mechanics: clone the `Arc<Toolbox>`, move owned `request.name` / `args` /
  `grant` into the closure, `.await` the `JoinHandle`. A `JoinError` (panicked
  task) maps to an MCP internal error — matching the existing "panics do not
  corrupt stdout" requirement.

### D2: Worker-thread floor in `main.rs`

`Builder::new_multi_thread().worker_threads(available_parallelism().max(2))`.
Belt-and-braces alongside D1: even if some future handler blocks inline again,
a single stalled worker cannot halt the scheduler. Two is enough — the runtime
only ever runs cheap futures once D1 lands.

### D3: Build logging lives in `ensure_built`

Emit the start line and measure elapsed time inside `ensure_built` (around the
actual work, after the `built` early-return), not in `warm()` — so the inline
build-on-first-query path is timed identically to the background warmup, and
repeated calls stay silent. Extend the existing `recall index ready` INFO line
with an `elapsed` field rather than adding a second completion line.

### D4: HTTP `/v1/*` context handlers are wrapped too

`render_session_context` / `render_layout` do vault reads but never touch the
engine lock; still, they go through the same `spawn_blocking` treatment for
consistency — the cost is one line each and it removes the class of bug rather
than the single instance.

## Risks / Trade-offs

- [Queries still serialize behind the engine lock during a build] → Accepted:
  readiness gating keeps traffic away during startup; steady-state reconciles
  are short. The persistent-index change addresses the long-build root cause.
- [`spawn_blocking` adds a thread handoff per tool call] → Microseconds against
  handlers that do filesystem I/O; not measurable.
- [Owned-argument cloning for the closure] → `JsonObject` args are small;
  negligible.

## Migration Plan

Pure runtime-behavior change, no config or API surface. Deploy normally; roll
back by reverting the image. Verify in staging: hammer `/healthz` while issuing
a recall against a large vault during startup — probe latency must stay flat.
