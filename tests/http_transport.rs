//! HTTP transport integration tests (tasks 10.5–10.7).
//!
//! Each test spawns the real `muninn` binary as a child process bound to a
//! loopback (or wildcard) address and drives it over HTTP.

use std::process::Stdio;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, GetPromptRequestParams, ReadResourceRequestParams};
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Spawn the binary with the given bind and optional bearer. `bind` of `None`
/// exercises the default `127.0.0.1:8000`. Stderr is piped so warnings can be
/// inspected.
fn spawn(root: &std::path::Path, bind: Option<&str>, bearer: Option<&str>) -> Child {
    spawn_full(root, bind, bearer, None)
}

/// Like [`spawn`], with an optional `MUNINN_HTTP_ALLOWED_HOSTS` value.
fn spawn_full(
    root: &std::path::Path,
    bind: Option<&str>,
    bearer: Option<&str>,
    allowed_hosts: Option<&str>,
) -> Child {
    let bin = env!("CARGO_BIN_EXE_muninn");
    let mut cmd = Command::new(bin);
    cmd.env("MUNINN_ROOT_DIR", root)
        .env("MUNINN_TRANSPORT", "http")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(b) = bind {
        cmd.env("MUNINN_HTTP_BIND", b);
    }
    if let Some(token) = bearer {
        cmd.env("MUNINN_HTTP_BEARER", token);
    }
    if let Some(hosts) = allowed_hosts {
        cmd.env("MUNINN_HTTP_ALLOWED_HOSTS", hosts);
    }
    cmd.spawn().expect("spawn muninn")
}

/// `POST /mcp` carrying an explicit `Host` header, returning the HTTP status.
/// rmcp's DNS-rebinding gate answers `403` before any MCP processing when the
/// `Host` is not allowed; an allowed `Host` falls through to MCP handling
/// (which, for this minimal body, is any non-`403` status).
async fn mcp_post_status(base: &str, host: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .header("Host", host)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(r#"{"jsonrpc":"2.0","method":"ping","id":1}"#)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// Poll `GET /healthz` until it succeeds or the timeout elapses. The budget is
/// generous because the suite spawns many servers in parallel, each starting its
/// own recall warm-up at boot.
async fn wait_health(base: &str) {
    let client = reqwest::Client::new();
    for _ in 0..200 {
        if let Ok(resp) = client.get(format!("{base}/healthz")).send().await
            && resp.status().is_success()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("server did not become healthy at {base}");
}

/// Write a tokens file into `dir` (kept outside the vault root) and return its path.
fn write_tokens_file(dir: &assert_fs::TempDir, tokens: serde_json::Value) -> std::path::PathBuf {
    let path = dir.path().join("tokens.json");
    std::fs::write(&path, serde_json::json!({ "tokens": tokens }).to_string()).unwrap();
    path
}

/// Like [`spawn_full`], with an `MUNINN_HTTP_TOKENS_FILE` path.
fn spawn_scoped(
    root: &std::path::Path,
    bind: &str,
    bearer: Option<&str>,
    tokens_file: &std::path::Path,
) -> Child {
    let bin = env!("CARGO_BIN_EXE_muninn");
    let mut cmd = Command::new(bin);
    cmd.env("MUNINN_ROOT_DIR", root)
        .env("MUNINN_TRANSPORT", "http")
        .env("MUNINN_HTTP_BIND", bind)
        .env("MUNINN_HTTP_TOKENS_FILE", tokens_file)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(token) = bearer {
        cmd.env("MUNINN_HTTP_BEARER", token);
    }
    cmd.spawn().expect("spawn muninn")
}

/// An MCP client whose every request presents `Authorization: Bearer <token>`.
async fn mcp_client(base: &str, token: &str) -> RunningService<RoleClient, ()> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let transport = StreamableHttpClientTransport::with_client(
        client,
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")),
    );
    ().serve(transport).await.expect("mcp initialize")
}

/// Call `load_session_context` for the given scope and return the tool result.
async fn call_context_tool(
    service: &RunningService<RoleClient, ()>,
    agent: &str,
    user: &str,
) -> rmcp::model::CallToolResult {
    service
        .call_tool(
            CallToolRequestParams::new("load_session_context").with_arguments(
                json!({ "agent": agent, "user": user })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("tools/call transport round-trip")
}

/// Assert a tool result is the `scope_denied` domain error naming `key`.
fn assert_scope_denied(result: &rmcp::model::CallToolResult, key: &str) {
    assert_eq!(result.is_error, Some(true), "expected a tool error");
    let structured = result.structured_content.as_ref().unwrap();
    assert_eq!(structured["code"], "scope_denied");
    let message = structured["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("'{key}'")),
        "message should name '{key}': {message}"
    );
}

#[tokio::test]
async fn http_default_bind_health_and_mcp_roundtrip() {
    let tmp = assert_fs::TempDir::new().unwrap();
    // MUNINN_ROOT_DIR is the only override → default bind 127.0.0.1:8000.
    let mut child = spawn(tmp.path(), None, None);
    let base = "http://127.0.0.1:8000";

    wait_health(base).await;

    // MCP initialize + tools/list over Streamable HTTP.
    let transport = StreamableHttpClientTransport::with_client(
        reqwest::Client::default(),
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")),
    );
    let service = ().serve(transport).await.expect("mcp initialize");
    let tools = service.list_tools(Default::default()).await.unwrap();
    // Fourteen core tools plus recall_memory_notes (default `simple` backend).
    assert_eq!(tools.tools.len(), 15);
    assert!(tools.tools.iter().any(|t| t.name == "recall_memory_notes"));
    service.cancel().await.unwrap();

    child.kill().await.unwrap();
}

/// Stateless + JSON-direct mode: `POST /mcp` answers with `application/json`
/// (not `text/event-stream`), issues no `Mcp-Session-Id`, and a follow-up
/// request succeeds without carrying one.
#[tokio::test]
async fn http_post_returns_json_and_no_session_id() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18658";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();
    let post = |body: String| {
        client
            .post(format!("{base}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(body)
            .send()
    };

    // initialize → plain JSON, no session id header.
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }
    });
    let resp = post(init.to_string()).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/json"),
        "initialize must return application/json, not SSE"
    );
    assert!(
        resp.headers().get("mcp-session-id").is_none(),
        "stateless mode must not issue a session id"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["serverInfo"]["name"], "rmcp");

    // tools/call carrying no session id still resolves, as plain JSON.
    let call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "load_session_context",
            "arguments": { "agent": "jarvis", "user": "tony" }
        }
    });
    let resp = post(call.to_string()).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/json"),
        "tools/call must return application/json, not SSE"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_ne!(body["result"]["isError"], true);
    assert!(
        body["result"]["structuredContent"]["rendered"]
            .as_str()
            .unwrap()
            .contains("# Session Context")
    );

    child.kill().await.unwrap();
}

/// Stateless mode skips rmcp's built-in handshake negotiation, so the server
/// must echo the client's requested protocol version itself — otherwise it
/// always advertises its latest, which older clients reject.
#[tokio::test]
async fn http_initialize_echoes_requested_protocol_version() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18659";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();
    // Each known version the client asks for must come back unchanged.
    for requested in ["2025-03-26", "2025-06-18", "2025-11-25"] {
        let resp = client
            .post(format!("{base}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {
                        "protocolVersion": requested,
                        "capabilities": {},
                        "clientInfo": { "name": "test", "version": "0" }
                    }
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["result"]["protocolVersion"], requested,
            "server must echo the requested protocol version {requested}"
        );
    }

    child.kill().await.unwrap();
}

#[tokio::test]
async fn http_readyz_and_healthz_probes() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18657";
    // A bearer is set so we also confirm the probes need no Authorization header.
    let mut child = spawn(tmp.path(), Some(bind), Some("s3cret"));
    let base = format!("http://{bind}");
    let client = reqwest::Client::new();

    // Liveness comes up with the process, with no bearer token.
    wait_health(&base).await;

    // Readiness flips to 200 once the eager index build completes (also ungated).
    let mut ready = false;
    for _ in 0..100 {
        if let Ok(resp) = client.get(format!("{base}/readyz")).send().await
            && resp.status().is_success()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready, "/readyz never reported ready");

    child.kill().await.unwrap();
}

#[tokio::test]
async fn http_unauthenticated_request_is_rejected_when_bearer_set() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18651";
    let mut child = spawn(tmp.path(), Some(bind), Some("s3cret"));
    let base = format!("http://{bind}");

    // /healthz is unauthenticated, so it is the readiness probe.
    wait_health(&base).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(r#"{"jsonrpc":"2.0","method":"ping","id":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    child.kill().await.unwrap();
}

#[tokio::test]
async fn http_non_loopback_without_bearer_warns_on_startup() {
    let tmp = assert_fs::TempDir::new().unwrap();
    // Wildcard bind, ephemeral port, no bearer → startup WARN expected.
    let mut child = spawn(tmp.path(), Some("0.0.0.0:0"), None);

    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();

    let found = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("WARN") && line.contains("MUNINN_HTTP_BEARER") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    child.kill().await.unwrap();
    assert!(found, "expected a startup WARN naming MUNINN_HTTP_BEARER");
}

// --- Host validation (MUNINN_HTTP_ALLOWED_HOSTS) --------------------------

#[tokio::test]
async fn http_rejects_non_loopback_host_by_default() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18661";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await; // /healthz is not Host-gated.

    // Default allow-list is loopback only, so a cluster DNS Host is rejected.
    let status = mcp_post_status(&base, "muninn.svc.cluster.local").await;
    assert_eq!(
        status, 403,
        "non-loopback Host should be forbidden by default"
    );

    child.kill().await.unwrap();
}

#[tokio::test]
async fn http_accepts_allowlisted_host() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18662";
    let mut child = spawn_full(
        tmp.path(),
        Some(bind),
        None,
        Some("muninn.svc.cluster.local"),
    );
    let base = format!("http://{bind}");
    wait_health(&base).await;

    // The allow-listed Host clears the DNS-rebinding gate (non-403).
    let status = mcp_post_status(&base, "muninn.svc.cluster.local").await;
    assert_ne!(status, 403, "allow-listed Host should pass validation");

    // A Host outside the list is still rejected.
    let status = mcp_post_status(&base, "evil.example.net").await;
    assert_eq!(status, 403, "non-listed Host should remain forbidden");

    child.kill().await.unwrap();
}

#[tokio::test]
async fn http_wildcard_accepts_any_host() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18663";
    let mut child = spawn_full(tmp.path(), Some(bind), None, Some("*"));
    let base = format!("http://{bind}");
    wait_health(&base).await;

    // With validation disabled, an arbitrary Host clears the gate.
    let status = mcp_post_status(&base, "anything.example.org").await;
    assert_ne!(status, 403, "wildcard should accept any Host");

    child.kill().await.unwrap();
}

#[tokio::test]
async fn http_loopback_host_accepted_when_unset() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18664";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    // The unchanged default still accepts loopback.
    let status = mcp_post_status(&base, "127.0.0.1:18664").await;
    assert_ne!(status, 403, "loopback Host must remain accepted by default");

    child.kill().await.unwrap();
}

// --- GET /v1/context -------------------------------------------------------

#[tokio::test]
async fn context_endpoint_renders_markdown_bootstrap() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18652";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/context?agent=jarvis&user=tony"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/markdown"),
        "expected text/markdown content type"
    );
    let body = resp.text().await.unwrap();
    // A fresh vault renders the compiled-in default context template: the
    // foundational slots and a pointer to the layout surface, no tools guide
    // and no embedded layout prose.
    assert!(body.contains("# Session Context"));
    assert!(body.contains("<PERSONA>"));
    assert!(!body.contains("<MUNINN:TOOLS>"));
    assert!(body.contains("session-layout"));

    child.kill().await.unwrap();
}

#[tokio::test]
async fn context_endpoint_json_negotiation_reports_missing() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18653";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/context?agent=jarvis&user=tony"))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["rendered"]
            .as_str()
            .unwrap()
            .contains("# Session Context")
    );
    // A fresh vault has all five foundational files absent.
    let missing = body["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 5);
    assert!(missing.iter().any(|v| v == "PERSONA.md"));
    assert!(missing.iter().any(|v| v == "MEMORY.md"));

    child.kill().await.unwrap();
}

#[tokio::test]
async fn context_endpoint_rejects_invalid_scope() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18654";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();

    // Missing placeholder `user`.
    let resp = client
        .get(format!("{base}/v1/context?agent=jarvis"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("user"));

    // Empty value for `user`.
    let resp = client
        .get(format!("{base}/v1/context?agent=jarvis&user="))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("user"));

    // Unexpected parameter `role`.
    let resp = client
        .get(format!(
            "{base}/v1/context?agent=jarvis&user=tony&role=admin"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("role"));

    child.kill().await.unwrap();
}

#[tokio::test]
async fn context_endpoint_honours_bearer() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18655";
    let mut child = spawn(tmp.path(), Some(bind), Some("s3cret"));
    let base = format!("http://{bind}");
    wait_health(&base).await; // /healthz stays reachable without auth.

    let client = reqwest::Client::new();
    let url = format!("{base}/v1/context?agent=jarvis&user=tony");

    // No bearer → 401.
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    // Matching bearer → 200.
    let resp = client
        .get(&url)
        .header("Authorization", "Bearer s3cret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp.text().await.unwrap().contains("# Session Context"));

    child.kill().await.unwrap();
}

#[tokio::test]
async fn bootstrap_endpoint_renders_lean() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18681";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/bootstrap?agent=jarvis&user=tony"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/markdown")
    );
    let body = resp.text().await.unwrap();
    // Lean: distinct heading, scope banner, load_session_context + layout
    // pointers; no persona, no wrapper tags, no heavier sections.
    assert!(body.contains("# Session Bootstrap"));
    assert!(!body.contains("# Session Context"));
    assert!(!body.contains("<PERSONA>"));
    assert!(!body.contains("<RULES>"));
    assert!(body.contains("load_session_context"));
    assert!(body.contains("session-layout"));
    assert!(!body.contains("<MEMORY>"));
    assert!(!body.contains("<MUNINN:TOOLS>"));

    child.kill().await.unwrap();
}

#[tokio::test]
async fn layout_endpoint_renders_and_reuses_context_behavior() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18682";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();

    // Renders the layout document as markdown.
    let resp = client
        .get(format!("{base}/v1/layout?agent=jarvis&user=tony"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("# Memory Layout"));
    assert!(body.contains("ordinary filesystem"));
    // Onboarding guidance is not part of the layout surface.
    assert!(!body.contains("Onboarding needed"));

    // Reuses the context endpoint's scope validation.
    let resp = client
        .get(format!("{base}/v1/layout?agent=jarvis"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let err: serde_json::Value = resp.json().await.unwrap();
    assert!(err["error"].as_str().unwrap().contains("user"));

    // JSON negotiation mirrors /v1/context, with an empty `missing`.
    let resp = client
        .get(format!("{base}/v1/layout?agent=jarvis&user=tony"))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(
        json["rendered"]
            .as_str()
            .unwrap()
            .contains("# Memory Layout")
    );
    assert_eq!(json["missing"].as_array().unwrap().len(), 0);

    child.kill().await.unwrap();
}

// --- Scoped bearer tokens (MUNINN_HTTP_TOKENS_FILE) -----------------------

#[tokio::test]
async fn scoped_token_confined_to_its_grant_on_tools() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([ { "token": "t1", "scopes": { "agent": "jarvis", "user": "*" } } ]),
    );
    let bind = "127.0.0.1:18671";
    let mut child = spawn_scoped(tmp.path(), bind, None, &tokens);
    let base = format!("http://{bind}");
    wait_health(&base).await; // probes stay ungated with a tokens file configured

    let service = mcp_client(&base, "t1").await;

    // Own scope (wildcard user) succeeds.
    let ok = call_context_tool(&service, "jarvis", "tony").await;
    assert_ne!(ok.is_error, Some(true), "own scope must succeed");

    // Foreign agent → scope_denied naming `agent`, before any IO.
    let denied = call_context_tool(&service, "friday", "tony").await;
    assert_scope_denied(&denied, "agent");

    // A scope-bearing write is denied the same way.
    let denied = service
        .call_tool(
            CallToolRequestParams::new("write_memory_note").with_arguments(
                json!({
                    "agent": "friday", "user": "tony",
                    "path": "Agents/topics/note.md", "content": "nope"
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .unwrap();
    assert_scope_denied(&denied, "agent");
    // Deny-before-IO: nothing was written for the foreign scope.
    assert!(!tmp.path().join("Agents/friday.tony").exists());

    service.cancel().await.unwrap();
    child.kill().await.unwrap();
}

#[tokio::test]
async fn unknown_bearer_is_rejected_with_401() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([ { "token": "t1", "scopes": { "agent": "jarvis", "user": "*" } } ]),
    );
    let bind = "127.0.0.1:18672";
    let mut child = spawn_scoped(tmp.path(), bind, None, &tokens);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();
    for auth in [None, Some("Bearer nope")] {
        let mut req = client
            .post(format!("{base}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(r#"{"jsonrpc":"2.0","method":"ping","id":1}"#);
        if let Some(value) = auth {
            req = req.header("Authorization", value);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 401, "auth case {auth:?}");
    }

    child.kill().await.unwrap();
}

#[tokio::test]
async fn static_bearer_retains_all_scopes_alongside_tokens_file() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([ { "token": "t1", "scopes": { "agent": "jarvis", "user": "*" } } ]),
    );
    let bind = "127.0.0.1:18673";
    let mut child = spawn_scoped(tmp.path(), bind, Some("admin"), &tokens);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let service = mcp_client(&base, "admin").await;
    for (agent, user) in [("jarvis", "tony"), ("friday", "pepper")] {
        let result = call_context_tool(&service, agent, user).await;
        assert_ne!(
            result.is_error,
            Some(true),
            "static bearer must reach {agent}/{user}"
        );
    }
    service.cancel().await.unwrap();

    child.kill().await.unwrap();
}

#[tokio::test]
async fn union_of_grants_for_a_repeated_token() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([
            { "token": "t2", "scopes": { "agent": "jarvis", "user": "tony" } },
            { "token": "t2", "scopes": { "agent": "friday", "user": "tony" } }
        ]),
    );
    let bind = "127.0.0.1:18674";
    let mut child = spawn_scoped(tmp.path(), bind, None, &tokens);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let service = mcp_client(&base, "t2").await;

    // Either granted combination works.
    for agent in ["jarvis", "friday"] {
        let result = call_context_tool(&service, agent, "tony").await;
        assert_ne!(result.is_error, Some(true), "{agent}/tony must succeed");
    }
    // The union is entry-wise: no cross-combination.
    let denied = call_context_tool(&service, "jarvis", "pepper").await;
    assert_scope_denied(&denied, "user");

    service.cancel().await.unwrap();
    child.kill().await.unwrap();
}

/// Grants are resolved per request, never cached. In stateless mode each
/// `POST /mcp` is independent: it carries its own bearer, gets a plain
/// `application/json` response with no session id, and an unknown bearer is
/// rejected — the prior request grants it nothing.
#[tokio::test]
async fn grant_resolved_per_request_stateless() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([
            { "token": "t1", "scopes": { "agent": "jarvis", "user": "*" } },
            { "token": "t3", "scopes": { "agent": "friday", "user": "*" } }
        ]),
    );
    let bind = "127.0.0.1:18675";
    let mut child = spawn_scoped(tmp.path(), bind, None, &tokens);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let client = reqwest::Client::new();
    let post = |bearer: &str, body: String| {
        client
            .post(format!("{base}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {bearer}"))
            .body(body)
            .send()
    };

    let call = |id: u64, agent: &str, user: &str| {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {
                "name": "load_session_context",
                "arguments": { "agent": agent, "user": user }
            }
        })
        .to_string()
    };

    // t1 reaches its own scope; the result is a plain JSON body, no session id.
    let resp = post("t1", call(2, "jarvis", "tony")).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp.headers().get("mcp-session-id").is_none());
    let msg: serde_json::Value = resp.json().await.unwrap();
    assert_ne!(msg["result"]["isError"], true);

    // A different bearer on the next request resolves to its own grant …
    let resp = post("t3", call(3, "friday", "tony")).await.unwrap();
    let msg: serde_json::Value = resp.json().await.unwrap();
    assert_ne!(msg["result"]["isError"], true);

    // … and is denied outside it (jarvis is t1's scope, not t3's).
    let resp = post("t3", call(4, "jarvis", "tony")).await.unwrap();
    let msg: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(msg["result"]["isError"], true);
    assert_eq!(msg["result"]["structuredContent"]["code"], "scope_denied");

    // A bearer not in the table is rejected; the prior requests cached nothing.
    let resp = post("revoked", call(5, "jarvis", "tony")).await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    child.kill().await.unwrap();
}

#[tokio::test]
async fn scoped_token_gates_resource_and_prompt_surfaces() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([ { "token": "t1", "scopes": { "agent": "jarvis", "user": "*" } } ]),
    );
    let bind = "127.0.0.1:18676";
    let mut child = spawn_scoped(tmp.path(), bind, None, &tokens);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    let service = mcp_client(&base, "t1").await;

    // Resource: own scope renders; a foreign scope is refused with scope_denied.
    let ok = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-context/jarvis/tony",
        ))
        .await
        .expect("own-scope resource read");
    assert!(!ok.contents.is_empty());
    let err = service
        .read_resource(ReadResourceRequestParams::new(
            "muninn://session-context/friday/tony",
        ))
        .await
        .expect_err("foreign-scope resource read must fail");
    let rendered = format!("{err:?}");
    assert!(rendered.contains("scope_denied"), "got: {rendered}");

    // Prompt: same gate.
    let args = json!({ "agent": "jarvis", "user": "tony" });
    let ok = service
        .get_prompt(
            GetPromptRequestParams::new("session-context")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .expect("own-scope prompt");
    assert!(!ok.messages.is_empty());
    let args = json!({ "agent": "friday", "user": "tony" });
    let err = service
        .get_prompt(
            GetPromptRequestParams::new("session-context")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .expect_err("foreign-scope prompt must fail");
    let rendered = format!("{err:?}");
    assert!(rendered.contains("scope_denied"), "got: {rendered}");

    service.cancel().await.unwrap();
    child.kill().await.unwrap();
}

#[tokio::test]
async fn context_endpoint_enforces_scoped_token_grant() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([ { "token": "t1", "scopes": { "agent": "jarvis", "user": "*" } } ]),
    );
    let bind = "127.0.0.1:18677";
    let mut child = spawn_scoped(tmp.path(), bind, None, &tokens);
    let base = format!("http://{bind}");
    wait_health(&base).await; // probes need no auth

    let client = reqwest::Client::new();

    // No bearer → 401.
    let resp = client
        .get(format!("{base}/v1/context?agent=jarvis&user=tony"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    // Own scope → 200 with the rendered context.
    let resp = client
        .get(format!("{base}/v1/context?agent=jarvis&user=tony"))
        .header("Authorization", "Bearer t1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp.text().await.unwrap().contains("# Session Context"));

    // Foreign scope → 403 with the standard error body naming the key.
    let resp = client
        .get(format!("{base}/v1/context?agent=friday&user=tony"))
        .header("Authorization", "Bearer t1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("'agent'"),
        "got: {body}"
    );

    child.kill().await.unwrap();
}

#[tokio::test]
async fn invalid_tokens_file_fails_startup_without_echoing_tokens() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    // `tenant` is not a placeholder of the default `<agent>.<user>` scheme.
    let tokens = write_tokens_file(
        &aux,
        json!([ { "token": "sup3r-secret", "scopes": { "tenant": "x" } } ]),
    );
    let child = spawn_scoped(tmp.path(), "127.0.0.1:18678", None, &tokens);

    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("startup validation must exit promptly")
        .unwrap();
    assert!(!output.status.success(), "startup must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("MUNINN_HTTP_TOKENS_FILE"), "got: {stderr}");
    assert!(stderr.contains("'tenant'"), "got: {stderr}");
    assert!(!stderr.contains("sup3r-secret"), "token echoed: {stderr}");
}

#[tokio::test]
async fn print_config_redacts_tokens() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let aux = assert_fs::TempDir::new().unwrap();
    let tokens = write_tokens_file(
        &aux,
        json!([ { "token": "sup3r-secret", "scopes": { "agent": "jarvis", "user": "*" } } ]),
    );

    let bin = env!("CARGO_BIN_EXE_muninn");
    let output = Command::new(bin)
        .arg("--print-config")
        .env("MUNINN_ROOT_DIR", tmp.path())
        .env("MUNINN_TRANSPORT", "http")
        .env("MUNINN_HTTP_TOKENS_FILE", &tokens)
        .env("MUNINN_HTTP_BEARER", "h4rd-secret")
        .output()
        .await
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tokens=1 token(s)"), "got: {stderr}");
    assert!(stderr.contains("bearer=set"), "got: {stderr}");
    assert!(!stderr.contains("sup3r-secret"), "token echoed: {stderr}");
    assert!(!stderr.contains("h4rd-secret"), "bearer echoed: {stderr}");
}

#[tokio::test]
async fn context_endpoint_matches_load_session_context_tool() {
    let tmp = assert_fs::TempDir::new().unwrap();
    let bind = "127.0.0.1:18656";
    let mut child = spawn(tmp.path(), Some(bind), None);
    let base = format!("http://{bind}");
    wait_health(&base).await;

    // Rendered markdown from the plain HTTP endpoint.
    let client = reqwest::Client::new();
    let http_body = client
        .get(format!("{base}/v1/context?agent=jarvis&user=tony"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    // Rendered text from the `load_session_context` MCP tool for the same scope.
    let transport = StreamableHttpClientTransport::with_client(
        reqwest::Client::default(),
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")),
    );
    let service = ().serve(transport).await.expect("mcp initialize");
    let mut arguments = serde_json::Map::new();
    arguments.insert("agent".into(), serde_json::json!("jarvis"));
    arguments.insert("user".into(), serde_json::json!("tony"));
    let result = service
        .call_tool(CallToolRequestParams::new("load_session_context").with_arguments(arguments))
        .await
        .unwrap();
    let tool_rendered = result.structured_content.as_ref().unwrap()["rendered"]
        .as_str()
        .unwrap()
        .to_string();
    service.cancel().await.unwrap();

    assert_eq!(
        http_body, tool_rendered,
        "the /v1/context body must be byte-identical to the tool result"
    );

    child.kill().await.unwrap();
}
