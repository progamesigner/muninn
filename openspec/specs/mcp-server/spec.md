# mcp-server Specification

## Purpose
TBD - created by archiving change build-agentmem-mcp-server. Update Purpose after archive.
## Requirements
### Requirement: Server binary lifecycle
The system SHALL ship a single Rust binary `muninn` that, on launch, reads configuration from the environment, initialises logging to the correct sink for the selected transport, registers all tools with the `rmcp` server, and begins serving requests until terminated by a signal.

#### Scenario: Successful stdio startup
- **WHEN** `muninn` is launched with `MUNINN_TRANSPORT=stdio` and a valid `MUNINN_ROOT_DIR`
- **THEN** the process reads JSON-RPC frames from stdin, writes JSON-RPC responses to stdout, writes all logs and diagnostics to stderr, and continues running until stdin is closed or it receives `SIGTERM`/`SIGINT`

#### Scenario: Successful http startup (default)
- **WHEN** `muninn` is launched with `MUNINN_TRANSPORT` unset (defaults to `http`) and a valid `MUNINN_ROOT_DIR`
- **THEN** the process binds a TCP listener on `127.0.0.1:8000`, serves the MCP Streamable HTTP endpoint at `POST /mcp` in stateless JSON-response mode, a liveness route at `GET /healthz`, a readiness route at `GET /readyz`, and runs until receiving `SIGTERM`/`SIGINT`

#### Scenario: Misconfiguration fails fast
- **WHEN** `MUNINN_ROOT_DIR` is missing or invalid, or `MUNINN_VFS_SCHEME`/`MUNINN_POLICY`/`MUNINN_AGENTS_DIR` is set to an invalid value
- **THEN** the process writes a single human-readable line to stderr explaining which variable is wrong, exits with a non-zero status code, and does NOT begin accepting MCP requests

### Requirement: Transport selection
The system SHALL select its transport based on the `MUNINN_TRANSPORT` environment variable, accepting the values `stdio` and `http`, and SHALL default to `http` when the variable is unset.

#### Scenario: http is the default transport
- **WHEN** `MUNINN_TRANSPORT` is unset
- **THEN** the server uses the `rmcp` Streamable HTTP transport mounted under an `axum` router

#### Scenario: stdio is selectable
- **WHEN** `MUNINN_TRANSPORT` is set to `stdio`
- **THEN** the server uses the `rmcp` stdio transport and no TCP listener is opened

#### Scenario: http is selectable explicitly
- **WHEN** `MUNINN_TRANSPORT` is set to `http`
- **THEN** the server uses the `rmcp` Streamable HTTP transport and binds the listener address from `MUNINN_HTTP_BIND` (default `127.0.0.1:8000`)

#### Scenario: Unknown transport value
- **WHEN** `MUNINN_TRANSPORT` is set to any value other than `stdio` or `http`
- **THEN** the server exits with a non-zero status and writes a stderr message that names the accepted values

### Requirement: Stdio output discipline
The system SHALL guarantee that, when running under stdio transport, no byte that is not a valid JSON-RPC frame is ever written to stdout.

#### Scenario: Logs go to stderr under stdio
- **WHEN** the server is running under stdio and emits a log line at any level
- **THEN** the line is written to stderr and stdout receives only the bytes that constitute MCP JSON-RPC frames

#### Scenario: Panics do not corrupt stdout
- **WHEN** an internal panic occurs in a tool handler
- **THEN** the panic message is written to stderr, the JSON-RPC response sent on stdout is a well-formed MCP error, and the server continues serving subsequent requests

### Requirement: Tool registration and listing
The system SHALL register the following nine tools with the MCP server and advertise them through `tools/list`: `list_memory_notes`, `read_memory_note`, `write_memory_note`, `edit_memory_note`, `delete_memory_note`, `load_session_context`, `evolve_core_persona`, `update_task_heartbeat`, `append_diary_entry`.

#### Scenario: tools/list returns the full set
- **WHEN** an MCP client calls `tools/list` after the `initialize` handshake
- **THEN** the response contains exactly the nine tool entries above, each with a JSON Schema generated from its Rust input struct via `schemars` and merged with the scheme-derived scope fields

#### Scenario: Schema reflects the configured VFS scheme
- **WHEN** the server is started with `MUNINN_VFS_SCHEME=<agent>` and a client calls `tools/list`
- **THEN** the input schemas for every tool include a required string `agent` parameter and do NOT include a `user` parameter

#### Scenario: Schema includes custom scheme keys
- **WHEN** the server is started with `MUNINN_VFS_SCHEME=<team>.<agent>.<env>.<user>` and a client calls `tools/list`
- **THEN** the input schemas for every tool include required string parameters `team`, `agent`, `env`, `user` in that order

#### Scenario: Empty scheme removes scope fields from schemas
- **WHEN** the server is started with `MUNINN_VFS_SCHEME=` (empty) and a client calls `tools/list`
- **THEN** the input schemas for every tool include no scope parameters at all

### Requirement: Error reporting at the MCP boundary
The system SHALL map every internal error into an MCP tool result that contains a human-readable `text` message and a structured `code` discriminator. Raw OS error messages SHALL NOT be passed through verbatim.

#### Scenario: Policy violation returns a structured error
- **WHEN** a tool call is rejected because it tries to write to a `shared_readonly` path
- **THEN** the tool result is an MCP error whose text is of the form "write denied: path '...' is in a read-only region" and whose `code` field is `write_denied`

#### Scenario: Missing file
- **WHEN** `read_workspace_file` is called with a virtual path that resolves to a non-existent file
- **THEN** the tool result is an MCP error with code `not_found` and a message that includes the virtual path the client supplied (never the resolved physical path)

### Requirement: Blocking dispatch stays off the async runtime
The system SHALL execute the synchronous work behind MCP tool calls, resource reads, and prompt renders on threads outside the async runtime's worker pool, so that no single request — however long it blocks (filesystem I/O, index reconciliation, or waiting on the recall engine's lock) — can prevent the runtime from serving other futures. The `GET /healthz` liveness endpoint SHALL respond within its probe timeout while any number of tool calls are blocked, including for the whole duration of the eager recall index build.

#### Scenario: Liveness stays green during the eager index build
- **WHEN** the server starts against a large vault, the eager recall index build is still running, and a client issues a `recall_memory_notes` call that blocks waiting for the build to finish
- **THEN** `GET /healthz` continues to respond `200 OK` within the probe timeout for the entire build, and the blocked tool call completes (or times out at the client) without the process being restarted by its orchestrator

#### Scenario: A slow tool call does not starve concurrent requests
- **WHEN** one tool call is executing long-running blocking work (for example a stat-diff reconcile of a large scope) and a second, independent request arrives (another tool call, a resource read, or a health probe)
- **THEN** the second request is scheduled and answered without waiting for the first call's blocking work to yield the runtime

#### Scenario: Runtime keeps multiple workers under a CPU limit of 1
- **WHEN** the server runs in a container whose cgroup CPU quota resolves the detected parallelism to 1
- **THEN** the async runtime is still built with more than one worker thread, so a single stalled future cannot halt the scheduler

### Requirement: HTTP transport static authentication
The system SHALL, when running under `http` transport, optionally require an `Authorization: Bearer <token>` header matching `MUNINN_HTTP_BEARER`. The static bearer SHALL carry an all-scopes grant: requests presenting it may name any scope. When both `MUNINN_HTTP_BEARER` and `MUNINN_HTTP_TOKENS_FILE` are unset, no authentication is enforced and a startup warning is logged; when either is set, requests without an acceptable bearer SHALL be rejected.

#### Scenario: Bearer token accepted
- **WHEN** `MUNINN_HTTP_BEARER=secret` is set and a client sends a request to `POST /mcp` with header `Authorization: Bearer secret`
- **THEN** the request is processed normally

#### Scenario: Bearer token rejected
- **WHEN** `MUNINN_HTTP_BEARER=secret` is set and a client sends a request without the header or with the wrong token
- **THEN** the server responds with HTTP 401 and an MCP error body, and the request never reaches a tool handler

#### Scenario: Auth disabled emits warning
- **WHEN** the server starts in `http` mode with both `MUNINN_HTTP_BEARER` and `MUNINN_HTTP_TOKENS_FILE` unset
- **THEN** a single `WARN`-level log line is emitted indicating that the HTTP endpoint is unauthenticated

### Requirement: HTTP transport Host validation
The system SHALL, when running under `http` transport, configure the `rmcp` Streamable HTTP service's inbound `Host` validation from the resolved allowed-hosts list (`MUNINN_HTTP_ALLOWED_HOSTS` / `--http-allowed-hosts`). When the list is non-empty, requests whose `Host` header authority matches an entry SHALL be accepted and all others SHALL be rejected by the transport. When the list is unset, the transport SHALL retain its loopback-only default (`localhost`, `127.0.0.1`, `::1`). The single value `*` SHALL disable `Host` validation so requests with any `Host` header are accepted.

This makes the HTTP transport usable behind a Kubernetes Service or ingress, where clients address the server by a cluster DNS name or external hostname rather than a loopback address.

#### Scenario: Cluster hostname accepted when allow-listed
- **WHEN** the server runs under `http` transport with `MUNINN_HTTP_ALLOWED_HOSTS=muninn.svc.cluster.local` and a client sends `POST /mcp` carrying `Host: muninn.svc.cluster.local`
- **THEN** the transport accepts the request and processes the MCP call

#### Scenario: Non-listed host rejected
- **WHEN** the server runs under `http` transport with `MUNINN_HTTP_ALLOWED_HOSTS=muninn.example.com` and a client sends a request carrying `Host: evil.example.net`
- **THEN** the transport rejects the request

#### Scenario: Loopback default preserved when unset
- **WHEN** the server runs under `http` transport with `MUNINN_HTTP_ALLOWED_HOSTS` unset and a client on the same host sends `POST /mcp` carrying `Host: 127.0.0.1:8000`
- **THEN** the transport accepts the request, unchanged from prior behavior

#### Scenario: Validation disabled by wildcard
- **WHEN** the server runs under `http` transport with `MUNINN_HTTP_ALLOWED_HOSTS=*` and a client sends a request carrying any `Host` header
- **THEN** the transport accepts the request without `Host` validation

### Requirement: HTTP transport stateless JSON responses
The system SHALL, when running under `http` transport, configure the `rmcp` Streamable HTTP service in stateless mode with direct JSON responses (`stateful_mode = false`, `json_response = true`). Each `POST /mcp` request SHALL be handled independently and its JSON-RPC response returned with `Content-Type: application/json`, not `text/event-stream`. The server SHALL NOT issue an `mcp-session-id` header and SHALL NOT depend on a per-session SSE event stream for delivering responses or notifications, consistent with its advertised capabilities (no `listChanged`, no `subscribe`).

This matches the server's request→response semantics — every tool call resolves synchronously and the server never initiates messages — and avoids the SSE-on-POST response shape and `GET /mcp` resume churn that break clients which do not consume server-streamed responses.

#### Scenario: Tool call returns a JSON response
- **WHEN** a client completes the `initialize` handshake and sends a `tools/call` request to `POST /mcp` with `Accept: application/json, text/event-stream`
- **THEN** the server responds with `Content-Type: application/json` carrying the single JSON-RPC result, and the connection closes without an SSE event stream

#### Scenario: No session id is issued
- **WHEN** a client sends an `initialize` request to `POST /mcp`
- **THEN** the response does NOT include an `mcp-session-id` header and subsequent requests are accepted without one

### Requirement: Resources and prompts capability advertisement
The system SHALL advertise the resources and prompts capabilities during the MCP `initialize` handshake, in addition to tools, so that clients discover the `session-context`, `session-bootstrap`, and `session-layout` resources and the `session-context` prompt.

#### Scenario: Capabilities include resources and prompts
- **WHEN** an MCP client completes the `initialize` handshake
- **THEN** the server's advertised capabilities include both resources and prompts alongside tools

#### Scenario: Resource templates list all three session resources
- **WHEN** a client calls `resources/templates/list`
- **THEN** the listed resources include `session-context`, `session-bootstrap`, and `session-layout`, each at its scheme-parameterized templated URI

### Requirement: `session-context` resource
The system SHALL expose a `session-context` resource at the templated URI `muninn://session-context/{…}`, registered through `resources/templates/list`, whose URI parameters are derived, in order, from the configured scheme's placeholders. A `resources/read` of a concrete URI SHALL return the rendered session-context (produced by the shared renderer) as the resource contents for the scope encoded in the URI.

#### Scenario: Resource URI params follow the scheme
- **WHEN** the server is started with `MUNINN_VFS_SCHEME=<agent>.<user>` and a client calls `resources/templates/list`
- **THEN** the listed URI is `muninn://session-context/{agent}/{user}`

#### Scenario: Reading a resource renders the context
- **WHEN** a client calls `resources/read` for `muninn://session-context/jarvis/tony`
- **THEN** the response contents are the rendered session-context for scope `{agent: jarvis, user: tony}`

#### Scenario: Reading an empty-vault scope succeeds
- **WHEN** a client reads the session-context resource for a scope with no foundational files and no template
- **THEN** the read succeeds and returns the compiled-in default template with missing sentinels, never a not-found error

### Requirement: `session-context` prompt
The system SHALL expose a prompt named `session-context` through `prompts/list`, whose arguments are derived from the configured scheme's placeholders. A `prompts/get` SHALL return a message whose content is the rendered session-context (produced by the shared renderer) for the scope supplied in the arguments.

#### Scenario: Prompt arguments follow the scheme
- **WHEN** the server is started with `MUNINN_VFS_SCHEME=<agent>.<user>` and a client calls `prompts/list`
- **THEN** the `session-context` prompt declares required string arguments `agent` and `user`

#### Scenario: Getting the prompt renders the context
- **WHEN** a client calls `prompts/get` for `session-context` with `{agent: jarvis, user: tony}`
- **THEN** the returned message content is the rendered session-context for that scope

#### Scenario: Missing required argument is rejected
- **WHEN** a client calls `prompts/get` for `session-context` omitting a required scope argument
- **THEN** the server returns an error naming the missing argument and does not render

### Requirement: `session-bootstrap` resource
The system SHALL expose a `session-bootstrap` resource at the templated URI `muninn://session-bootstrap/{…}`, registered through `resources/templates/list`, whose URI parameters are derived, in order, from the configured scheme's placeholders. A `resources/read` of a concrete URI SHALL return the **lean bootstrap** render (the `bootstrap` render kind of the shared renderer) as the resource contents for the scope encoded in the URI.

#### Scenario: Bootstrap resource URI params follow the scheme
- **WHEN** the server is started with `MUNINN_VFS_SCHEME=<agent>.<user>` and a client calls `resources/templates/list`
- **THEN** the listed URI is `muninn://session-bootstrap/{agent}/{user}`

#### Scenario: Reading the bootstrap resource renders the lean bootstrap
- **WHEN** a client calls `resources/read` for `muninn://session-bootstrap/jarvis/tony`
- **THEN** the response contents are the lean `bootstrap` render for scope `{agent: jarvis, user: tony}`

#### Scenario: Reading an empty-vault scope succeeds
- **WHEN** a client reads the `session-bootstrap` resource for a scope with no foundational files and no template
- **THEN** the read succeeds and returns the compiled-in default bootstrap template with missing sentinels and the onboarding directive, never a not-found error

### Requirement: `session-layout` resource
The system SHALL expose a `session-layout` resource at the templated URI `muninn://session-layout/{…}`, registered through `resources/templates/list`, whose URI parameters are derived, in order, from the configured scheme's placeholders. A `resources/read` of a concrete URI SHALL return the **layout** render for the scope encoded in the URI.

#### Scenario: Layout resource URI params follow the scheme
- **WHEN** the server is started with `MUNINN_VFS_SCHEME=<agent>.<user>` and a client calls `resources/templates/list`
- **THEN** the listed URI is `muninn://session-layout/{agent}/{user}`

#### Scenario: Reading the layout resource renders the layout
- **WHEN** a client calls `resources/read` for `muninn://session-layout/jarvis/tony`
- **THEN** the response contents are the rendered layout for scope `{agent: jarvis, user: tony}`, identical to what `GET /v1/layout` returns for the same scope

#### Scenario: Reading the layout for an empty-vault scope succeeds
- **WHEN** a client reads the `session-layout` resource for a scope with no layout template configured
- **THEN** the read succeeds and returns the compiled-in default layout content, never a not-found error

### Requirement: Scoped-token gating covers the bootstrap and layout surfaces
The system SHALL, when running under `http` transport with `MUNINN_HTTP_TOKENS_FILE` configured, authorize the new scope-bearing surfaces against the presenting token's scope grants exactly as it does for the `session-context` resource and `GET /v1/context`: a `session-bootstrap` resource read, a `session-layout` resource read, `GET /v1/bootstrap`, and `GET /v1/layout` SHALL each be permitted only when every requested scope key matches the token's grant, and a mismatch SHALL be rejected with a `scope_denied`/`403` error naming the offending key before any path resolution or IO. Unauthenticated bearers SHALL be rejected with `401` as for the existing surfaces.

#### Scenario: Scoped token gates the bootstrap and layout surfaces
- **WHEN** a client presenting a token granted `agent=jarvis` only requests the `session-bootstrap` resource, the `session-layout` resource, `GET /v1/bootstrap`, or `GET /v1/layout` for `agent=friday`
- **THEN** the request is rejected with a `scope_denied` error (HTTP `403` for the HTTP routes) naming the offending key `agent`, before any rendering

#### Scenario: Scoped token renders its own scope on the new surfaces
- **WHEN** the same token requests any of those surfaces for `agent=jarvis&user=tony`
- **THEN** the request succeeds and returns the rendered content for that scope

### Requirement: HTTP per-tenant scoped tokens
The system SHALL, when running under `http` transport with `MUNINN_HTTP_TOKENS_FILE` configured, authenticate each request to `/mcp` and `/v1/context` against the configured token set and authorize every scope-bearing operation against the presenting token's scope grants. A bearer that is neither a configured token nor the static `MUNINN_HTTP_BEARER` SHALL be rejected with HTTP 401. For an authenticated scoped token, any operation naming scope keys — a `tools/call`, a `session-context` resource read, a `session-context` prompt request, or `GET /v1/context` — SHALL be permitted only when every requested scope key matches the token's grant (exact value or `*` per key, the union of grants when a token has several entries); a mismatch SHALL be rejected with a `scope_denied` error before any path resolution or IO, and the error message SHALL name the offending key without enumerating valid grants. Operations carrying no scope keys (e.g. `tools/list`) SHALL require only authentication. Tokens SHALL NOT appear in logs. The `stdio` transport SHALL be unaffected. Grants SHALL be resolved per request, so a token removed from the file no longer authorizes new operations after a restart-reload, including on already-open sessions.

#### Scenario: Scoped token confined to its grant
- **WHEN** the tokens file grants token `t1` `{ "agent": "jarvis", "user": "*" }` and a client presenting `t1` calls a tool with `agent=jarvis, user=tony`
- **THEN** the call proceeds normally

#### Scenario: Scope mismatch is denied before IO
- **WHEN** the same client presenting `t1` calls a tool with `agent=friday, user=tony`
- **THEN** the response is an MCP error with code `scope_denied` naming `agent`, and no vault path is resolved or read

#### Scenario: Unknown bearer is unauthenticated
- **WHEN** `MUNINN_HTTP_TOKENS_FILE` is configured and a client presents a bearer that appears in neither the tokens file nor `MUNINN_HTTP_BEARER`
- **THEN** the server responds with HTTP 401

#### Scenario: Static bearer retains all scopes
- **WHEN** both `MUNINN_HTTP_TOKENS_FILE` and `MUNINN_HTTP_BEARER=admin` are configured and a client presenting `admin` calls a tool with any valid scope
- **THEN** the call proceeds normally

#### Scenario: Scoped token gates the session-context surfaces
- **WHEN** a client presenting `t1` (granted `agent=jarvis` only) requests the `session-context` resource or prompt for `agent=friday`
- **THEN** the request is rejected with `scope_denied` and no context is rendered

#### Scenario: Union of grants for a repeated token
- **WHEN** the tokens file lists token `t2` twice, once granting `{ "agent": "jarvis", "user": "tony" }` and once `{ "agent": "friday", "user": "tony" }`
- **THEN** `t2` may name either agent with `user=tony` and no other combination
