# Security & trust model

This document describes what `muninn` guarantees, what it deliberately does not
guarantee in v1, and how to widen or narrow its visibility.

## Threat model at a glance

| Concern | Stance |
|---|---|
| Claimed scope keys (`agent`, `user`, …) | **Bound to the bearer when `MUNINN_HTTP_TOKENS_FILE` is set** (HTTP transport): each token may only name the scopes its grant covers. Without a tokens file, scope keys are trusted claims, as in v1. |
| Path traversal (`..`, absolute paths, symlink escape) | **Prevented.** Structurally impossible to escape the vault root. |
| Cross-scope access inside the agents folder | **Structurally impossible.** The resolver always appends the caller's own scope. |
| Cross-scope leakage via `[[wikilinks]]` in shared notes | **Prevented.** A shared note linking to the caller's own scoped note is refused with `write_denied`; only the owning scope's suffix is ever persisted, and only in files that scope alone can read. |
| Writes into human-owned regions | **Governed by policy.** Denied unless the active policy permits it. |
| HTTP endpoint exposure | **Optional static bearer token and/or per-tenant scoped tokens.** Loopback-only by default. |

## Scope keys: trusted by default, bound to the bearer with scoped tokens

Scope keys are supplied **per tool call**, not per session. By default they are
trusted claims: a single server process serves many agents concurrently with no
session handshake, and any client that clears the endpoint gate may name any
scope. That is fine for a loopback sidecar whose clients are all launched by the
same operator — and dishonest for a shared server reachable over the network.

For the shared deployment, the HTTP transport supports **per-tenant scoped
tokens** via `MUNINN_HTTP_TOKENS_FILE`, a JSON file mapping bearer tokens to
scope grants:

```json
{
  "tokens": [
    { "token": "jarvis-secret", "scopes": { "agent": "jarvis", "user": "*" } },
    { "token": "ops-secret",    "scopes": { "agent": "friday", "user": "tony" } }
  ]
}
```

- Grant keys MUST be exactly the active scheme's placeholders; each value is an
  exact string or the total wildcard `*` (partial patterns like `"t*"` are
  rejected at startup). A token listed in several entries gets the union of its
  entries — a request is permitted when at least one entry matches every key.
- **Authentication** happens in the transport middleware: when the tokens file
  is configured, every request to `/mcp` and `/v1/context` must present either a
  configured scoped token or the static `MUNINN_HTTP_BEARER` (which keeps its
  semantics as the operator token and carries the all-scopes grant); anything
  else is `401`. The probe routes stay open.
- **Authorization** happens at scope validation, where every scoped surface
  already funnels: a `tools/call`, the `session-context` resource and prompt,
  and `GET /v1/context` each check the requested scope keys against the
  presenting token's grant. A mismatch is the `scope_denied` domain error
  (HTTP `403` on `/v1/context`), rejected **before any path resolution or IO**.
  The message names the offending key and never enumerates valid grants.
- The file is read once at startup; an unreadable or invalid file is a startup
  error, not a silently open server, and token values never appear in logs or
  `--print-config` output. **Rotation requires a restart** — grants are
  resolved per request from the startup table, so a token dropped from the file
  stops authorizing on the next restart, even on already-open sessions.
- Standard secret-mount practice applies: keep the file readable only by the
  server's user (e.g. a Kubernetes `Secret` volume).

Without a tokens file, `MUNINN_HTTP_BEARER` alone still protects only the
**endpoint**, not individual tenants — a coarse gate, not per-scope
authorization. The stdio transport is unchanged either way: the launching
process owns the vault, so process-level trust applies by design.

> **Operational guidance:** for shared or remote deployments, configure
> `MUNINN_HTTP_TOKENS_FILE` so every client is confined to its own scopes, and
> reserve `MUNINN_HTTP_BEARER` for operator tooling. Expose an
> unauthenticated or static-bearer-only server just to clients you trust to
> honestly declare their own scope.

The HTTP transport also enforces **DNS-rebinding protection** on the inbound
`Host` header. By default only loopback hosts (`localhost`, `127.0.0.1`, `::1`)
are accepted; off-host clients addressing the server by a Kubernetes Service DNS
name or ingress hostname must be allow-listed via `MUNINN_HTTP_ALLOWED_HOSTS`,
or their requests are rejected with `403`. Keep the list as tight as the
deployment allows; reserve the `*` opt-out for cases where an upstream proxy or
ingress already validates `Host`.

## Traversal is not trusted

Every client-supplied virtual path is validated and normalised:

- empty paths, embedded NUL bytes, absolute paths, and any `..` component are
  rejected before resolution;
- after resolution, the deepest existing ancestor of the target is canonicalised
  and must lie within the canonical vault root — this catches `..` survivors and
  **symlink escapes** (a symlink inside the vault pointing outside is refused);
- anything resolving outside the vault root is always denied, regardless of policy.

## Own-scope strictness is structural

Inside the agents folder, with a non-empty scheme, the resolver appends the
caller's rendered scope as both the first directory segment and the file-stem
suffix on **every** read, write, edit, and delete. A request for `PERSONA.md` from
`{agent: jarvis, user: tony}` can only ever resolve to
`Agents/jarvis.tony/PERSONA.jarvis.tony.md`. There is no virtual path — legitimate
or crafted — that addresses another scope's file: a crafted path such as
`Agents/PERSONA.jarvis.sam.md` still resolves under `jarvis.tony` and lands at
`Agents/jarvis.tony/PERSONA.jarvis.sam.jarvis.tony.md`, never sam's file. Listings
likewise filter by the caller's own suffix, so other scopes' files are invisible.

An empty scheme (`MUNINN_VFS_SCHEME=`) disables suffixing: the agents folder
degenerates into a plain shared directory governed by the policy's outside-folder
rules, with no own-scope isolation.

## Cross-note links never leak a scope

`[[wikilink]]` and relative markdown link targets in note content are rewritten on
the same own-scope boundary as filenames. On read, the caller's own suffix is
stripped from every target, so an agent only ever sees clean shortest names and
never another scope's suffix. On write, a link to the caller's own scoped note is
rewritten to its suffixed physical form — but that form is only ever stored inside
the caller's own scope, which no other scope can read.

The one direction that could leak is a **shared** note (readable by every scope)
linking to the caller's **own scoped** note: persisting the suffixed target would
expose the scope's existence to every reader. This is refused before any bytes are
written, with code `write_denied` and a message naming the offending target. With
an empty scheme there are no scopes and the transform is a no-op.

## The four policies and their guarantees outside the agents folder

There are exactly two regions: **inside** the agents folder and **outside** it but
still within the vault root. One server-wide policy (`MUNINN_POLICY`) governs
both:

| Policy | Inside agents folder | Outside agents folder |
|---|---|---|
| `scoped` | own-scope read/write | **denied** (`path_not_permitted`) |
| `namespaced` *(default)* | own-scope read/write | read-only (writes → `write_denied`) |
| `readonly` | own-scope read-only | read-only (writes → `write_denied`) |
| `readwrite` | own-scope read/write | read/write |

The distinction between the two refusal codes is deliberate: a region that is
entirely unreachable (`scoped` outside) reports `path_not_permitted` and does not
confirm whether a file exists; a region that is readable but not writable reports
`write_denied`. When the agents folder is the vault root (`MUNINN_AGENTS_DIR=.`),
the "outside" region is empty and that column has no effect.

## Visibility filters

To avoid surfacing editor/VCS noise and to keep agents from trampling tool state,
two filters apply to listing **and** to direct read/write/edit/delete:

- **Hidden filter** — any path segment beginning with `.` is excluded by default.
  The configured agents folder is exempt even if it begins with `.` (e.g.
  `MUNINN_AGENTS_DIR=.agents` stays traversable). Toggle off with
  `MUNINN_INCLUDE_HIDDEN=true`.
- **Ignore-file filter** — `.gitignore` and `.obsidianignore` patterns are honoured
  hierarchically (the same machinery `ripgrep` uses). Toggle off with
  `MUNINN_HONOR_IGNORE_FILES=false`.

Direct addressing of an excluded path returns `path_not_permitted` — **not**
`not_found` — so the filter does not leak whether the file actually exists.

### Widening visibility

Set `MUNINN_INCLUDE_HIDDEN=true` and/or `MUNINN_HONOR_IGNORE_FILES=false` when
an operator genuinely wants the agent to see hidden or ignored files. Both default
to the conservative setting.

## Recall index isolation

Content recall (`recall_memory_notes`) is backed by **per-scope in-memory indexes
plus one shared-region index** — never a single combined index. A scope's notes
live only in that scope's index, and a query opens **only** the caller's own-scope
index plus (when the policy permits reading the shared region) the shared index.
Cross-scope recall is therefore **structurally impossible**, not merely filtered:
there is no index that holds two scopes' content, so no filter bug can leak one
scope's notes — paths, snippets, or scores — into another's results.

Indexes are seeded from the same visibility walk as `list_memory_notes`, so hidden
and `.gitignore`/`.obsidianignore`-excluded notes never enter any index and can
never surface as a recall hit. The index is held entirely in memory; nothing is
written to disk, so it never appears in the vault, in git, or in a listing.

Snippets pass through the same own-scope suffix strip as the read path, so a
returned fragment never exposes another scope's filename suffix.

## Error hygiene

Internal errors are mapped at the MCP boundary into a human-readable message plus
a structured `code`. Messages reference the **virtual** path the client supplied,
never the resolved physical path, and raw OS error strings are never propagated —
IO failures carry only an error kind and a static context label.

## Deferred to follow-up changes

- **Hot reload of the tokens file** (rotation currently requires a restart).
- **CORS / auth presets** for non-loopback HTTP deployments.
