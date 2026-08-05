//! The MCP server: a [`rmcp::ServerHandler`] that advertises the tools, the
//! `session-context` resource, and the `session-context` prompt, and dispatches
//! requests to the shared [`crate::tools::Toolbox`].
//!
//! Domain errors from `tools/call` (policy, not-found, edit preconditions, …) are
//! surfaced as structured *tool results* (`is_error = true` with a `code` field)
//! so the agent can read and react to them. Only an unknown tool name becomes a
//! protocol-level `method not found` error. The resource and prompt surfaces,
//! which have no structured-result channel, map domain errors to protocol errors.

use std::collections::BTreeMap;
use std::sync::Arc;

use rmcp::ServerHandler;
use rmcp::model::{
    AnnotateAble, CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult,
    Implementation, InitializeRequestParams, InitializeResult, ListPromptsResult,
    ListResourceTemplatesResult, ListToolsResult, PaginatedRequestParams, Prompt, PromptArgument,
    PromptMessage, PromptMessageRole, ProtocolVersion, RawResourceTemplate,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, model::JsonObject};

use crate::config::{Config, Grant};
use crate::error::MuninnError;
use crate::storage::Storage;
use crate::tools::Toolbox;

/// The URI prefix for the session-context resource (note the trailing slash; the
/// per-scope segments follow it).
const SESSION_CONTEXT_URI_PREFIX: &str = "muninn://session-context/";
/// The shared name of the session-context resource and prompt.
const SESSION_CONTEXT_NAME: &str = "session-context";
/// The URI prefix and name for the lean session-bootstrap resource.
const SESSION_BOOTSTRAP_URI_PREFIX: &str = "muninn://session-bootstrap/";
const SESSION_BOOTSTRAP_NAME: &str = "session-bootstrap";
/// The URI prefix and name for the layout resource.
const SESSION_LAYOUT_URI_PREFIX: &str = "muninn://session-layout/";
const SESSION_LAYOUT_NAME: &str = "session-layout";

/// The MCP server handler. Cheap to clone — the shared [`Toolbox`] lives behind an
/// `Arc`, so the HTTP transport's per-session factory hands out lightweight
/// clones that all front the same storage layer and locks.
#[derive(Clone)]
pub struct MuninnServer {
    toolbox: Arc<Toolbox>,
}

impl MuninnServer {
    /// Build a server from a fully-resolved [`Config`].
    pub fn new(config: &Config) -> MuninnServer {
        let storage = Storage::new(
            config.resolver(),
            config.honor_ignore_files,
            config.include_hidden,
            &config.include_hidden_globs,
        );
        // The recall engine reads through its own `Storage` view (it never writes,
        // so it needs no share of the write-lock map). `None` when recall is off.
        let recall = {
            let engine_storage = Arc::new(Storage::new(
                config.resolver(),
                config.honor_ignore_files,
                config.include_hidden,
                &config.include_hidden_globs,
            ));
            crate::recall::RecallEngine::new(engine_storage, config.recall.clone()).map(Arc::new)
        };
        let toolbox = Toolbox::new(
            storage,
            config.policy,
            config.timezone,
            config.session_context_template_file.clone(),
            config.session_bootstrap_template_file.clone(),
            config.memory_layout_template_file.clone(),
            recall,
        );
        MuninnServer {
            toolbox: Arc::new(toolbox),
        }
    }

    /// `true` when the server is ready to serve recall traffic — backs `GET
    /// /readyz`. When recall is disabled the server is ready as soon as the
    /// process is up; otherwise readiness waits for the eager index build.
    pub fn recall_ready(&self) -> bool {
        self.toolbox
            .recall_engine()
            .is_none_or(|engine| engine.is_ready())
    }

    /// Start the recall filesystem watcher and kick off the eager index build in
    /// the background, so liveness stays up and `GET /readyz` flips green only once
    /// every index is built. A no-op when recall is disabled.
    pub fn spawn_recall_warmup(&self) {
        if let Some(engine) = self.toolbox.recall_engine() {
            engine.start_watcher();
            tokio::task::spawn_blocking(move || engine.warm());
        }
    }

    /// The scheme's placeholder idents, in order — the scope keys every surface
    /// requires. Exposed so the HTTP `GET /v1/context` handler can bind query
    /// parameters to the scope without reaching into the private toolbox.
    pub fn scheme_placeholders(&self) -> Vec<String> {
        self.toolbox.scheme_placeholders()
    }

    /// Render the session-context for a validated scope map, checked against the
    /// caller's grant. `kind` selects the full `Context` or lean `Bootstrap`
    /// render. Exposed so the HTTP `GET /v1/context` and `GET /v1/bootstrap`
    /// handlers can reuse the same renderer (and authorization) as the MCP
    /// resources.
    pub fn render_session_context(
        &self,
        scope: &BTreeMap<String, String>,
        grant: &Grant,
        kind: crate::session_context::RenderKind,
    ) -> Result<crate::session_context::SessionContext, MuninnError> {
        self.toolbox.render_session_context(scope, grant, kind)
    }

    /// Render the layout document for a validated scope map, checked against the
    /// caller's grant. Exposed so the HTTP `GET /v1/layout` handler can reuse the
    /// same renderer (and authorization) as the `session-layout` resource.
    pub fn render_layout(
        &self,
        scope: &BTreeMap<String, String>,
        grant: &Grant,
    ) -> Result<String, MuninnError> {
        self.toolbox.render_layout(scope, grant)
    }

    /// The `muninn://<prefix>/{k1}/{k2}/…` URI template for the active scheme;
    /// the params follow the scheme's placeholders in order.
    fn uri_template_for(&self, prefix: &str) -> String {
        let params: Vec<String> = self
            .toolbox
            .scheme_placeholders()
            .iter()
            .map(|k| format!("{{{k}}}"))
            .collect();
        format!("{prefix}{}", params.join("/"))
    }

    /// Map the scheme's placeholders onto the path segments of a concrete resource
    /// URI carrying the given prefix, returning the scope map. Errors if the URI
    /// does not carry exactly one segment per placeholder.
    fn scope_from_uri(
        &self,
        prefix: &str,
        uri: &str,
    ) -> Result<BTreeMap<String, String>, McpError> {
        let rest = uri.strip_prefix(prefix).ok_or_else(|| {
            McpError::invalid_params(format!("unknown resource URI '{uri}'"), None)
        })?;
        let placeholders = self.toolbox.scheme_placeholders();
        let segments: Vec<&str> = if rest.is_empty() {
            Vec::new()
        } else {
            rest.split('/').collect()
        };
        if segments.len() != placeholders.len() {
            return Err(McpError::invalid_params(
                format!(
                    "resource URI '{uri}' has {} scope segment(s), expected {}",
                    segments.len(),
                    placeholders.len()
                ),
                None,
            ));
        }
        Ok(placeholders
            .into_iter()
            .zip(segments)
            .map(|(k, v)| (k, v.to_string()))
            .collect())
    }

    /// Read the scheme's placeholders out of a prompt's `arguments` object.
    fn scope_from_prompt_args(
        &self,
        arguments: &Option<JsonObject>,
    ) -> Result<BTreeMap<String, String>, McpError> {
        let mut scope = BTreeMap::new();
        let args = arguments.as_ref();
        for key in self.toolbox.scheme_placeholders() {
            match args.and_then(|a| a.get(&key)) {
                Some(serde_json::Value::String(s)) => {
                    scope.insert(key, s.clone());
                }
                Some(_) => {
                    return Err(McpError::invalid_params(
                        format!("prompt argument '{key}' must be a string"),
                        None,
                    ));
                }
                None => {
                    return Err(McpError::invalid_params(
                        format!("missing required prompt argument '{key}'"),
                        None,
                    ));
                }
            }
        }
        Ok(scope)
    }
}

/// Map a domain error onto a protocol error for the resource/prompt surfaces.
/// The structured `code` rides in the `data` field so clients can branch on it.
fn to_mcp_error(err: MuninnError) -> McpError {
    let data = Some(serde_json::json!({ "code": err.code().as_str() }));
    McpError::invalid_params(err.to_string(), data)
}

/// Map a blocking-task join failure — a panicking handler, or a cancelled task
/// during shutdown — onto a protocol-level internal error. Keeps the "a panic
/// does not corrupt the transport" guarantee now that dispatch runs off-runtime.
fn to_join_error(err: tokio::task::JoinError) -> McpError {
    McpError::internal_error(format!("handler failed: {err}"), None)
}

/// Which render a `muninn://…` resource URI selects. Resolved from the URI
/// prefix on the async side so the blocking closure carries only owned data.
enum ResourceRender {
    Context,
    Bootstrap,
    Layout,
}

/// The scope grant the HTTP auth middleware resolved for this request, read
/// back out of the propagated `http::request::Parts` extension. Absent parts or
/// grant — the stdio transport, or HTTP with no authentication configured —
/// means every scope is permitted.
fn request_grant(context: &RequestContext<RoleServer>) -> Grant {
    #[cfg(feature = "transport-http")]
    {
        if let Some(grant) = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<Grant>())
        {
            return grant.clone();
        }
    }
    #[cfg(not(feature = "transport-http"))]
    let _ = context;
    Grant::AllScopes
}

impl ServerHandler for MuninnServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so it cannot be built with a struct
        // expression here; start from its `Default` and override the rest.
        // `Implementation::from_build_env()` is *not* good enough for `server_info`:
        // its `env!("CARGO_CRATE_NAME")` is baked in where that line is compiled
        // (inside the `rmcp` crate itself), so it always resolves to `"rmcp"`
        // regardless of which binary links it. Build our own from this crate's
        // own build-time env instead.
        let mut info = ServerInfo::default();
        info.server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        info.instructions = Some(
            "Durable, namespaced markdown memory for agents. Every tool call must \
             carry the scope keys defined by the server's VFS scheme; paths are \
             virtual and relative to the vault root. The `session-context` resource \
             and prompt render the per-scope bootstrap."
                .to_string(),
        );
        info
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // Stateless `serve_directly` bypasses rmcp's normal initialize handshake
        // negotiation, so the default handler would advertise our latest
        // protocol version (`ProtocolVersion::LATEST`) regardless of what the
        // client asked for. Clients pinned to an older revision then reject the
        // handshake (e.g. Raycast: "Unsupported protocol version"). Restore the
        // negotiation here: echo the client's requested version whenever we
        // recognize it, otherwise keep our default.
        let mut info = self.get_info();
        if ProtocolVersion::KNOWN_VERSIONS.contains(&request.protocol_version) {
            info.protocol_version = request.protocol_version.clone();
        }
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        Ok(info)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.toolbox.list_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args: JsonObject = request.arguments.unwrap_or_default();
        let grant = request_grant(&context);
        let toolbox = Arc::clone(&self.toolbox);
        let name = request.name;
        // The toolbox is fully synchronous: vault IO, the recall engine's mutex,
        // and index commits all block. Dispatching it on the blocking pool keeps
        // long calls (a cold index build, a large reconcile) off the runtime's
        // workers, so `GET /healthz` and every other future stay schedulable.
        let (name, outcome) = tokio::task::spawn_blocking(move || {
            let outcome = toolbox.call(&name, &args, &grant);
            (name, outcome)
        })
        .await
        .map_err(to_join_error)?;
        match outcome {
            Some(Ok(result)) => Ok(result),
            Some(Err(err)) => Ok(err.into_tool_result()),
            None => Err(McpError::invalid_params(
                format!("unknown tool '{name}'"),
                None,
            )),
        }
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let templates = vec![
            RawResourceTemplate {
                uri_template: self.uri_template_for(SESSION_CONTEXT_URI_PREFIX),
                name: SESSION_CONTEXT_NAME.to_string(),
                title: Some("Session context".to_string()),
                description: Some(
                    "The full rendered session context for a scope: the foundational \
                     files woven into the configured template."
                        .to_string(),
                ),
                mime_type: Some("text/markdown".to_string()),
                icons: None,
            }
            .no_annotation(),
            RawResourceTemplate {
                uri_template: self.uri_template_for(SESSION_BOOTSTRAP_URI_PREFIX),
                name: SESSION_BOOTSTRAP_NAME.to_string(),
                title: Some("Session bootstrap".to_string()),
                description: Some(
                    "The lean session bootstrap for a scope: scope, persona, rules, and \
                     pointers to the full context and the layout."
                        .to_string(),
                ),
                mime_type: Some("text/markdown".to_string()),
                icons: None,
            }
            .no_annotation(),
            RawResourceTemplate {
                uri_template: self.uri_template_for(SESSION_LAYOUT_URI_PREFIX),
                name: SESSION_LAYOUT_NAME.to_string(),
                title: Some("Session layout".to_string()),
                description: Some(
                    "The vault layout and conventions guidance for a scope.".to_string(),
                ),
                mime_type: Some("text/markdown".to_string()),
                icons: None,
            }
            .no_annotation(),
        ];
        Ok(ListResourceTemplatesResult::with_all_items(templates))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let grant = request_grant(&context);
        // Dispatch by URI prefix to the matching render. Longest/most-specific is
        // unambiguous: the three prefixes share no common tail. URI parsing is
        // cheap and stays inline; only the vault-reading render moves off-runtime.
        let (render, scope) = if request.uri.starts_with(SESSION_BOOTSTRAP_URI_PREFIX) {
            (
                ResourceRender::Bootstrap,
                self.scope_from_uri(SESSION_BOOTSTRAP_URI_PREFIX, &request.uri)?,
            )
        } else if request.uri.starts_with(SESSION_LAYOUT_URI_PREFIX) {
            (
                ResourceRender::Layout,
                self.scope_from_uri(SESSION_LAYOUT_URI_PREFIX, &request.uri)?,
            )
        } else {
            (
                ResourceRender::Context,
                self.scope_from_uri(SESSION_CONTEXT_URI_PREFIX, &request.uri)?,
            )
        };
        let toolbox = Arc::clone(&self.toolbox);
        let rendered = tokio::task::spawn_blocking(move || match render {
            ResourceRender::Bootstrap => toolbox
                .render_session_context(
                    &scope,
                    &grant,
                    crate::session_context::RenderKind::Bootstrap,
                )
                .map(|sc| sc.rendered),
            ResourceRender::Layout => toolbox.render_layout(&scope, &grant),
            ResourceRender::Context => toolbox
                .render_session_context(&scope, &grant, crate::session_context::RenderKind::Context)
                .map(|sc| sc.rendered),
        })
        .await
        .map_err(to_join_error)?
        .map_err(to_mcp_error)?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            rendered,
            request.uri,
        )]))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let arguments: Vec<PromptArgument> = self
            .toolbox
            .scheme_placeholders()
            .into_iter()
            .map(|key| {
                let description = format!("Scope key '{key}' identifying the caller.");
                PromptArgument::new(key)
                    .with_description(description)
                    .with_required(true)
            })
            .collect();
        let prompt = Prompt::new(
            SESSION_CONTEXT_NAME,
            Some("Render the per-scope session-context bootstrap."),
            Some(arguments),
        );
        Ok(ListPromptsResult::with_all_items(vec![prompt]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if request.name != SESSION_CONTEXT_NAME {
            return Err(McpError::invalid_params(
                format!("unknown prompt '{}'", request.name),
                None,
            ));
        }
        let scope = self.scope_from_prompt_args(&request.arguments)?;
        let grant = request_grant(&context);
        let toolbox = Arc::clone(&self.toolbox);
        let sc = tokio::task::spawn_blocking(move || {
            toolbox.render_session_context(
                &scope,
                &grant,
                crate::session_context::RenderKind::Context,
            )
        })
        .await
        .map_err(to_join_error)?
        .map_err(to_mcp_error)?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            PromptMessageRole::User,
            sc.rendered,
        )])
        .with_description("Session-context bootstrap."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handler that panics must surface as a well-formed protocol error rather
    /// than tear down the transport. Now that dispatch runs on the blocking pool,
    /// the panic reaches the handler as a [`tokio::task::JoinError`] — this drives
    /// a real panicking blocking task through the same `spawn_blocking(…).await`
    /// seam the three handlers use and checks the mapping. (The handlers
    /// themselves are not called directly: `RequestContext<RoleServer>` is only
    /// constructible inside rmcp's dispatch, so the end-to-end path is covered by
    /// the transport integration tests.)
    #[tokio::test]
    async fn a_panicking_blocking_task_maps_to_an_internal_error() {
        let joined = tokio::task::spawn_blocking(|| -> Option<CallToolResult> {
            panic!("handler exploded");
        })
        .await;
        let err = to_join_error(joined.expect_err("the panicking task must not join cleanly"));

        assert_eq!(err.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert!(
            err.message.starts_with("handler failed: "),
            "unexpected message: {}",
            err.message
        );
        // Well-formed on the wire: serializes to a JSON-RPC error object.
        let wire = serde_json::to_value(&err).expect("error serializes");
        assert_eq!(wire["code"], -32603);
        assert!(wire["message"].is_string());

        // The panic stayed inside the blocking pool — the runtime is still usable.
        assert_eq!(
            tokio::task::spawn_blocking(|| 7)
                .await
                .expect("still alive"),
            7
        );
    }

    /// A cancelled dispatch (the runtime shutting down under a blocked call) takes
    /// the same mapping rather than panicking the handler.
    #[tokio::test]
    async fn a_cancelled_blocking_task_maps_to_an_internal_error() {
        let handle = tokio::task::spawn(std::future::pending::<()>());
        handle.abort();
        let err = to_join_error(
            handle
                .await
                .expect_err("aborted task must not join cleanly"),
        );
        assert_eq!(err.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
    }
}
