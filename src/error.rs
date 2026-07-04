//! The crate-wide error type and its mapping onto the MCP boundary.
//!
//! Internal layers return [`MuninnError`]. At the tool boundary it is converted
//! into either a structured MCP tool result (domain errors the agent should see and
//! react to) or a protocol-level [`rmcp::ErrorData`] (argument/schema violations).
//!
//! Raw OS error strings are never propagated to the MCP-facing message — IO failures
//! carry only an [`std::io::ErrorKind`] and a static context label.

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, Content};
use serde_json::json;

/// The structured discriminator attached to every error surfaced to a client.
///
/// The string form is the stable `code` value referenced throughout the specs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    PathEscapesRoot,
    PathNotPermitted,
    WriteDenied,
    MissingScope,
    ScopeDenied,
    NotFound,
    DestinationExists,
    EditSearchNotFound,
    EditSearchAmbiguous,
    InvalidArgument,
    Unsupported,
    Io,
    Config,
    Transport,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::PathEscapesRoot => "path_escapes_root",
            ErrorCode::PathNotPermitted => "path_not_permitted",
            ErrorCode::WriteDenied => "write_denied",
            ErrorCode::MissingScope => "missing_scope",
            ErrorCode::ScopeDenied => "scope_denied",
            ErrorCode::NotFound => "not_found",
            ErrorCode::DestinationExists => "destination_exists",
            ErrorCode::EditSearchNotFound => "edit_search_not_found",
            ErrorCode::EditSearchAmbiguous => "edit_search_ambiguous",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Io => "io",
            ErrorCode::Config => "config",
            ErrorCode::Transport => "transport",
        }
    }
}

/// The crate-wide error type.
///
/// Every variant maps to exactly one [`ErrorCode`]. Messages are written for an
/// LLM reader and reference the *virtual* path the client supplied, never the
/// resolved physical path.
#[derive(Debug, thiserror::Error)]
pub enum MuninnError {
    #[error("path '{virtual_path}' escapes the configured vault root")]
    PathEscapesRoot { virtual_path: String },

    #[error("path '{virtual_path}' is not permitted under the active policy or visibility filters")]
    PathNotPermitted { virtual_path: String },

    /// A generic write/edit/delete targeted an agents-folder root-level core file.
    /// Reported with the `path_not_permitted` code; the message names the wrapper
    /// tool that owns the file.
    #[error("path '{virtual_path}' is a reserved root-level core file; use {wrapper} instead")]
    RootPathReserved {
        virtual_path: String,
        wrapper: &'static str,
    },

    #[error("write denied: path '{virtual_path}' is in a read-only region under the active policy")]
    WriteDenied { virtual_path: String },

    /// A shared note linked to a target in the caller's own scope; persisting its
    /// suffixed form would leak the scope's existence. Reported with the
    /// `write_denied` code; the message names the offending link target.
    #[error(
        "write denied: link target '{target}' resolves into your own scope and cannot be \
         referenced from a shared note (it would leak your scope's existence)"
    )]
    CrossScopeLink { target: String },

    #[error("missing required scope key '{key}'")]
    MissingScope { key: String },

    /// The presented token's grant does not cover a requested scope key. The
    /// message names the key but never enumerates the valid grants.
    #[error(
        "scope denied: this token's grant does not cover the requested value of scope key '{key}'"
    )]
    ScopeDenied { key: String },

    #[error("not found: '{virtual_path}'")]
    NotFound { virtual_path: String },

    /// A rename targeted a destination at which a note already exists. The agent
    /// should pick another name rather than treat this as a bad argument.
    #[error("destination '{virtual_path}' already exists; pick another name")]
    DestinationExists { virtual_path: String },

    #[error("edit failed: the search string was not found in the target file")]
    EditSearchNotFound,

    #[error(
        "edit failed: the search string occurs {count} times; retry with a longer, unique snippet"
    )]
    EditSearchAmbiguous { count: usize },

    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },

    /// A request asked for a capability the active backend does not provide
    /// (e.g. frontmatter property filters against the `simple` recall backend).
    #[error("unsupported: {message}")]
    Unsupported { message: String },

    /// An IO failure. Only the kind and a static context label are retained so no
    /// raw OS error string ever reaches the client.
    #[error("io error while {context}")]
    Io {
        kind: std::io::ErrorKind,
        context: &'static str,
    },

    #[error("configuration error: {message}")]
    Config { message: String },

    #[error("transport error: {message}")]
    Transport { message: String },
}

impl MuninnError {
    /// The structured code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            MuninnError::PathEscapesRoot { .. } => ErrorCode::PathEscapesRoot,
            MuninnError::PathNotPermitted { .. } => ErrorCode::PathNotPermitted,
            MuninnError::RootPathReserved { .. } => ErrorCode::PathNotPermitted,
            MuninnError::WriteDenied { .. } => ErrorCode::WriteDenied,
            MuninnError::CrossScopeLink { .. } => ErrorCode::WriteDenied,
            MuninnError::MissingScope { .. } => ErrorCode::MissingScope,
            MuninnError::ScopeDenied { .. } => ErrorCode::ScopeDenied,
            MuninnError::NotFound { .. } => ErrorCode::NotFound,
            MuninnError::DestinationExists { .. } => ErrorCode::DestinationExists,
            MuninnError::EditSearchNotFound => ErrorCode::EditSearchNotFound,
            MuninnError::EditSearchAmbiguous { .. } => ErrorCode::EditSearchAmbiguous,
            MuninnError::InvalidArgument { .. } => ErrorCode::InvalidArgument,
            MuninnError::Unsupported { .. } => ErrorCode::Unsupported,
            MuninnError::Io { .. } => ErrorCode::Io,
            MuninnError::Config { .. } => ErrorCode::Config,
            MuninnError::Transport { .. } => ErrorCode::Transport,
        }
    }

    /// Construct an [`MuninnError::Io`] from a raw IO error, retaining only its
    /// kind and a caller-supplied static context label. The raw OS message is
    /// dropped here so it can never leak past the boundary.
    pub fn io(context: &'static str, err: &std::io::Error) -> Self {
        MuninnError::Io {
            kind: err.kind(),
            context,
        }
    }

    /// Render this error as a structured MCP tool result (`is_error = true`).
    ///
    /// The human-readable text is the [`std::fmt::Display`] output; the structured
    /// content carries the stable `code` discriminator for orchestrators.
    pub fn into_tool_result(self) -> CallToolResult {
        let code = self.code().as_str();
        let message = self.to_string();
        let mut result = CallToolResult::error(vec![Content::text(message.clone())]);
        result.structured_content = Some(json!({ "code": code, "message": message }));
        result
    }
}

/// Map an internal error to a protocol-level MCP error.
///
/// Used for argument/schema violations that should be reported as JSON-RPC errors
/// rather than as tool results. The structured `code` is preserved in the `data`
/// field so clients can branch on it programmatically.
impl From<MuninnError> for McpError {
    fn from(err: MuninnError) -> Self {
        let code = err.code();
        let data = Some(json!({ "code": code.as_str() }));
        match code {
            ErrorCode::MissingScope
            | ErrorCode::ScopeDenied
            | ErrorCode::InvalidArgument
            | ErrorCode::Unsupported
            | ErrorCode::DestinationExists
            | ErrorCode::EditSearchNotFound
            | ErrorCode::EditSearchAmbiguous => McpError::invalid_params(err.to_string(), data),
            ErrorCode::NotFound => McpError::resource_not_found(err.to_string(), data),
            _ => McpError::internal_error(err.to_string(), data),
        }
    }
}

/// A convenient crate-wide result alias.
pub type Result<T> = std::result::Result<T, MuninnError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every error code string matches the value referenced in the specs.
    #[test]
    fn code_strings_are_stable() {
        assert_eq!(ErrorCode::PathEscapesRoot.as_str(), "path_escapes_root");
        assert_eq!(ErrorCode::PathNotPermitted.as_str(), "path_not_permitted");
        assert_eq!(ErrorCode::WriteDenied.as_str(), "write_denied");
        assert_eq!(ErrorCode::MissingScope.as_str(), "missing_scope");
        assert_eq!(ErrorCode::ScopeDenied.as_str(), "scope_denied");
        assert_eq!(ErrorCode::NotFound.as_str(), "not_found");
        assert_eq!(ErrorCode::DestinationExists.as_str(), "destination_exists");
        assert_eq!(
            ErrorCode::EditSearchNotFound.as_str(),
            "edit_search_not_found"
        );
        assert_eq!(
            ErrorCode::EditSearchAmbiguous.as_str(),
            "edit_search_ambiguous"
        );
        assert_eq!(ErrorCode::InvalidArgument.as_str(), "invalid_argument");
        assert_eq!(ErrorCode::Unsupported.as_str(), "unsupported");
        assert_eq!(ErrorCode::Io.as_str(), "io");
        assert_eq!(ErrorCode::Config.as_str(), "config");
        assert_eq!(ErrorCode::Transport.as_str(), "transport");
    }

    /// Task 7.3: no variant leaks a raw OS error string into the MCP-facing message.
    #[test]
    fn io_error_does_not_leak_os_string() {
        let raw = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "os error 13: Permission denied (/etc/shadow)",
        );
        let err = MuninnError::io("reading note", &raw);
        let message = err.to_string();
        assert!(!message.contains("os error 13"));
        assert!(!message.contains("/etc/shadow"));
        assert!(!message.contains("Permission denied"));
        assert_eq!(message, "io error while reading note");

        let result = err.into_tool_result();
        let rendered = serde_json::to_string(&result.structured_content).unwrap();
        assert!(!rendered.contains("/etc/shadow"));
        assert!(rendered.contains("\"code\":\"io\""));
    }

    #[test]
    fn tool_result_carries_code_and_is_error() {
        let err = MuninnError::NotFound {
            virtual_path: "PERSONA.md".to_string(),
        };
        let result = err.into_tool_result();
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["code"], "not_found");
        assert!(
            structured["message"]
                .as_str()
                .unwrap()
                .contains("PERSONA.md")
        );
    }

    /// `scope_denied` names the offending key in both the message and the
    /// structured code, and never enumerates the grant set.
    #[test]
    fn scope_denied_names_key_only() {
        let err = MuninnError::ScopeDenied {
            key: "agent".to_string(),
        };
        assert_eq!(err.code().as_str(), "scope_denied");
        assert!(err.to_string().contains("'agent'"));

        let result = err.into_tool_result();
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["code"], "scope_denied");
        assert!(structured["message"].as_str().unwrap().contains("'agent'"));
    }

    /// The virtual path supplied by the client appears in the message; the
    /// resolved physical path never does (callers pass only the virtual path).
    #[test]
    fn not_found_message_uses_virtual_path() {
        let err = MuninnError::NotFound {
            virtual_path: "tasks/plan.md".to_string(),
        };
        assert!(err.to_string().contains("tasks/plan.md"));
    }
}
