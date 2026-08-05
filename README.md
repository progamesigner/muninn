# Muninn

An MCP server fronting a plain-markdown vault for durable, multi-tenant
agent memory. One server process serves many agents concurrently; each tool call
carries its own scope (agent, user, …) so files are namespaced on disk while a
human can still browse and hand-edit the vault directly with any editor (Obsidian
works well for it). The name comes from Muninn, Odin's raven "memory" — twin
to Huginn, "thought" — a fitting name for a long-term memory server.

> **Status: released.** The current capability specs live under
> [`openspec/specs/`](openspec/specs/); proposed and archived changes live under
> [`openspec/changes/`](openspec/changes/).

## What it does

- Exposes a small, generic tool surface (nine tools — see [Tools](#tools)).
- Speaks the official MCP protocol over both **HTTP** (default, `127.0.0.1:8000`)
  and **stdio** via the [`rmcp`](https://crates.io/crates/rmcp) SDK.
- Namespaces each agent's files with a configurable VFS suffix scheme and
  enforces a single server-wide policy across two regions (inside / outside the
  agents folder).
- Writes atomically (temp-file + fsync + rename), prevents path traversal, honours
  `.ignore`/`.gitignore`/`.obsidianignore` (nested, per-directory) and hidden-file
  filters, and keeps all logs on stderr.

See [`docs/security.md`](docs/security.md) for the trust model.

## Install

```sh
cargo install muninn            # from crates.io (once published)
cargo install --path .            # from a checkout
```

Pre-built release binaries for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
and `x86_64-pc-windows-msvc` are attached to each tagged release.

## Container image

A minimal multi-arch image (`linux/amd64`, `linux/arm64`) is published to the
GitHub Container Registry on every tagged release. It is a statically linked
binary on `scratch` (no shell or OS userland) running as a non-root user.

```sh
docker pull ghcr.io/progamesigner/muninn:latest
```

Tags per release: `:<version>` (e.g. `:0.1.0`), the moving `:latest`, and an
immutable `:sha-<gitsha>`.

The published image is built with the [tantivy recall backend](#recall) and
defaults `MUNINN_RECALL_BACKEND=tantivy`, so BM25 recall works out of the box;
set `MUNINN_RECALL_BACKEND=simple` (or `off`) to opt out. The lightweight
`simple` backend is not published as an image — build it from source
(`cargo build`) or use a [release binary](#install).

Run it with a vault mounted at `/vault` (the image sets `MUNINN_ROOT_DIR=/vault`
and binds the HTTP transport to `0.0.0.0:8000`):

```sh
# The mounted vault must be writable by the image's non-root UID (65532).
docker run --rm -p 8000:8000 \
  -e MUNINN_HTTP_BEARER=change-me \
  -v "$PWD/vault:/vault" \
  ghcr.io/progamesigner/muninn:latest
```

Set `MUNINN_HTTP_BEARER` for any deployment reachable off-host — the endpoint
is otherwise unauthenticated (a startup `WARN` is logged). Override any
[configuration variable](#configuration) with `-e`.

**Per-tenant scoped tokens.** When one server is shared by several agents, mount
a tokens file and set `MUNINN_HTTP_TOKENS_FILE` so each client's bearer is
confined to its own scopes (see [docs/security.md](docs/security.md) for the
grant model):

```sh
# tokens.json: { "tokens": [ { "token": "…", "scopes": { "agent": "jarvis", "user": "*" } } ] }
docker run --rm -p 8000:8000 \
  -e MUNINN_HTTP_TOKENS_FILE=/run/secrets/tokens.json \
  -v "$PWD/tokens.json:/run/secrets/tokens.json:ro" \
  -v "$PWD/vault:/vault" \
  ghcr.io/progamesigner/muninn:latest
```

**Reachable by hostname (Kubernetes, ingress).** The http transport applies
DNS-rebinding protection and, by default, only accepts the loopback hosts
`localhost`, `127.0.0.1`, and `::1` in the inbound `Host` header. When clients
reach the server through a Kubernetes Service DNS name or an ingress hostname,
set `MUNINN_HTTP_ALLOWED_HOSTS` to those hostnames or every request is
rejected with `403`:

```sh
docker run --rm -p 8000:8000 \
  -e MUNINN_HTTP_BEARER=change-me \
  -e MUNINN_HTTP_ALLOWED_HOSTS=muninn.default.svc.cluster.local,muninn.example.com \
  -v "$PWD/vault:/vault" \
  ghcr.io/progamesigner/muninn:latest
```

A bare hostname matches any port; add `:port` to pin one. The single value `*`
disables `Host` validation entirely — only appropriate when an upstream proxy or
ingress already enforces `Host` trust.

**Health checks.** The image has no shell, so it cannot carry a Docker
`HEALTHCHECK`. Probe the HTTP routes from your orchestrator instead. There are
two: `GET /healthz` (liveness — succeeds as soon as the process is up, never
gated on the recall index) and `GET /readyz` (readiness — `503` until the recall
index has finished its eager build, `200` thereafter, or immediately when recall
is disabled). Both are reachable without a bearer token.

```yaml
# Kubernetes — the kubelet probes over HTTP, nothing runs inside the container.
livenessProbe:
  httpGet: { path: /healthz, port: 8000 }
readinessProbe:
  httpGet: { path: /readyz, port: 8000 }
# A long cold build over a large vault fits a startupProbe with a high
# failureThreshold, after which liveness/readiness take over. To make restarts
# cheap instead of merely survivable, persist the index — see
# "Persistent recall index".
startupProbe:
  httpGet: { path: /readyz, port: 8000 }
  failureThreshold: 60
  periodSeconds: 5
```

A Docker Compose `healthcheck:` runs its command *inside* the container and so
cannot work against this image (no shell, no `wget`/`curl`). Probe `/healthz`
from outside instead — the orchestrator, a sidecar, or an external monitor.

Every published image carries the full set of dynamic OCI labels
(`org.opencontainers.image.{created,revision,version,title,description,source,url,authors,documentation,vendor}`):

```sh
docker inspect ghcr.io/progamesigner/muninn:latest -f '{{json .Config.Labels}}'
```

## Quick start

```sh
# HTTP transport (default), loopback, no auth — ideal for local development.
MUNINN_ROOT_DIR=/path/to/vault muninn

# Inspect the effective configuration without starting the server.
MUNINN_ROOT_DIR=/path/to/vault muninn --print-config
```

## Configuration

Everything is configured through environment variables. Every CLI flag mirrors —
and overrides — the matching variable (`--root-dir`, `--policy`, `--http-bind`, …).
`MUNINN_ROOT_DIR` is the only required variable; all others have defaults.

| Variable | Default | Description |
|---|---|---|
| `MUNINN_ROOT_DIR` | *(required)* | Absolute path to the vault root. Must exist, be a directory, and be canonicalisable. |
| `MUNINN_AGENTS_DIR` | `Agents` | Agents folder relative to the root. `.` or empty means the vault root itself is the agents folder. Must be relative with no traversal. |
| `MUNINN_VFS_SCHEME` | `<agent>.<user>` | VFS suffix scheme. Each `<ident>` becomes a required scope parameter on every tool call. Empty string disables suffixing. |
| `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE` | `<root>/AGENT_SESSION_CONTEXT.md` | Path to the global full session-context template (see [Session context](#session-context)). Relative paths resolve against the vault root. Need not exist — falls back to the compiled-in default. |
| `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE` | `<root>/AGENT_SESSION_BOOTSTRAP.md` | Path to the global lean session-bootstrap template (see [Session context](#session-context)). Relative paths resolve against the vault root. Need not exist — falls back to the compiled-in default. |
| `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE` | `<root>/AGENT_MEMORY_LAYOUT.md` | Path to the global memory-layout template (see [Session context](#session-context)). Relative paths resolve against the vault root. Need not exist — falls back to the compiled-in default. |
| `MUNINN_POLICY` | `namespaced` | One of `scoped`, `namespaced`, `readonly`, `readwrite` (see [Policies](#policies)). |
| `MUNINN_TRANSPORT` | `http` | `http` or `stdio`. |
| `MUNINN_HTTP_BIND` | `127.0.0.1:8000` | HTTP bind address (http transport only). |
| `MUNINN_HTTP_BEARER` | *(unset)* | If set, `POST/GET /mcp` requires `Authorization: Bearer <token>`. The static bearer carries the all-scopes grant. When both this and `MUNINN_HTTP_TOKENS_FILE` are unset → unauthenticated (a startup `WARN` is logged). |
| `MUNINN_HTTP_TOKENS_FILE` | *(unset)* | Path to a JSON file mapping bearer tokens to per-scope grants: `{ "tokens": [ { "token": "…", "scopes": { "agent": "jarvis", "user": "*" } } ] }`. Grant keys must be the active scheme's placeholders; values are exact strings or `*`; duplicate tokens union their grants. Requests presenting a scoped token may only name scopes its grant covers (`scope_denied` / HTTP `403` on `/v1/context` otherwise); unknown bearers get `401`. Read once at startup (rotation requires a restart); invalid files fail startup; token values never appear in logs or `--print-config`. Ignored under the `stdio` transport. |
| `MUNINN_HTTP_ALLOWED_HOSTS` | *(unset)* | Comma-separated `Host` allow-list for the http transport's DNS-rebinding protection. Unset → loopback only (`localhost`, `127.0.0.1`, `::1`). List the cluster/ingress hostnames clients use (e.g. `muninn.default.svc.cluster.local,muninn.example.com:8000`); a bare hostname matches any port. The single value `*` disables `Host` validation (logs a `WARN`) — only for deployments that terminate `Host` trust at an upstream proxy. |
| `MUNINN_TIMEZONE` | `UTC` | IANA timezone used to date diary entries. |
| `MUNINN_HONOR_IGNORE_FILES` | `true` | Honour `.ignore` / `.gitignore` / `.obsidianignore` (nested, composed per-directory like `git`) for list and direct addressing. Strict boolean (`true`/`false`). |
| `MUNINN_INCLUDE_HIDDEN` | `false` | Include dotfiles/dot-directories. Strict boolean. |
| `MUNINN_INCLUDE_HIDDEN_GLOBS` | *(empty)* | Comma-separated gitignore-style globs (relative to the vault root) whose matches — and their whole subtree — are exempt from hidden filtering while other dotfiles stay excluded. E.g. `.obsidian/**,**/.config`. Ignore-file rules still apply unless also disabled. |
| `MUNINN_LOG` | `warn,muninn=info` | `tracing` env-filter directive. Logs always go to stderr. |
| `MUNINN_RECALL_BACKEND` | `simple` | Content-recall backend: `simple` (case-insensitive substring + regex), `tantivy` (BM25 ranking + frontmatter property filters; requires the `recall-tantivy` build feature, else falls back to `simple`), or `off` (no `recall_memory_notes` tool). |
| `MUNINN_RECALL_WATCH_DEBOUNCE_MS` | `500` | Filesystem-watcher debounce window for external edits. |
| `MUNINN_RECALL_REGEX_SCAN_BYTES` | `67108864` | Byte cap on a regex-only recall scan before it truncates (the result is flagged). |
| `MUNINN_RECALL_MAX_RESIDENT_SCOPES` | `256` | Cap on resident per-scope indexes before idle ones are evicted (they rebuild on next use). |
| `MUNINN_RECALL_FRESHNESS_MS` | `2000` | How long an index stays fresh before a query re-runs the stat-diff reconcile (the watcher can mark it dirty sooner). |
| `MUNINN_RECALL_INDEX_DIR` | *(unset)* | Directory for persistent on-disk recall indexes, so a restart re-reads only what changed while the server was down instead of rebuilding from the whole vault (see [Persistent recall index](#persistent-recall-index)). Unset — the default — keeps every index in memory and writes no index data to disk. Must be an absolute path **outside** the vault root (a path inside it fails startup: the vault watcher would see every index write). `tantivy` backend only; under `simple` it is ignored with a startup `WARN`. One writer per directory. |

### VFS scheme

The scheme defines how scope keys are spelled into directory segments and
filename stems. With the default `<agent>.<user>` scheme and caller
`{agent: "jarvis", user: "tony"}`, the virtual path `Agents/tasks/plan.md`
resolves to:

```
<root>/Agents/jarvis.tony/tasks/plan.jarvis.tony.md
```

The scope appears both as the per-scope directory and as a suffix on the file
stem, so a human opening the vault in any editor can immediately see whose file is
whose, and another scope's file is structurally unaddressable.

### Worked layouts

**Default config** (`MUNINN_AGENTS_DIR=Agents`, `MUNINN_VFS_SCHEME=<agent>.<user>`):

```
vault/
├── Agents/                       ← agent-owned region (scoped, suffixed)
│   └── jarvis.tony/
│       ├── PERSONA.jarvis.tony.md
│       ├── MEMORY.jarvis.tony.md
│       ├── HEARTBEAT.jarvis.tony.md
│       └── diary/2026-05-25.jarvis.tony.md
└── Actions/release.md            ← human-owned region (shared, no suffix)
```

**Vault root as agents folder** (`MUNINN_AGENTS_DIR=.`): the whole vault is the
agents folder, there is no "outside" region, and wrapper tools resolve to
`PERSONA.jarvis.tony.md` at the vault root.

## Tools

| Tool | Purpose |
|---|---|
| `list_memory_notes` | Paginated listing of virtual paths visible to the scope (`limit` default 200, max 1000; opaque `cursor`; optional `path_prefix`). |
| `read_memory_note` | Read a note by virtual path. |
| `read_memory_notes` | Read 1–20 notes in one call. Returns one `notes` entry per requested path, in request order: `{ path, content }` on success or `{ path, error: { code, message } }` on failure. Per-path semantics match `read_memory_note`; a per-path failure never fails the call (only malformed arguments do). |
| `write_memory_note` | Atomic full-file write; returns the byte count. With `append: true`, `content` is appended verbatim instead (exact bytes, no separator; a missing note is created), serialised under the per-target lock; the byte count is the note's total size after the write. |
| `edit_memory_note` | Atomic search/replace; the search string must occur exactly once. |
| `delete_memory_note` | Delete a single file (never directories). |
| `rename_memory_note` | Move a note from `path` to `new_path`, rewriting every visible incoming link (aliases, headings, and embeds preserved) and the note's own self-references to resolve to the new location. The destination must not already exist (`destination_exists`); all preconditions are validated before the first write. |
| `read_note_properties` | Read a note's frontmatter properties as a JSON object in `{ properties }`. Absent, unterminated, or malformed frontmatter yields an empty object — never an error. Read gating matches `read_memory_note`; root core files are readable. |
| `update_note_properties` | Merge a JSON object into a note's frontmatter atomically under the per-target lock: each key is upserted with its JSON value, an explicit `null` deletes a key, and the body stays byte-identical. The block is created when absent, removed when the merge empties it, and re-serialized in normalized form (sorted keys; comments not preserved); a leading fence that is not valid YAML is refused with `invalid_argument`. The note must exist; write gating matches the generic write tools (root core files stay wrapper-only). Returns the full post-update `{ properties }`. |
| `load_session_context` | Render the scope's session-context (see [Session context](#session-context)); returns `{ rendered, missing }`. |
| `evolve_core_persona` | Atomic write to one of the five foundational files (`persona`/`prompt`/`rules`/`user`/`memory`), selected by `which`. Enforces line caps: `USER.md` ≤ 100, `MEMORY.md` ≤ 200. |
| `update_task_heartbeat` | Atomic write to `HEARTBEAT.md`. |
| `append_diary_entry` | Append a timestamped section to `diary/<YYYY-MM-DD>.md`; a newly created file opens with a `# <YYYY-MM-DD>` H1, and an optional `title` makes the heading `## <HH:MM:SS> — <title>`. |
| `recall_memory_notes` | Search notes by content within the caller's visible set; returns ranked `{ path, score (0–1), snippets, modified_at }`. Supply at least one of `query` (full-text), `regex`, `filters` (frontmatter properties), or the `modified_after`/`modified_before` time bounds. Paginated like `list_memory_notes`. Present unless `MUNINN_RECALL_BACKEND=off`. |

Every tool's input schema includes the scope parameters derived from the active
scheme; introspect them via the standard MCP `tools/list` call.

### Recall

`recall_memory_notes` finds notes by content so an agent need not list-and-read to
locate them. It is backed by an **in-memory** index by default, the way Obsidian
works — nothing is written to disk; the index is built eagerly at startup, updated
synchronously on the server's own writes, and reconciled against external edits by
a filesystem watcher with a stat-diff backstop. The index is disposable: delete
the process and it rebuilds from the vault. On a large vault that rebuild can be
expensive enough to matter at restart, which is what
[`MUNINN_RECALL_INDEX_DIR`](#persistent-recall-index) addresses.

Isolation is **structural**: each scope has its own index plus one shared-region
index, so a query opens only the caller's index and (policy permitting) the shared
one — another scope's content is unreachable, not merely filtered.

Two backends, selected by `MUNINN_RECALL_BACKEND`:

- **`simple`** (default) — case-insensitive substring `query` and `regex`. No
  ranking beyond match count; frontmatter property `filters` return `unsupported`.
- **`tantivy`** — BM25-ranked full-text, snippet generation, and frontmatter
  property `filters` (`eq`, `contains`, `exists`, numeric/lexical comparisons).
  Requires building with the feature; otherwise the server logs a fallback to
  `simple`:

  ```sh
  cargo build --release --features recall-tantivy
  ```

Cross-index scores are normalized to 0–1 per index before merging. On a large
vault the eager cold build is gated by `GET /readyz` (see [Container image](#container-image)); a regex-only query is byte-capped and flags truncation in its result.

Every hit carries `modified_at` (RFC 3339, UTC), and the optional
`modified_after`/`modified_before` arguments bound results by modification time.
Each accepts an RFC 3339 timestamp or a bare `YYYY-MM-DD` date interpreted as
start of day in `MUNINN_TIMEZONE`, forming a half-open interval
(`modified_after ≤ mtime < modified_before`). A time bound is a sufficient
predicate on its own: with no `query`/`regex`/`filters`, hits come straight from
the in-memory manifest with no content scan, ordered by `modified_at` descending
with `score: 1.0` and empty snippets — "what did I work on recently?" costs no
reads. Combined with a content predicate, the bounds filter the hits and score
ordering stands. The compared time is the filesystem mtime — restores and sync
tools may carry old timestamps — and the behavior is identical on both backends.

Inside the agents folder the root level is **wrapper-only**: the core files
(`PERSONA.md`, `PROMPT.md`, `RULES.md`, `USER.md`, `MEMORY.md`, `HEARTBEAT.md`)
are changed only through `evolve_core_persona` / `update_task_heartbeat`.
`write_memory_note` / `edit_memory_note` / `delete_memory_note` may only target
paths under a subfolder; a root-level target is rejected with `path_not_permitted`.
Reads of root files remain allowed.

#### Persistent recall index

Set `MUNINN_RECALL_INDEX_DIR` (tantivy backend only) to keep the indexes on disk
across restarts. Each region's index is memory-mapped under that directory, and
every document additionally stores its source file's mtime and size — so opening a
persisted index also recovers the reconcile manifest, and startup becomes "open the
index, stat-walk the vault, re-index only what changed while the server was down"
instead of reading and tokenizing everything. Recall results are identical either
way; only the time to `GET /readyz` differs.

Unset is the default and behaves exactly as before: memory only, nothing written to
disk. Rolling back is just unsetting the variable — nothing on disk is needed to
start in memory-only mode, and a leftover index directory is inert.

The layout is `<index_dir>/<fingerprint>/<region>/`, where the fingerprint covers
the index format version plus the configuration that shapes what gets ingested
(`MUNINN_VFS_SCHEME`, `MUNINN_AGENTS_DIR`, and the visibility settings). Change any
of those, or upgrade to a server with a newer index format, and the old fingerprint
directory is deleted and the index rebuilt — a persisted index is never read under
a configuration it was not built for. The same fallback covers damage: an index
that cannot be opened, or whose schema does not match, is wiped and rebuilt with a
`WARN`. The process does not exit, and it never serves results from a foreign or
half-read index.

Operational constraints:

- **One writer per directory.** tantivy takes a writer lock per index, so two
  server processes must not share one index directory. Give each replica its own.
- **Absolute, and outside the vault root.** The vault watcher watches the root
  recursively; index writes inside it would mark the engine dirty on every commit.
  A path inside the vault fails startup with a message naming both variables.
- **Disk sizing.** Budget roughly the size of the vault's indexable text, plus
  headroom for segment merges — 2–3× the text size is a safe starting point for a
  volume. Stale fingerprint directories are pruned at startup, so disk use does not
  accumulate across upgrades or configuration changes.

In Kubernetes an `emptyDir` is usually the right volume: it survives the container
restarts a liveness kill causes (the case where a slow cold build hurts most) and
is discarded when the pod is rescheduled, which pays one full rebuild — correct by
construction, since a fresh pod may land on a different node.

```yaml
volumes:
  - name: recall-index
    emptyDir:
      sizeLimit: 2Gi
containers:
  - name: muninn
    env:
      - { name: MUNINN_RECALL_BACKEND, value: tantivy }
      - { name: MUNINN_RECALL_INDEX_DIR, value: /var/lib/muninn/index }
    volumeMounts:
      - { name: recall-index, mountPath: /var/lib/muninn/index }
```

Watch the `recall index ready` log line's `elapsed` field to see the payoff: the
second start over an unchanged vault should reach ready in a fraction of the cold
build's time.

### Cross-note links

Notes may reference each other with Obsidian `[[wikilink]]` syntax and relative
markdown links `[text](path.md)`. Write the **shortest unambiguous note name**
(`[[rust]]`, or `[[topics/rust]]` when a bare basename is shared by two visible
notes) — the server resolves it against your visible set (your own scope plus the
shared region) exactly as Obsidian resolves by basename.

Links round-trip transparently. On read you only ever see clean shortest names; on
write a link to your own scoped note is rewritten to its on-disk suffixed form so a
human browsing the vault in Obsidian can follow it, and a link to a shared note is
left clean. Aliases (`[[t|alias]]`), headings (`[[t#h]]`), and embeds (`![[t]]`)
are preserved. A link that does not resolve is left verbatim as a dangling link.

Because a suffixed link would expose your scope's existence, a note in the **shared
region** that links to one of **your own scoped** notes is rejected with
`write_denied`; link shared notes from shared notes, or scoped notes from scoped
notes.

## Session context

A **session-context template** weaves the five foundational files
(`PERSONA`/`PROMPT`/`RULES`/`USER`/`MEMORY`) together with operator prose into a
single rendered bootstrap. It is an ordinary markdown document with `{{…}}`
placeholders:

- `{{files.persona}}`, `{{files.prompt}}`, `{{files.rules}}`, `{{files.user}}`, `{{files.memory}}` — the foundational file contents (a missing file renders a sentinel)
- `{{scope.<key>}}` — a scope value (e.g. `{{scope.agent}}`); `<key>` is any scheme placeholder
- `{{scope_directive}}` — the server-generated banner naming the active scope keys
- `{{onboarding_directive}}` — empty unless foundational files are missing, then a directive to interview the user and commit them via `evolve_core_persona`

The renderer produces two kinds, each with its own template family:

- **Full context** (`AGENT_SESSION_CONTEXT.md`): orders the sections `PERSONA →
  RULES → MEMORY → USER → PROMPT`, then the onboarding directive and a pointer to
  the layout. For the model to pull its full operating context.
- **Lean bootstrap** (`AGENT_SESSION_BOOTSTRAP.md`): scope banner + `PERSONA` +
  `RULES` + onboarding directive + pointers to the full context and the layout —
  small enough to fit a SessionStart byte budget.

The **layout** (vault structure, the core-files-vs-filesystem distinction, the
path-addressing rule, and the documented line caps) lives in its own
`AGENT_MEMORY_LAYOUT.md`, served on demand rather than embedded in every render.
External-tool facts (camera, SSH, etc.) belong in `PROMPT.md`. Operators who
supply their own templates fully control that prose.

Each template is resolved per request, first hit wins (the bootstrap and layout
follow the same layering with their own filenames and env vars):

1. a per-scope file inside the agents folder (suffix-resolved like any scoped file)
2. the global file at `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE` / `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE` / `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE`
3. a compiled-in default

Nothing errors on absence — a fresh vault renders an instructions-only bootstrap.
Unknown `{{…}}` tokens are left literal. The renders are exposed through MCP
resources/prompt/tool and plain-HTTP endpoints:

| Surface | Shape | For |
|---|---|---|
| `load_session_context` tool | `{ rendered, missing }` (full context) | the model pulling its own context mid-session |
| `session-context` resource | `muninn://session-context/{…}` (full context) | client auto-attach |
| `session-bootstrap` resource | `muninn://session-bootstrap/{…}` (lean) | session-start injection |
| `session-layout` resource | `muninn://session-layout/{…}` (layout) | on-demand vault mechanics |
| `session-context` prompt | required args follow the scheme | user slash-command |
| `GET /v1/context` | `text/markdown` (or `{ rendered, missing }` JSON) | full context without MCP |
| `GET /v1/bootstrap` | `text/markdown` (or JSON) | lean session-start hook without MCP |
| `GET /v1/layout` | `text/markdown` (or JSON) | layout without MCP |

### `GET /v1/context`, `/v1/bootstrap`, `/v1/layout`

Versioned, stateless, read-only HTTP routes (HTTP transport only). `/v1/context`
renders the full context, `/v1/bootstrap` the lean bootstrap (the recommended
SessionStart hook target), and `/v1/layout` the vault-mechanics guidance. Each
VFS-scheme placeholder is a query parameter; the scope is bound in scheme order:

```sh
# Lean bootstrap — markdown by default, drop straight into a system prompt.
curl 'http://127.0.0.1:8000/v1/bootstrap?agent=jarvis&user=tony'

# Full context as JSON ({ rendered, missing }) via content negotiation.
curl -H 'Accept: application/json' \
  'http://127.0.0.1:8000/v1/context?agent=jarvis&user=tony'

# Vault layout and conventions.
curl 'http://127.0.0.1:8000/v1/layout?agent=jarvis&user=tony'
```

All three share one behavior: missing, empty, or unexpected scope parameters
return `400` with a `{ "error": … }` body; absent foundational files are never
errors. They sit behind the same authentication gate as `/mcp` (add
`-H "Authorization: Bearer <token>"` when `MUNINN_HTTP_BEARER` or
`MUNINN_HTTP_TOKENS_FILE` is configured); a scoped token requesting a scope
outside its grant gets `403`. Only the `/healthz` and `/readyz` probes are
always reachable.

To bootstrap this context automatically at the start of every session — via a
Claude Code / Codex `SessionStart` hook, an opencode plugin, or the MCP
fallback for other clients — see
[`docs/session-context-hooks.md`](docs/session-context-hooks.md).

## Policies

There are two regions per server: **inside** the agents folder (scoped,
suffix-applied) and **outside** it (shared, no suffix). One server-wide policy
governs both:

| Policy | Inside agents folder | Outside agents folder |
|---|---|---|
| `scoped` | own-scope read/write | **denied** |
| `namespaced` *(default)* | own-scope read/write | read-only |
| `readonly` | own-scope read-only | read-only |
| `readwrite` | own-scope read/write | read/write |

Anything resolving outside the vault root is always denied.

## Client configuration

Every MCP client connects over one of the two transports:

- **stdio sidecar** — the client *launches* `muninn` itself, so it owns the
  process lifecycle. You supply `MUNINN_ROOT_DIR` and `MUNINN_TRANSPORT=stdio`
  through the client's env block. The binary must be on `PATH` (or give an
  absolute path).
- **Local HTTP** — you run `muninn` yourself (HTTP is the default transport, on
  `127.0.0.1:8000`) and point the client at `http://127.0.0.1:8000/mcp`. Start the
  server *before* the client connects.
- **Docker sidecar** — a stdio sidecar that needs no local `muninn` binary: the
  client launches `docker run -i ... ghcr.io/progamesigner/muninn:latest`, which
  speaks stdio over the container's stdin/stdout. The image already sets
  `MUNINN_ROOT_DIR=/vault`, so mount your vault there.

The snippets below show all three per client. When `MUNINN_HTTP_BEARER`
is set, attach `Authorization: Bearer <token>` through whatever header mechanism
the client exposes (noted inline where relevant).

### Claude Desktop

`claude_desktop_config.json` (stdio only):

```json
{
  "mcpServers": {
    "muninn": {
      "command": "muninn",
      "env": {
        "MUNINN_ROOT_DIR": "/Users/me/vault",
        "MUNINN_TRANSPORT": "stdio"
      }
    },
    "muninn-docker": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "MUNINN_TRANSPORT=stdio",
        "-v", "/Users/me/vault:/vault",
        "ghcr.io/progamesigner/muninn:latest"
      ]
    }
  }
}
```

### Claude Code

Add it from the CLI — stdio (note the `--` before the server command):

```sh
claude mcp add muninn \
  --env MUNINN_ROOT_DIR=/path/to/vault --env MUNINN_TRANSPORT=stdio \
  -- muninn

# …or HTTP against an already-running server:
claude mcp add --transport http muninn http://127.0.0.1:8000/mcp
```

Or commit a project-scoped `.mcp.json` (`mcpServers` key; `command`/`env` for
stdio, `type`/`url` for HTTP):

```json
{
  "mcpServers": {
    "muninn-stdio": {
      "command": "muninn",
      "env": { "MUNINN_ROOT_DIR": "/path/to/vault", "MUNINN_TRANSPORT": "stdio" }
    },
    "muninn-http": {
      "type": "http",
      "url": "http://127.0.0.1:8000/mcp"
    },
    "muninn-docker": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "MUNINN_TRANSPORT=stdio",
        "-v", "/path/to/vault:/vault",
        "ghcr.io/progamesigner/muninn:latest"
      ]
    }
  }
}
```

### Codex CLI

`~/.codex/config.toml` — each server is a `[mcp_servers.<name>]` table. A `command`
key makes it a stdio server; a `url` key (no `command`) makes it streamable HTTP:

```toml
# stdio sidecar
[mcp_servers.muninn]
command = "muninn"
env = { MUNINN_ROOT_DIR = "/path/to/vault", MUNINN_TRANSPORT = "stdio" }

# …or HTTP against an already-running server (use one table or the other)
[mcp_servers.muninn]
url = "http://127.0.0.1:8000/mcp"
# bearer_token_env_var = "MUNINN_TOKEN"   # if MUNINN_HTTP_BEARER is set

# …or a Docker stdio sidecar (no local binary needed)
[mcp_servers.muninn-docker]
command = "docker"
args = ["run", "-i", "--rm", "-e", "MUNINN_TRANSPORT=stdio", "-v", "/path/to/vault:/vault", "ghcr.io/progamesigner/muninn:latest"]
```

`codex mcp add muninn -- muninn` is the stdio shortcut; HTTP servers are added
by editing `config.toml`.

### Antigravity CLI

Edit the MCP config at `~/.gemini/antigravity/mcp_config.json` (in the IDE: Agent
panel → **MCP Servers** → **Manage MCP Servers** → **View raw config**). The
top-level key is `mcpServers`, and HTTP uses **`serverUrl`** — *not* `url`:

```json
{
  "mcpServers": {
    "muninn-stdio": {
      "command": "muninn",
      "env": { "MUNINN_ROOT_DIR": "/path/to/vault", "MUNINN_TRANSPORT": "stdio" }
    },
    "muninn-http": {
      "serverUrl": "http://127.0.0.1:8000/mcp"
    },
    "muninn-docker": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "MUNINN_TRANSPORT=stdio",
        "-v", "/path/to/vault:/vault",
        "ghcr.io/progamesigner/muninn:latest"
      ]
    }
  }
}
```

### GitHub Copilot

Workspace `.vscode/mcp.json` — Copilot (agent mode) reads VS Code's MCP config.
The top-level key is **`servers`** (not `mcpServers`) and every entry carries an
explicit `type`. Optional `inputs` prompt for secrets:

```json
{
  "inputs": [
    { "type": "promptString", "id": "muninn-token", "description": "Muninn bearer", "password": true }
  ],
  "servers": {
    "muninn-stdio": {
      "type": "stdio",
      "command": "muninn",
      "env": { "MUNINN_ROOT_DIR": "${workspaceFolder}/vault", "MUNINN_TRANSPORT": "stdio" }
    },
    "muninn-http": {
      "type": "http",
      "url": "http://127.0.0.1:8000/mcp",
      "headers": { "Authorization": "Bearer ${input:muninn-token}" }
    },
    "muninn-docker": {
      "type": "stdio",
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "MUNINN_TRANSPORT=stdio",
        "-v", "${workspaceFolder}/vault:/vault",
        "ghcr.io/progamesigner/muninn:latest"
      ]
    }
  }
}
```

Drop the `inputs` block and the `headers` line when no bearer is configured.

### VS Code

VS Code's native MCP support uses the **same `.vscode/mcp.json` format** shown for
Copilot above (the `servers` key, `type: stdio` / `type: http`). The only
difference is *where* it can live: per workspace in `.vscode/mcp.json`, or for all
workspaces in your user profile — open the latter via the command palette
(**MCP: Open User Configuration**) rather than editing it by hand.

### OpenCode

`opencode.json` — servers live under the `mcp` key. OpenCode names the transports
`local`/`remote`, takes `command` as an **array**, and uses `environment` (not
`env`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "muninn-local": {
      "type": "local",
      "command": ["muninn"],
      "enabled": true,
      "environment": { "MUNINN_ROOT_DIR": "/path/to/vault", "MUNINN_TRANSPORT": "stdio" }
    },
    "muninn-remote": {
      "type": "remote",
      "url": "http://127.0.0.1:8000/mcp",
      "enabled": true
    },
    "muninn-docker": {
      "type": "local",
      "command": ["docker", "run", "-i", "--rm", "-e", "MUNINN_TRANSPORT=stdio", "-v", "/path/to/vault:/vault", "ghcr.io/progamesigner/muninn:latest"],
      "enabled": true
    }
  }
}
```

For a bearer-protected remote, add `"headers": { "Authorization": "Bearer …" }`.

### Generic MCP client

Most clients accept the canonical `mcpServers` block — `command`/`env` for a
stdio sidecar, `url` for Local HTTP:

```json
{
  "mcpServers": {
    "muninn-stdio": {
      "command": "muninn",
      "env": { "MUNINN_ROOT_DIR": "/path/to/vault", "MUNINN_TRANSPORT": "stdio" }
    },
    "muninn-http": {
      "url": "http://127.0.0.1:8000/mcp"
    },
    "muninn-docker": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "MUNINN_TRANSPORT=stdio",
        "-v", "/path/to/vault:/vault",
        "ghcr.io/progamesigner/muninn:latest"
      ]
    }
  }
}
```

### curl (raw HTTP transport)

For poking the HTTP transport directly, without an MCP client:

```sh
# Liveness and readiness probes.
curl http://127.0.0.1:8000/healthz
curl http://127.0.0.1:8000/readyz

# An MCP request (Streamable HTTP requires the dual Accept header).
curl -X POST http://127.0.0.1:8000/mcp \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

When `MUNINN_HTTP_BEARER` is set, add `-H "Authorization: Bearer <token>"`.

## Human-in-the-loop editing

Because the on-disk layout is plain markdown, a human can open the vault in any
editor — Obsidian is a convenient choice — and hand-edit any `Agents/<scope>/...`
file directly. The agent will see
the human's edits as if it had written them itself — this is the supported channel
for curating or correcting an agent's memory. Creating `plan.jarvis.tony.md` by
hand makes it appear to the `jarvis.tony` scope as the virtual note `plan.md`.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
