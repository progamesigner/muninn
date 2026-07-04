# Rename AgentMem to Muninn

## Why

The homelab service fleet adopted the Norse naming scheme: first-party products carry mythological names, and `agentmem` becomes **Muninn** — Odin's raven "memory", twin to the planned Huginn agent service. This change covers the repo-side portion of that rename; the decision mandates a full rename with no old/new mixed state.

## What Changes

- **BREAKING**: Rename the crate, binary, and lib from `agentmem` to `muninn` (`Cargo.toml` package/bin/lib names). The MCP server-advertised implementation name follows automatically via `Implementation::from_build_env()`, as do the `CARGO_BIN_EXE_*` test-harness handles and the tracing target.
- **BREAKING**: Rename every environment variable from the `AGENTMEM_*` prefix to `MUNINN_*` (22 variables). No backward-compatible aliases (solo deployment; image and manifests flip together).
- **BREAKING**: Rename the MCP resource URI scheme `agentmem://` to `muninn://` (`session-context`, `session-bootstrap`, `session-layout` prefixes), including the scheme as it appears inside rendered default templates.
- **BREAKING**: The default log filter changes from `warn,agentmem=info` to `warn,muninn=info` (the crate-named tracing target follows the crate rename).
- **BREAKING**: The container image moves to `ghcr.io/progamesigner/muninn`; the binary inside the image becomes `/muninn` (Dockerfile copy path and `ENTRYPOINT`).
- Rename in-code types and modules that carry the old name (`AgentmemServer` → `MuninnServer`, `AgentmemError` → `MuninnError`, etc.).
- Update CI workflow artifact names (`agentmem-<target>` → `muninn-<target>`), smoke-test tags, and image references.
- Update `Cargo.toml` repository/homepage/documentation URLs to `github.com/progamesigner/muninn`; regenerate `Cargo.lock`.
- Update README, `docs/`, and other repo prose from AgentMem to Muninn. The README introduction additionally gains a short naming-rationale sentence: Muninn is Odin's raven "memory" (twin to Huginn, "thought"), a fitting name for a long-term memory server.
- The legacy template tag names referenced by negative requirements (asserted absent from rendered output) are renamed too, `<AGENTMEM:TOOLS>` / `<AGENTMEM:LAYOUT>` → `<MUNINN:TOOLS>` / `<MUNINN:LAYOUT>` — the decision mandates no old/new mixed state even in historical-literal sentinels with no functional effect.

Out of scope (handled outside this repo, per the P1 plan): the GitHub repo rename itself, manifests/K8s component and hostname changes, per-device MCP client configuration, Windmill resource paths, DNS/TLS, and vault note updates. The `openspec/changes/archive/` tree is a historical record and is not touched.

## Capabilities

### New Capabilities

None — this is a rename of existing capabilities; no capability is added or removed.

### Modified Capabilities

- `configuration`: all `AGENTMEM_*` variables restated as `MUNINN_*`; the default log filter becomes `warn,muninn=info`; example paths such as `/etc/agentmem/…` updated.
- `mcp-server`: the binary name `agentmem` → `muninn`; `AGENTMEM_*` references → `MUNINN_*`; the `agentmem://` resource URI scheme → `muninn://`.
- `context-http-api`: `AGENTMEM_HTTP_*` / `AGENTMEM_VFS_SCHEME` references → `MUNINN_*`; the layout-parity scenario's `agentmem://session-layout/{…}` reference → `muninn://`.
- `container-image`: the published image `ghcr.io/progamesigner/agentmem` → `ghcr.io/progamesigner/muninn`; the `agentmem` binary in the image → `muninn`.
- `recall-search`: `AGENTMEM_RECALL_*` / `AGENTMEM_TIMEZONE` references → `MUNINN_*`.
- `vault-storage`: `AGENTMEM_ROOT_DIR` / `AGENTMEM_AGENTS_DIR` / `AGENTMEM_VFS_SCHEME` / `AGENTMEM_POLICY` / ignore- and hidden-related variables → `MUNINN_*`.
- `memory-tools`: `AGENTMEM_VFS_SCHEME` / template-file variables → `MUNINN_*`; `agentmem://session-layout` pointers in template requirements → `muninn://`; the legacy `<AGENTMEM:TOOLS>` / `<AGENTMEM:LAYOUT>` tag literals → `<MUNINN:TOOLS>` / `<MUNINN:LAYOUT>`.

## Impact

- **Code**: `Cargo.toml`, `Cargo.lock`, all of `src/` (env-var constants in `config.rs`, URI prefixes in `mcp.rs`, type names, doc comments, embedded default template text in `session_context.rs`), `benches/`, `tests/` (imports, `CARGO_BIN_EXE_*`, env vars in harnesses).
- **Build/distribution**: `Dockerfile` (binary path, `ENV` block, `ENTRYPOINT`), `.github/workflows/ci.yml` (artifact names, image name, build args, smoke tags). First push to `ghcr.io/progamesigner/muninn` creates a new GHCR package — visibility/repo linkage must be re-checked out-of-band.
- **Docs**: `README.md`, `docs/security.md`, `docs/session-context-hooks.md`, `CLAUDE.md` if applicable.
- **Compatibility**: BREAKING for any deployment setting `AGENTMEM_*` variables, pulling the old image, or dereferencing `agentmem://` URIs (e.g. session hooks). The P1 plan coordinates those flips; this repo provides no transition shims.
- **Specs**: seven live capability specs restate their name-bearing requirements (delta specs in this change); archived changes stay untouched.
