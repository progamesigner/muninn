# Tasks: unblock-tool-dispatch

## 1. Dispatch off the runtime

- [x] 1.1 Wrap `toolbox.call` in `tokio::task::spawn_blocking` in `call_tool` (`src/mcp.rs`): move owned `request.name`, `args`, and `grant` into the closure with a cloned `Arc<Toolbox>`, await the join handle, and map a `JoinError` to an MCP internal error
- [x] 1.2 Apply the same `spawn_blocking` treatment to the sync bodies of `read_resource` and `get_prompt` in `src/mcp.rs`
- [x] 1.3 Wrap the `render_session_context` / `render_layout` calls in the HTTP `/v1/context`, `/v1/bootstrap`, and `/v1/layout` handlers (`src/transport/http.rs`) in `spawn_blocking`

## 2. Runtime worker floor

- [x] 2.1 Set `worker_threads(std::thread::available_parallelism().map_or(2, |n| n.get().max(2)))` on the runtime builder in `src/main.rs`

## 3. Build observability

- [x] 3.1 In `ensure_built` (`src/recall/mod.rs`), emit an INFO log when the build actually starts (after the `built` early-return) and add an `elapsed` duration field to the existing `recall index ready` INFO line

## 4. Tests & verification

- [x] 4.1 Add a test that a panicking tool handler still yields a well-formed MCP error result after the `spawn_blocking` change (covers the `JoinError` mapping)
- [x] 4.2 Add a test (or extend an existing recall test) asserting `warm()` on a populated vault logs the ready line with a duration field — via a `tracing` capture layer or by asserting `is_ready()` flips and the code path compiles with the field
- [x] 4.3 Manual/integration verification: with a large test vault, poll `GET /healthz` while a `recall_memory_notes` call arrives mid-build and confirm the probe answers within 1s throughout
- [x] 4.4 Run `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo test` (including `--features recall-tantivy`)
