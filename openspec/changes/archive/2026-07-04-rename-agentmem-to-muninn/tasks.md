# Tasks — rename AgentMem to Muninn

## 1. Crate identity

- [x] 1.1 `Cargo.toml`: rename `package.name`, `[[bin]].name`, `[lib].name` to `muninn`; update `repository`/`homepage`/`documentation` URLs to `github.com/progamesigner/muninn`.
- [x] 1.2 Regenerate `Cargo.lock` via `cargo check` (root package entry renames).
- [x] 1.3 `src/config.rs`: change `DEFAULT_LOG_FILTER` to `warn,muninn=info`; update the clap `name = "agentmem"` command attribute and any prose/doc comments.

## 2. Environment variables

- [x] 2.1 Rename all 22 `AGENTMEM_*` env vars to `MUNINN_*` in `src/config.rs` (clap `env = …` attributes, `Config::build` lookups, error messages, doc comments, unit tests).
- [x] 2.2 Sweep remaining `AGENTMEM_*` references in `src/` (e.g. `storage.rs`, `session_context.rs` comments) and `tests/`/`benches/` harnesses.

## 3. Resource URI scheme

- [x] 3.1 `src/mcp.rs`: change `SESSION_CONTEXT_URI_PREFIX`, `SESSION_BOOTSTRAP_URI_PREFIX`, `SESSION_LAYOUT_URI_PREFIX` to `muninn://…`; update the URI-template doc comment.
- [x] 3.2 `src/session_context.rs`: update the compiled-in default template text pointing at `agentmem://session-layout/…` (context and bootstrap variants); also rename the legacy `<AGENTMEM:TOOLS>`/`<AGENTMEM:LAYOUT>` absence-assertion literals to `<MUNINN:TOOLS>`/`<MUNINN:LAYOUT>` (in `src/session_context.rs` and `tests/http_transport.rs`) and the corresponding `memory-tools` spec text — full rename, no old-brand string left anywhere live, even in a historical-literal sentinel.

## 4. Code identifiers

- [x] 4.1 Rename `AgentmemServer` → `MuninnServer` and `AgentmemError` → `MuninnError` across `src/` (mcp.rs, config.rs, error.rs, storage.rs, path.rs, wikilink.rs, transport/http.rs, session_context.rs, …).
- [x] 4.2 Update `use agentmem::…` imports and `CARGO_BIN_EXE_agentmem` references in `tests/` and `benches/`.
- [x] 4.3 Sweep remaining `agentmem`/`AgentMem` doc comments and string literals in `src/`.

## 5. Build & CI

- [x] 5.1 `Dockerfile`: binary copy path `target/…/release/muninn` → `/muninn`, `ENTRYPOINT ["/muninn"]`, `ENV`/`ARG` block to `MUNINN_*`.
- [x] 5.2 `.github/workflows/ci.yml`: artifact names `muninn-<target>`, release binary paths, image `ghcr.io/progamesigner/muninn`, smoke tags `muninn:smoke-*`, build args `MUNINN_RECALL_BACKEND`.

## 6. Docs

- [x] 6.1 `README.md`: title, prose, env-var table, image/repo references; add a naming-rationale sentence to the introduction (Muninn = Odin's raven "memory", twin to Huginn "thought" — hence the name for a memory server).
- [x] 6.2 `docs/security.md` and `docs/session-context-hooks.md`: env vars, URIs, hostnames/examples as applicable.
- [x] 6.3 `CLAUDE.md` and `setup.sh` if they carry the old name.

## 7. Verification

- [x] 7.1 `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` (both default and `recall-tantivy` feature paths as CI does); regenerate insta snapshots only where diffs are exactly renamed tokens.
- [x] 7.2 `grep -rni agentmem` across the repo (excluding `.git/`, `target/`): only allowed hits are `openspec/changes/archive/**` and the untracked `TASKS.md`.
- [x] 7.3 Smoke-run the binary (`MUNINN_ROOT_DIR=… MUNINN_TRANSPORT=http`) and confirm `initialize` advertises server name `muninn` and `resources/list` returns `muninn://…` URIs. (`Implementation::from_build_env()` does not actually track the downstream crate name — its `env!("CARGO_CRATE_NAME")` is baked in at `rmcp`'s own compile time, so it always returned `"rmcp"`, pre-rename included. Fixed by setting `server_info` explicitly in `MuninnServer::get_info()` from this crate's own `env!("CARGO_PKG_NAME"/"CARGO_PKG_VERSION")`; updated the `http_transport.rs` test that had pinned the old `"rmcp"` value.)

## 8. Out-of-band reminders (not blocking this repo's PR)

- [ ] 8.1 After the first tag push, check the new GHCR package `ghcr.io/progamesigner/muninn` visibility and repo linkage (fresh package, settings do not carry over).
