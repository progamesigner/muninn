# Loading session context at session start

This guide wires the `GET /v1/bootstrap` endpoint (see [Session context](../README.md#session-context)) into the session-start mechanism of common agent clients, so every new session is bootstrapped with the scope's lean rules automatically — no manual tool call.

> **HTTP transport only.** `GET /v1/bootstrap` is a plain HTTP route served by the HTTP transport. In stdio mode there is no HTTP listener and these hooks cannot reach it. Start the server with the default `--transport http` (or `MUNINN_TRANSPORT=http`), bound to `127.0.0.1:8000` by default.

> **Which endpoint?** Three render endpoints share the same scope binding, auth, and negotiation:
> - **`/v1/bootstrap`** — the **lean** bootstrap, ordered server-owned-content first: a `# Session Bootstrap` heading, the scope banner, a pointer to the full context and the layout, then the scope's `RULES.md` last. The recommended SessionStart payload: small enough to survive a harness's byte budget, and it tells the agent to pull the rest (persona, memory, user profile, workflow prompt) on demand. It imposes no memory loop — recall/diary discipline is whatever your `RULES.md`/`PROMPT.md` define.
> - **`/v1/context`** — the **full** context (adds persona, memory, user profile, workflow prompt). What `load_session_context` returns; fetch it when you want everything injected up front.
> - **`/v1/layout`** — the vault structure and conventions, on demand.
>
> Because `RULES.md` is inlined here, it is capped at **40 lines** (enforced on `evolve_core_persona` writes) so the bootstrap stays within the SessionStart budget; `USER.md` (≤ 100) and `MEMORY.md` (≤ 200) are not in the bootstrap.

## The building block

Every recipe below ultimately runs one request. It returns the rendered bootstrap as `text/markdown`, ready to drop into a system/developer message:

```sh
curl -sf 'http://127.0.0.1:8000/v1/bootstrap?agent=jarvis&user=tony'
```

- Replace `agent`/`user` with your VFS-scheme placeholders and values. Each scheme placeholder is one query parameter.
- `-s` silences progress; `-f` makes curl exit non-zero (and emit nothing) on HTTP errors or a down server, so a stopped Muninn never injects a broken payload or blocks startup.
- Add `-H "Authorization: Bearer <token>"` when the server was started with `--http-bearer` / `MUNINN_HTTP_BEARER`. Prefer referencing it from the environment (`-H "Authorization: Bearer $MUNINN_HTTP_BEARER"`) over inlining the secret.
- Add `-H 'Accept: application/json'` if you need `{ rendered, missing }` instead of raw markdown (for hooks that expect JSON).
- Swap `/v1/bootstrap` for `/v1/context` to inject the full context up front, or `/v1/layout` for the vault conventions.

Quick sanity check before wiring any client:

```sh
curl -sf 'http://127.0.0.1:8000/healthz' && echo ok    # server up?
curl -sf 'http://127.0.0.1:8000/v1/bootstrap?agent=jarvis&user=tony' | head    # bootstrap renders?
```

## Choosing an approach

Two strategies, depending on what the client supports:

1. **Native session-start hook (preferred).** The client runs a command when a session begins and injects its stdout into the model context. Guarantees the context is present on turn one. Claude Code and Codex CLI support this.
2. **MCP fallback (universal).** Every client here already speaks MCP, so the `load_session_context` tool is available without any hook. The catch: the *model* must choose to call it. Make that reliable by instructing it to call `load_session_context` first in the client's always-on instructions file (`AGENTS.md`, system prompt, etc.). Use this where no native session-start hook exists.

## Claude Code (CLI + VS Code / JetBrains extensions)

Claude Code fires a `SessionStart` hook whose stdout is appended to the session context. The extensions read the same `settings.json`, so this one config covers the CLI and both IDE integrations.

Add to `~/.claude/settings.json` (user-level → applies to every project) or a project's `.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "curl -sf 'http://127.0.0.1:8000/v1/bootstrap?agent=jarvis&user=tony' -H \"Authorization: Bearer $MUNINN_HTTP_BEARER\""
          }
        ]
      }
    ]
  }
}
```

Drop the `-H "Authorization: ..."` part if no bearer is configured. Different agents needing different scopes is just a different `?agent=…&user=…` per project file.

## Codex CLI

Codex CLI supports a `SessionStart` hook in `config.toml`; plain stdout (or a JSON `additionalContext` field) is added as developer context. Enable hooks and add the handler to `~/.codex/config.toml`:

```toml
[features]
hooks = true

[[hooks.SessionStart]]
matcher = "startup|resume"

[[hooks.SessionStart.hooks]]
type = "command"
command = "curl -sf 'http://127.0.0.1:8000/v1/bootstrap?agent=jarvis&user=tony' -H \"Authorization: Bearer $MUNINN_HTTP_BEARER\""
statusMessage = "Loading Muninn session context"
timeout = 30
```

The `matcher` selects which `source` values fire the hook (`startup`, `resume`, `clear`, `compact`). Note Codex may render injected context as a visible developer message, and repo-local `.codex/config.toml` hook loading has been version-sensitive — verify with a throwaway `echo` command first if the hook seems silent.

## opencode

opencode has no stdout-injecting session-start hook, but a plugin can subscribe to `session.created`. Two practical options:

**A. MCP fallback (simplest).** Register Muninn as an MCP server in opencode's config and point the model at `load_session_context` via an instructions file. Add to `~/.config/opencode/opencode.json` (or project `.opencode/opencode.json`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "muninn": {
      "type": "remote",
      "url": "http://127.0.0.1:8000/mcp",
      "headers": { "Authorization": "Bearer ${MUNINN_HTTP_BEARER}" }
    }
  },
  "instructions": ["./.opencode/muninn-bootstrap.md"]
}
```

Then `./.opencode/muninn-bootstrap.md`:

```markdown
At the start of every session, call the `load_session_context` MCP tool with
arguments `{ "agent": "jarvis", "user": "tony" }` before doing anything else,
and treat its `rendered` output as your operating context.
```

**B. Plugin on `session.created` (forced injection).** Place a plugin at `~/.config/opencode/plugins/muninn.ts` (global) or `.opencode/plugins/muninn.ts` (project) that fetches `/v1/context` and pushes it into context. The hook receives a context object; fetch the markdown and feed it into the model however your opencode version exposes context injection (e.g. `output.context.push(...)`):

```typescript
import type { Plugin } from "@opencode-ai/plugin"

export const MuninnContext: Plugin = async ({ $ }) => {
  return {
    "session.created": async (_input, output) => {
      const md = await $`curl -sf 'http://127.0.0.1:8000/v1/bootstrap?agent=jarvis&user=tony'`.text()
      if (md) output.context?.push(md)
    },
  }
}
```

The exact injection field on `session.created` varies by opencode version — check the [plugin docs](https://opencode.ai/docs/plugins/) for the current hook signature. When in doubt, prefer option A.

## Any other MCP client (generic fallback)

For any client without a native session-start hook, the universal path is the MCP `load_session_context` tool plus an instruction to call it first. Point the client at the HTTP MCP endpoint:

- URL: `http://127.0.0.1:8000/mcp`
- Header (if a bearer is set): `Authorization: Bearer <token>`
- Tool: `load_session_context` with arguments `{ "agent": "jarvis", "user": "tony" }`

Then add a line to whatever always-on instructions file the client supports (system prompt, `AGENTS.md`, rules file) telling it to call that tool at the start of every session and treat the `rendered` field as its context.

A transport-agnostic alternative that needs no model cooperation: wrap your client launch in a script that prepends the context to the prompt.

```sh
#!/usr/bin/env sh
ctx="$(curl -sf 'http://127.0.0.1:8000/v1/bootstrap?agent=jarvis&user=tony')"
exec your-agent --system "$ctx" "$@"   # adapt to the client's prompt flag
```

## Troubleshooting

- **Hook produces nothing.** Confirm the server is in HTTP mode and reachable: `curl -sf http://127.0.0.1:8000/healthz`. stdio mode has no HTTP endpoint.
- **`401 Unauthorized`.** A bearer is configured but the hook omits or mismatches it. Add `-H "Authorization: Bearer $MUNINN_HTTP_BEARER"` and make sure the variable is exported in the environment the hook runs in.
- **`400 Bad Request`.** A scope parameter is missing, empty, or not a scheme placeholder. The query params must match the server's VFS scheme exactly.
- **Empty but `200`.** The scope's foundational files are absent — a fresh vault renders an instructions-only bootstrap, which is expected, not an error.
