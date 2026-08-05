//! The Streamable HTTP transport (default).
//!
//! Mounts the `rmcp` Streamable HTTP service at `POST`/`GET`/`DELETE /mcp` and a
//! plain `GET /v1/context` read endpoint behind an `axum` router that also serves
//! the Kubernetes-style `GET /healthz` (liveness) and `GET /readyz` (readiness)
//! probes. The MCP service runs **stateless** with **JSON-direct responses**: a
//! tool call resolves synchronously and the server advertises no notifications,
//! so each `POST /mcp` is answered with a plain `application/json` body rather
//! than an SSE stream, no `Mcp-Session-Id` is issued, and the `GET /mcp` stream
//! carries nothing. When `MUNINN_HTTP_BEARER` and/or `MUNINN_HTTP_TOKENS_FILE` is
//! configured, an `axum` middleware resolves the presented `Authorization:
//! Bearer <token>` header to a scope [`Grant`] — all scopes for the static
//! bearer, the configured grant for a scoped token — attaches it to the request,
//! and returns HTTP 401 for anything else; the probe routes are always
//! reachable. The token string itself never travels past the middleware.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Extension, Query, Request, State};
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::config::{Grant, TokenGrants};
use crate::error::MuninnError;
use crate::mcp::MuninnServer;
use crate::session_context::RenderKind;

/// Serve over Streamable HTTP, binding `bind` until a termination signal arrives.
///
/// `allowed_hosts` configures the transport's inbound `Host` validation: an empty
/// list keeps `rmcp`'s loopback-only default (`localhost`, `127.0.0.1`, `::1`),
/// the sole entry `*` disables validation, and any other list is used verbatim.
pub async fn serve(
    bind: SocketAddr,
    bearer: Option<String>,
    tokens: Option<TokenGrants>,
    allowed_hosts: Vec<String>,
    server: MuninnServer,
) -> anyhow::Result<()> {
    if bearer.is_none() && tokens.is_none() {
        tracing::warn!(
            "MUNINN_HTTP_BEARER and MUNINN_HTTP_TOKENS_FILE are unset; \
             the HTTP endpoint is unauthenticated"
        );
        if !bind.ip().is_loopback() {
            tracing::warn!(
                %bind,
                "binding a non-loopback interface without MUNINN_HTTP_BEARER or \
                 MUNINN_HTTP_TOKENS_FILE; the endpoint is reachable off-host \
                 without authentication"
            );
        }
    }

    // Host validation differs per branch; statelessness and JSON-direct
    // responses are applied uniformly to whichever base config is chosen.
    let http_config = if allowed_hosts.is_empty() {
        // No override: keep rmcp's loopback-only DNS-rebinding default.
        StreamableHttpServerConfig::default()
    } else if allowed_hosts == ["*"] {
        tracing::warn!(
            "MUNINN_HTTP_ALLOWED_HOSTS=* disables Host validation; \
             any Host header will be accepted"
        );
        StreamableHttpServerConfig::default().disable_allowed_hosts()
    } else {
        tracing::info!(allowed_hosts = %allowed_hosts.join(", "), "Host validation allow-list");
        StreamableHttpServerConfig::default().with_allowed_hosts(allowed_hosts)
    }
    // Stateless + JSON-direct: every tool call is a synchronous request→response
    // and the server advertises no notifications, so there is nothing for a
    // session or an SSE stream to carry. Each `POST /mcp` is answered with a
    // plain `application/json` body, no `Mcp-Session-Id` is issued, and the
    // `GET /mcp` resume churn disappears.
    .with_stateful_mode(false)
    .with_json_response(true);

    let mcp_service = StreamableHttpService::new(
        {
            let server = server.clone();
            move || Ok(server.clone())
        },
        // No sessions: the stateless POST path never touches the manager.
        Arc::new(NeverSessionManager::default()),
        http_config,
    );

    // The gated sub-router carries the MCP service and the `/v1/context` read
    // endpoint; both inherit the bearer middleware when one is configured. The
    // `MuninnServer` is wired in as handler state so `/v1/context` can reach
    // the shared renderer.
    // Ungated probes: liveness never depends on the index (so an orchestrator
    // won't kill a building pod), readiness reflects the eager index build.
    let probes = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(server.clone());

    let mut gated = Router::new()
        .route_service("/mcp", mcp_service)
        .route("/v1/context", get(context))
        .route("/v1/bootstrap", get(bootstrap))
        .route("/v1/layout", get(layout))
        .with_state(server);
    if bearer.is_some() || tokens.is_some() {
        let auth = Arc::new(HttpAuth {
            static_bearer: bearer,
            tokens,
        });
        gated = gated.layer(from_fn_with_state(auth, require_bearer));
    }

    let app = probes.merge(gated);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "serving MCP over Streamable HTTP");
    axum::serve(listener, app)
        .with_graceful_shutdown(super::shutdown_signal())
        .await?;
    Ok(())
}

/// Liveness route. Always succeeds once the process is up — it never depends on
/// the recall index, so a slow cold build cannot fail a liveness probe.
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness route. Reports `200` only once the recall index is built (or
/// immediately when recall is disabled); `503` while the eager build is in flight,
/// so traffic is held until the server can serve recall.
async fn readyz(State(server): State<MuninnServer>) -> Response {
    if server.recall_ready() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "indexing").into_response()
    }
}

/// `GET /v1/context` — render the per-scope full session context.
///
/// Each VFS-scheme placeholder is supplied as a query parameter (e.g.
/// `?agent=jarvis&user=tony`); the scheme's placeholders are bound into the
/// scope in order. The same renderer that backs `load_session_context` produces
/// the body. Returns `text/markdown` by default, or `{ rendered, missing }` JSON
/// when the `Accept` header prefers `application/json`.
async fn context(
    State(server): State<MuninnServer>,
    grant: Option<Extension<Grant>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let (scope, grant) = match bind_scope(&server, grant, &params) {
        Ok(bound) => bound,
        Err(resp) => return *resp,
    };
    // The renderer reads the vault synchronously; keep it off the runtime's
    // workers so a slow read cannot stall the probes or other requests.
    let rendered = tokio::task::spawn_blocking(move || {
        server.render_session_context(&scope, &grant, RenderKind::Context)
    })
    .await;
    match rendered {
        Ok(Ok(sc)) => render_payload(&headers, sc.rendered, sc.missing),
        Ok(Err(err)) => render_error(err),
        Err(err) => join_error(err),
    }
}

/// `GET /v1/bootstrap` — render the per-scope lean session bootstrap. Shares the
/// scope binding, response negotiation, auth gate, and error mapping with
/// `GET /v1/context`; only the render kind differs.
async fn bootstrap(
    State(server): State<MuninnServer>,
    grant: Option<Extension<Grant>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let (scope, grant) = match bind_scope(&server, grant, &params) {
        Ok(bound) => bound,
        Err(resp) => return *resp,
    };
    let rendered = tokio::task::spawn_blocking(move || {
        server.render_session_context(&scope, &grant, RenderKind::Bootstrap)
    })
    .await;
    match rendered {
        Ok(Ok(sc)) => render_payload(&headers, sc.rendered, sc.missing),
        Ok(Err(err)) => render_error(err),
        Err(err) => join_error(err),
    }
}

/// `GET /v1/layout` — render the per-scope layout document. Shares the scope
/// binding, response negotiation, auth gate, and error mapping with
/// `GET /v1/context`. The JSON form carries an empty `missing` list.
async fn layout(
    State(server): State<MuninnServer>,
    grant: Option<Extension<Grant>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let (scope, grant) = match bind_scope(&server, grant, &params) {
        Ok(bound) => bound,
        Err(resp) => return *resp,
    };
    let rendered = tokio::task::spawn_blocking(move || server.render_layout(&scope, &grant)).await;
    match rendered {
        Ok(Ok(rendered)) => render_payload(&headers, rendered, Vec::new()),
        Ok(Err(err)) => render_error(err),
        Err(err) => join_error(err),
    }
}

/// Resolve the request's grant (all-scopes when no authentication is configured)
/// and bind the scheme placeholders from the query. Rejects any non-placeholder
/// query parameter with a `400`; absent or empty values fall through to the
/// renderer's own validation (`MissingScope` / `InvalidArgument`). Shared by the
/// three `/v1/*` render endpoints.
fn bind_scope(
    server: &MuninnServer,
    grant: Option<Extension<Grant>>,
    params: &HashMap<String, String>,
) -> Result<(BTreeMap<String, String>, Grant), Box<Response>> {
    let grant = grant.map(|Extension(g)| g).unwrap_or(Grant::AllScopes);
    let placeholders = server.scheme_placeholders();
    if let Some(unexpected) = params
        .keys()
        .find(|k| !placeholders.iter().any(|p| p == *k))
    {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            format!("unexpected query parameter '{unexpected}'"),
        )));
    }
    let mut scope: BTreeMap<String, String> = BTreeMap::new();
    for ph in &placeholders {
        if let Some(value) = params.get(ph) {
            scope.insert(ph.clone(), value.clone());
        }
    }
    Ok((scope, grant))
}

/// Render a `{ rendered, missing }` payload as `text/markdown` by default, or as
/// JSON when the `Accept` header prefers `application/json`.
fn render_payload(headers: &HeaderMap, rendered: String, missing: Vec<String>) -> Response {
    if prefers_json(headers) {
        Json(serde_json::json!({ "rendered": rendered, "missing": missing })).into_response()
    } else {
        ([(CONTENT_TYPE, "text/markdown; charset=utf-8")], rendered).into_response()
    }
}

/// Map a render error to an HTTP response with the shared `{ "error": … }` shape:
/// scope-validation errors to `400`, grant denials to `403`, IO failures to `500`.
fn render_error(err: MuninnError) -> Response {
    let status = match err {
        MuninnError::MissingScope { .. } | MuninnError::InvalidArgument { .. } => {
            StatusCode::BAD_REQUEST
        }
        MuninnError::ScopeDenied { .. } => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, err.to_string())
}

/// Map a blocking-task join failure — a panicking render, or cancellation during
/// shutdown — onto a `500` with the shared `{ "error": … }` shape.
fn join_error(err: tokio::task::JoinError) -> Response {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("render failed: {err}"),
    )
}

/// `true` when the `Accept` header asks for `application/json`.
fn prefers_json(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/json"))
}

/// A `{ "error": <message> }` JSON response with the given status.
fn error(status: StatusCode, message: String) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The authentication configuration for the gated routes: the optional static
/// bearer (which carries the all-scopes grant) plus the optional per-token
/// grant table from `MUNINN_HTTP_TOKENS_FILE`.
struct HttpAuth {
    static_bearer: Option<String>,
    tokens: Option<TokenGrants>,
}

/// Resolve the presented bearer to a [`Grant`] and attach it to the request as
/// an extension (rmcp propagates request parts into the MCP request context;
/// the `/v1/context` handler reads it directly). Unknown or missing bearers are
/// rejected with 401. The token string is dropped here and never logged.
async fn require_bearer(
    State(auth): State<Arc<HttpAuth>>,
    mut request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let grant = presented.and_then(|token| {
        if auth.static_bearer.as_deref() == Some(token) {
            Some(Grant::AllScopes)
        } else {
            auth.tokens.as_ref().and_then(|t| t.grant_for(token))
        }
    });

    match grant {
        Some(grant) => {
            request.extensions_mut().insert(grant);
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": -32001, "message": "unauthorized: missing or invalid bearer token" },
                "id": null
            })),
        )
            .into_response(),
    }
}
