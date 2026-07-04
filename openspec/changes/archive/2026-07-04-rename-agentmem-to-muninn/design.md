# Design — rename AgentMem to Muninn

## Context

The Norse naming scheme renames the AgentMem MCP server to **Muninn**. The name currently lives at five layers of this repo: crate identity (`Cargo.toml`), distribution (Dockerfile, CI, GHCR image), wire protocol (`AGENTMEM_*` env vars, `agentmem://` URI scheme), in-code identifiers, and prose/specs. The deployment is solo-operated; the image and the manifests that set its environment always flip together, and per-device client reconfiguration is coordinated by the P1 plan outside this repo.

Precedent: the archived `2026-06-06-rename-vfs-template-to-scheme` change established the house style for renames — breaking, no aliases, full restated requirement blocks in delta specs.

## Goals / Non-Goals

**Goals:**

- Zero occurrences of `agentmem`/`AGENTMEM` in live code, build, CI, docs, and live specs after the change (verified by grep), excluding `openspec/changes/archive/`. This includes the legacy `<AGENTMEM:TOOLS>`/`<AGENTMEM:LAYOUT>` template-tag literals — renamed to `<MUNINN:TOOLS>`/`<MUNINN:LAYOUT>` too, so no old-brand string survives anywhere live, even in a historical-literal sentinel with no functional effect.
- Behavior identical byte-for-byte apart from renamed strings: same defaults, same tool schemas, same rendered templates modulo the URI scheme (and the renamed tag literal).

**Non-Goals:**

- No backward-compatible env-var aliases, dual URI schemes, or old-image republishing — the rename decision explicitly forbids a mixed state.
- No touching `openspec/changes/archive/` (historical record) or anything outside this repo (manifests, devices, Windmill, DNS, GitHub repo rename).

## Decisions

- **Crate rename drives derived names.** Renaming `package`/`[[bin]]`/`[lib]` to `muninn` automatically renames the tracing target and the test-harness binary handle (`CARGO_BIN_EXE_muninn`). Consequences that do *not* follow automatically and must be edited by hand: `DEFAULT_LOG_FILTER` (`warn,agentmem=info` → `warn,muninn=info` in `src/config.rs`), `CARGO_BIN_EXE_agentmem` references in integration tests, `use agentmem::` imports in tests/benches, and the Dockerfile's copy path + `ENTRYPOINT` (`/agentmem` → `/muninn`). The MCP server-advertised `serverInfo.name` does **not** follow automatically: `Implementation::from_build_env()` reads `env!("CARGO_CRATE_NAME")` where that line is compiled — inside the `rmcp` crate itself — so it always resolved to `"rmcp"`, both before and immediately after this rename. Fixed as part of this change by setting `server_info` explicitly in `MuninnServer::get_info()` (`src/mcp.rs`) from this crate's own `CARGO_PKG_NAME`/`CARGO_PKG_VERSION`.
- **Hard rename of env vars, no aliases.** Alternative considered: read both prefixes for one release. Rejected — it is exactly the「新舊混用」state the decision forbids, and the only consumer (manifests Deployment) flips in the same deploy as the image. The clap `env = "…"` attributes and `Config::build` lookups in `src/config.rs` are the single source; the Dockerfile `ENV` block and CI build arg (`AGENTMEM_RECALL_BACKEND`) follow.
- **Hard rename of the URI scheme.** `agentmem://session-context|session-bootstrap|session-layout/` → `muninn://…` in `src/mcp.rs` constants and in the compiled-in template text (`src/session_context.rs`) that points agents at the layout resource. Clients discover resources via `resources/list`, and devices with hard-coded URIs (session hooks) are already being reconfigured for the new hostname in the same P1 device sweep.
- **Type/idiom renames are mechanical, not architectural.** `AgentmemServer` → `MuninnServer`, `AgentmemError` → `MuninnError`; module structure unchanged. Doc comments and error message text follow.
- **Delta specs restate affected requirements in full**, following house style. Only requirements whose normative text carries a renamed token get a MODIFIED block. (In practice every requirement mentioning the legacy `<AGENTMEM:…>` tag literals also carries another renamed token — an env var or the URI scheme — so none were excluded on that basis alone; the tag literals were renamed to `<MUNINN:…>` within those same MODIFIED blocks.)
- **Cargo.lock regenerated via `cargo check`** after the `Cargo.toml` rename (single root-package entry changes).
- **GitHub URLs in `Cargo.toml` point at `progamesigner/muninn` immediately.** GitHub's rename redirect makes ordering with Han's manual repo rename a non-issue.

## Risks / Trade-offs

- [New image name boots with dead config if manifests lag] → Startup is fail-fast: `MUNINN_ROOT_DIR` missing → crash loop, immediately visible; the P1 plan lands manifests env rename and image swap in one PR. This repo cannot mitigate further by design (no aliases).
- [First push to `ghcr.io/progamesigner/muninn` creates a fresh GHCR package with default visibility/permissions] → Called out in tasks as an out-of-band check after the first tag push.
- [Hidden old-name stragglers (docs, comments, snapshot files)] → Final verification task greps case-insensitively across the repo; the only allowed hits are `openspec/changes/archive/**`.
- [Devices/hooks dereferencing `agentmem://` URIs break at deploy] → Accepted and coordinated by the P1 device sweep (out of scope here); the breakage window coincides with the hostname change those devices must absorb anyway.
- [Insta snapshots or test fixtures may embed the crate name or URIs] → Run the full test suite; regenerate snapshots only where the diff is exactly the renamed tokens.

## Migration Plan

1. Land this change on a feature branch; PR into `main` (signed commits).
2. Han renames the GitHub repo (redirect covers the interim in either order).
3. Tag a release; CI pushes `ghcr.io/progamesigner/muninn`; verify GHCR package visibility.
4. Manifests repo (out of scope here) flips image + env prefix in one PR.

Rollback: revert the PR; the old image tags remain on the old GHCR package untouched.

## Open Questions

None — scope was settled 2026-07-04 (env prefix and URI scheme included; devices/manifests excluded).
