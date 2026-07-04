//! Transport selection and serving.
//!
//! The stdio transport is always available; the HTTP transport is gated behind
//! the `transport-http` Cargo feature (on by default). [`serve`] dispatches to the
//! transport selected in the [`Config`].

use crate::config::{Config, Transport};
use crate::mcp::MuninnServer;

pub mod stdio;

#[cfg(feature = "transport-http")]
pub mod http;

/// Resolve when the process receives `SIGINT` (Ctrl-C) or, on Unix, `SIGTERM`.
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Serve the MCP server on the transport configured in `config`, blocking until
/// the server terminates (signal, closed stdin, or fatal error).
pub async fn serve(config: &Config, server: MuninnServer) -> anyhow::Result<()> {
    // Start the recall watcher and kick off the eager index build in the
    // background, so liveness stays up while `GET /readyz` reports not-ready until
    // every index is built.
    server.spawn_recall_warmup();
    match &config.transport {
        Transport::Stdio => stdio::serve(server).await,
        #[cfg(feature = "transport-http")]
        Transport::Http {
            bind,
            bearer,
            tokens,
            allowed_hosts,
        } => {
            http::serve(
                *bind,
                bearer.clone(),
                tokens.clone(),
                allowed_hosts.clone(),
                server,
            )
            .await
        }
        #[cfg(not(feature = "transport-http"))]
        Transport::Http { .. } => Err(anyhow::anyhow!(
            "HTTP transport requested but this binary was built without the `transport-http` feature"
        )),
    }
}
