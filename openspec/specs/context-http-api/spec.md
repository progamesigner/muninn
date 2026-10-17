# context-http-api Specification

## Purpose
Provide stateless, read-only HTTP access to per-scope session context, lean
bootstrap, and vault layout guidance for clients that need rendered Markdown or
JSON without an MCP exchange. Reuse the shared renderers and HTTP authentication
and scope authorization, and expose unauthenticated liveness and readiness probes.

## Requirements
### Requirement: Versioned context endpoint
The system SHALL serve a stateless, read-only HTTP route `GET /v1/context` on the
HTTP transport's `axum` router, alongside `POST/GET /mcp`, `GET /healthz`, and
`GET /readyz`. The route SHALL render the per-scope session-context bootstrap using
the same renderer as the `load_session_context` tool, the `session-context`
resource, and the `session-context` prompt. The route SHALL exist only when the
binary is built with the `transport-http` feature and the HTTP transport is selected.

#### Scenario: Endpoint renders the bootstrap
- **WHEN** a client issues `GET /v1/context?agent=jarvis&user=tony` against a
  server whose scheme is `<agent>.<user>`
- **THEN** the server responds `200 OK` with the rendered session-context for
  the scope `{agent: "jarvis", user: "tony"}`, identical to what
  `load_session_context` would return for the same scope

#### Scenario: Rendering never errors on absent files
- **WHEN** `GET /v1/context` is called for a scope whose foundational files do
  not yet exist
- **THEN** the server responds `200 OK` with the bootstrap rendered from the
  compiled-in default template, with the missing files substituted by their
  sentinel — absence is never an error

#### Scenario: Endpoint is absent without the HTTP transport
- **WHEN** the server is running under the `stdio` transport
- **THEN** no TCP listener is opened and `GET /v1/context` is not served

### Requirement: Scope parameter binding
The endpoint SHALL accept exactly one query parameter per VFS-scheme placeholder,
named by the placeholder ident, and bind them into the scope map in the scheme's
order. It SHALL reject requests that omit a required placeholder, supply an empty
value, or include an unexpected parameter, with HTTP `400` and a JSON error body.
When the scheme is empty, the endpoint SHALL require no parameters.

#### Scenario: All placeholders supplied
- **WHEN** the scheme is `<team>.<agent>.<env>.<user>` and the request carries
  `?team=t&agent=a&env=prod&user=tony`
- **THEN** the server binds the scope `{team: "t", agent: "a", env: "prod", user: "tony"}`
  and responds `200 OK`

#### Scenario: Missing placeholder is rejected
- **WHEN** the scheme is `<agent>.<user>` and the request carries only `?agent=jarvis`
- **THEN** the server responds `400 Bad Request` with a JSON body that names the
  missing scope key `user`

#### Scenario: Empty placeholder value is rejected
- **WHEN** the request carries `?agent=jarvis&user=` (empty value)
- **THEN** the server responds `400 Bad Request` with a JSON body that names the
  offending scope key

#### Scenario: Unexpected parameter is rejected
- **WHEN** the scheme is `<agent>.<user>` and the request carries
  `?agent=jarvis&user=tony&role=admin`
- **THEN** the server responds `400 Bad Request` with a JSON body that names the
  unexpected parameter `role`

#### Scenario: Empty scheme requires no parameters
- **WHEN** the scheme is empty (`MUNINN_VFS_SCHEME=`) and the request is
  `GET /v1/context` with no query parameters
- **THEN** the server responds `200 OK` with the rendered single-tenant bootstrap

### Requirement: Response negotiation
The endpoint SHALL return the rendered bootstrap as `text/markdown` by default so
it can be used directly as a system prompt. When the request's `Accept` header
prefers `application/json`, the endpoint SHALL instead return a JSON object
`{ "rendered": <string>, "missing": [<string>, …] }` mirroring the
`load_session_context` tool result, with `Content-Type: application/json`.

#### Scenario: Default is markdown
- **WHEN** `GET /v1/context?agent=jarvis&user=tony` is sent without an `Accept`
  header (or with `Accept: text/markdown` or `*/*`)
- **THEN** the response `Content-Type` is `text/markdown` and the body is the
  rendered bootstrap text verbatim

#### Scenario: JSON negotiation
- **WHEN** `GET /v1/context?agent=jarvis&user=tony` is sent with
  `Accept: application/json`
- **THEN** the response `Content-Type` is `application/json` and the body is an
  object with a `rendered` string field and a `missing` array of the absent
  foundational filenames

### Requirement: Authentication reuse
The endpoint SHALL sit behind the same authentication gate as `/mcp`. When
`MUNINN_HTTP_BEARER` is set, `GET /v1/context` SHALL require a matching
`Authorization: Bearer <token>` header and SHALL respond `401` otherwise. When
`MUNINN_HTTP_TOKENS_FILE` is configured, a request presenting a configured
scoped token SHALL additionally have its query-parameter scope checked against
that token's grant: a mismatch SHALL be rejected with `403` and a JSON
`{ "error": … }` body naming the offending key, before any rendering. When
neither variable is set the endpoint is unauthenticated, like `/mcp`. The probe
routes `GET /healthz` and `GET /readyz` SHALL remain reachable without
authentication regardless.

#### Scenario: Missing bearer is rejected when configured
- **WHEN** the server is started with `MUNINN_HTTP_BEARER=secret` and
  `GET /v1/context?agent=jarvis&user=tony` is sent without an `Authorization`
  header
- **THEN** the server responds `401 Unauthorized` and does not render any context

#### Scenario: Matching bearer is accepted
- **WHEN** the server is started with `MUNINN_HTTP_BEARER=secret` and the
  request carries `Authorization: Bearer secret`
- **THEN** the server responds `200 OK` with the rendered context

#### Scenario: Unauthenticated when bearer unset
- **WHEN** both `MUNINN_HTTP_BEARER` and `MUNINN_HTTP_TOKENS_FILE` are unset
  and `GET /v1/context?agent=jarvis&user=tony` is sent without an
  `Authorization` header
- **THEN** the server responds `200 OK` with the rendered context

#### Scenario: Scoped token renders only its own scope
- **WHEN** the tokens file grants the presented token
  `{ "agent": "jarvis", "user": "*" }` and the request is
  `GET /v1/context?agent=jarvis&user=tony`
- **THEN** the server responds `200 OK` with the rendered context

#### Scenario: Scope mismatch yields 403
- **WHEN** the same token requests `GET /v1/context?agent=friday&user=tony`
- **THEN** the server responds `403` with a JSON `{ "error": … }` body naming
  `agent`, and no context is rendered

### Requirement: Error mapping
The endpoint SHALL map invalid scope input to HTTP `400` with a JSON body of the
form `{ "error": <human-readable message> }`, and SHALL map genuine IO failures
during rendering to HTTP `500` with the same body shape. Error bodies SHALL NOT
leak resolved physical filesystem paths; messages refer to virtual paths and
scope keys only.

#### Scenario: Validation error shape
- **WHEN** a request is rejected for a missing or unexpected scope parameter
- **THEN** the response is `400` with `Content-Type: application/json` and a body
  `{ "error": "<message naming the offending key>" }`

#### Scenario: IO failure maps to 500
- **WHEN** rendering fails because of an unexpected IO error (not a missing file)
- **THEN** the response is `500` with a JSON `{ "error": … }` body that does not
  include any resolved physical path

### Requirement: Liveness and readiness probes
The HTTP transport SHALL serve two ungated probe routes: `GET /healthz` (liveness)
and `GET /readyz` (readiness). `GET /healthz` SHALL report success as soon as the
process is up and SHALL NOT depend on recall index state. `GET /readyz` SHALL report
not-ready until every recall scope index and the shared index have been eagerly built
at startup, and ready thereafter. Both routes SHALL remain reachable without
authentication regardless of `MUNINN_HTTP_BEARER`. When recall is `off`, `GET
/readyz` SHALL report ready once the process is up.

#### Scenario: Liveness is up during the index build
- **WHEN** the server is still building recall indexes at startup and `GET /healthz`
  is requested
- **THEN** the response is `200 OK`, so an orchestrator's liveness probe does not kill
  the process mid-build

#### Scenario: Readiness flips only after the build completes
- **WHEN** `GET /readyz` is requested before the eager index build has finished
- **THEN** the response indicates not-ready (HTTP `503`); once all scope indexes and
  the shared index are built, `GET /readyz` responds `200 OK`

#### Scenario: Probes need no bearer token
- **WHEN** `MUNINN_HTTP_BEARER` is set and `GET /healthz` or `GET /readyz` is
  requested without an `Authorization` header
- **THEN** the response is the normal probe result, not `401`

### Requirement: Versioned bootstrap endpoint
The system SHALL serve a stateless, read-only HTTP route `GET /v1/bootstrap` on the HTTP transport's `axum` router, alongside `GET /v1/context`, `POST/GET /mcp`, `GET /healthz`, and `GET /readyz`. The route SHALL render the per-scope **lean bootstrap** (the `bootstrap` render kind of the shared renderer). It SHALL reuse the same scope-parameter binding, response negotiation (`text/markdown` by default, `{ rendered, missing }` JSON when `Accept` prefers `application/json`), authentication gate, and error mapping defined for `GET /v1/context`. The route SHALL exist only when the binary is built with the `transport-http` feature and the HTTP transport is selected. Adding this route SHALL NOT change the behavior or output shape of `GET /v1/context`.

#### Scenario: Bootstrap endpoint renders the lean bootstrap
- **WHEN** a client issues `GET /v1/bootstrap?agent=jarvis&user=tony` against a server whose scheme is `<agent>.<user>`
- **THEN** the server responds `200 OK` with the lean `bootstrap` render for the scope `{agent: "jarvis", user: "tony"}` — the scope banner, persona and rules, the onboarding directive (empty when no files are missing), and pointers to the full context and layout surfaces

#### Scenario: Bootstrap rendering never errors on absent files
- **WHEN** `GET /v1/bootstrap` is called for a scope whose foundational files do not yet exist
- **THEN** the server responds `200 OK` with the bootstrap rendered from the compiled-in default template, the missing files substituted by their sentinel and the onboarding directive present — absence is never an error

#### Scenario: Bootstrap endpoint reuses scope binding and auth
- **WHEN** `GET /v1/bootstrap` is called with a missing or unexpected scope parameter, or without a required `Authorization` bearer when one is configured
- **THEN** the server responds with the same `400`/`401`/`403` outcomes and JSON error shape as `GET /v1/context` under the identical conditions

#### Scenario: Bootstrap endpoint is absent without the HTTP transport
- **WHEN** the server is running under the `stdio` transport
- **THEN** no TCP listener is opened and `GET /v1/bootstrap` is not served

### Requirement: Versioned layout endpoint
The system SHALL serve a stateless, read-only HTTP route `GET /v1/layout` on the HTTP transport's `axum` router. The route SHALL render the per-scope **layout** content (the layout renderer) carrying the vault-mechanics guidance. It SHALL reuse the same scope-parameter binding, response negotiation, authentication gate, and error mapping defined for `GET /v1/context`. The route SHALL exist only when the binary is built with the `transport-http` feature and the HTTP transport is selected.

#### Scenario: Layout endpoint renders the layout
- **WHEN** a client issues `GET /v1/layout?agent=jarvis&user=tony` against a server whose scheme is `<agent>.<user>`
- **THEN** the server responds `200 OK` with the rendered layout content for that scope, identical to what the `muninn://session-layout/{…}` resource returns for the same scope

#### Scenario: Layout endpoint reuses scope binding and auth
- **WHEN** `GET /v1/layout` is called with a missing or unexpected scope parameter, or without a required `Authorization` bearer when one is configured
- **THEN** the server responds with the same `400`/`401`/`403` outcomes and JSON error shape as `GET /v1/context` under the identical conditions

#### Scenario: Layout endpoint is absent without the HTTP transport
- **WHEN** the server is running under the `stdio` transport
- **THEN** no TCP listener is opened and `GET /v1/layout` is not served
