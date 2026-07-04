//! The stdio transport.
//!
//! Reads JSON-RPC frames from stdin and writes responses to stdout; all logging
//! goes to stderr (configured in [`crate::telemetry`]). On `SIGINT`/`SIGTERM` the
//! in-flight requests are allowed to drain and the process exits zero.

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::mcp::MuninnServer;

/// Serve over stdio until stdin closes or a termination signal arrives.
pub async fn serve(server: MuninnServer) -> anyhow::Result<()> {
    let running = server.serve(stdio()).await?;
    let cancel = running.cancellation_token();

    tokio::select! {
        // The transport ended on its own (stdin closed or peer disconnected).
        quit = running.waiting() => {
            let reason = quit?;
            tracing::info!(?reason, "stdio transport closed");
        }
        // A termination signal: cancel the service, which drains in-flight work.
        _ = super::shutdown_signal() => {
            tracing::info!("received termination signal; shutting down");
            cancel.cancel();
        }
    }
    Ok(())
}
