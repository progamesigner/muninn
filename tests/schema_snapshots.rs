//! Snapshot tests for the tool input schemas exposed via `tools/list`, across
//! several representative VFS schemes (task 8.11).
//!
//! These lock the JSON shape of the scheme-derived scope fields merged with
//! each tool's own fields. Run `cargo insta review` to accept intentional changes.

use std::sync::Arc;
use std::time::Duration;

use assert_fs::TempDir;
use camino::Utf8PathBuf;
use chrono_tz::Tz;
use muninn::config::{RecallBackendKind, RecallConfig};
use muninn::path::PathResolver;
use muninn::policy::Policy;
use muninn::recall::RecallEngine;
use muninn::scheme::Scheme;
use muninn::storage::Storage;
use muninn::tools::Toolbox;
use serde_json::{Value, json};

/// The `tools/list` schemas for a given scheme, as a name → inputSchema map.
fn schemas_for(scheme: &str) -> Value {
    let tmp = TempDir::new().unwrap();
    let resolver = PathResolver::new(
        tmp.path().canonicalize().unwrap(),
        Utf8PathBuf::from("Agents"),
        Scheme::parse(scheme).unwrap(),
    );
    let storage = Storage::new(resolver, true, false, &[]);
    let toolbox = Toolbox::new(
        storage,
        Policy::Namespaced,
        Tz::UTC,
        tmp.path().join("AGENT_SESSION_CONTEXT.md"),
        tmp.path().join("AGENT_SESSION_BOOTSTRAP.md"),
        tmp.path().join("AGENT_MEMORY_LAYOUT.md"),
        None,
    );

    let mut map = serde_json::Map::new();
    for tool in toolbox.list_tools() {
        map.insert(
            tool.name.to_string(),
            json!({
                "description": tool.description,
                "input_schema": &*tool.input_schema,
            }),
        );
    }
    Value::Object(map)
}

/// The `recall_memory_notes` schema for a given scheme. The tool is advertised
/// only when a recall engine is present, so this builds one (simple backend).
fn recall_schema_for(scheme: &str) -> Value {
    let tmp = TempDir::new().unwrap();
    let mk = || {
        PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            Utf8PathBuf::from("Agents"),
            Scheme::parse(scheme).unwrap(),
        )
    };
    let storage = Storage::new(mk(), true, false, &[]);
    let config = RecallConfig {
        backend: RecallBackendKind::Simple,
        watch_debounce: Duration::ZERO,
        regex_scan_byte_cap: usize::MAX,
        max_resident_scopes: 256,
        freshness: Duration::ZERO,
        index_dir: None,
    };
    let recall =
        RecallEngine::new(Arc::new(Storage::new(mk(), true, false, &[])), config).map(Arc::new);
    let toolbox = Toolbox::new(
        storage,
        Policy::Namespaced,
        Tz::UTC,
        tmp.path().join("AGENT_SESSION_CONTEXT.md"),
        tmp.path().join("AGENT_SESSION_BOOTSTRAP.md"),
        tmp.path().join("AGENT_MEMORY_LAYOUT.md"),
        recall,
    );
    let tool = toolbox
        .list_tools()
        .into_iter()
        .find(|t| t.name == "recall_memory_notes")
        .expect("recall tool advertised when the engine is present");
    json!({
        "description": tool.description,
        "input_schema": &*tool.input_schema,
    })
}

#[test]
fn schema_empty_scheme() {
    insta::assert_json_snapshot!("schema_empty_scheme", schemas_for(""));
}

#[test]
fn schema_agent_scheme() {
    insta::assert_json_snapshot!("schema_agent_scheme", schemas_for("<agent>"));
}

#[test]
fn schema_agent_user_scheme() {
    insta::assert_json_snapshot!("schema_agent_user_scheme", schemas_for("<agent>.<user>"));
}

#[test]
fn schema_team_agent_env_user_scheme() {
    insta::assert_json_snapshot!(
        "schema_team_agent_env_user_scheme",
        schemas_for("<team>.<agent>.<env>.<user>")
    );
}

#[test]
fn schema_recall_tool() {
    insta::assert_json_snapshot!("schema_recall_tool", recall_schema_for("<agent>.<user>"));
}
