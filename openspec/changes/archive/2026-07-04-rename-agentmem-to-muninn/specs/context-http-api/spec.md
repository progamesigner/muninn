## MODIFIED Requirements

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
