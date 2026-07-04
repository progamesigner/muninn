//! Tracing initialisation.
//!
//! The subscriber's writer is **always** `std::io::stderr()`. Under the stdio
//! transport this is mandatory — stdout carries the JSON-RPC frames and any stray
//! byte would corrupt the protocol stream (see `specs/mcp-server/spec.md`, "Stdio
//! output discipline"). Under HTTP it is merely the conventional choice.

use tracing_subscriber::EnvFilter;

/// Install the global tracing subscriber, filtering by `filter_directive`
/// (the resolved `MUNINN_LOG` value) and writing exclusively to stderr.
///
/// Returns an error if a global subscriber was already installed (e.g. when
/// called twice in a test process).
pub fn init(filter_directive: &str) -> Result<(), String> {
    let filter = EnvFilter::try_new(filter_directive)
        .map_err(|e| format!("invalid log filter {filter_directive:?}: {e}"))?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|e| e.to_string())
}
