//! In-process integration tests for `[[wikilink]]` and markdown-link rewriting,
//! covering the scenarios in `specs/wikilink-references/spec.md`. The tests drive
//! [`Toolbox`] directly — the same path the MCP `call_tool` handler uses — so the
//! transform is exercised end to end alongside scope resolution and policy gating.

use std::sync::Arc;
use std::time::Duration;

use assert_fs::TempDir;
use camino::Utf8PathBuf;
use chrono_tz::Tz;
use muninn::MuninnError;
use muninn::backlink::BacklinkEngine;
use muninn::config::{Grant, RecallBackendKind, RecallConfig};
use muninn::path::PathResolver;
use muninn::policy::Policy;
use muninn::scheme::Scheme;
use muninn::storage::Storage;
use muninn::tools::{TemplatePaths, Toolbox};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};

fn toolbox(tmp: &TempDir, policy: Policy) -> Toolbox {
    let mk = || {
        PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        )
    };
    let storage = Storage::new(mk(), true, false, &[]);
    // Freshness zero: every backlink query reconciles by stat-diff, matching
    // the pre-index always-scan behavior for externally-written fixtures.
    let backlinks = {
        let config = RecallConfig {
            backend: RecallBackendKind::Simple,
            watch_debounce: Duration::ZERO,
            regex_scan_byte_cap: usize::MAX,
            max_resident_scopes: 256,
            freshness: Duration::ZERO,
            index_dir: None,
        };
        Arc::new(BacklinkEngine::new(
            Arc::new(Storage::new(mk(), true, false, &[])),
            policy.list_visible_regions(false),
            &config,
        ))
    };
    Toolbox::new(
        storage,
        policy,
        Tz::UTC,
        TemplatePaths {
            session_context: tmp.path().join("AGENT_SESSION_CONTEXT.md"),
            session_bootstrap: tmp.path().join("AGENT_SESSION_BOOTSTRAP.md"),
            memory_layout: tmp.path().join("AGENT_MEMORY_LAYOUT.md"),
        },
        None,
        backlinks,
    )
}

fn call(tb: &Toolbox, name: &str, args: Value) -> Result<CallToolResult, MuninnError> {
    let obj = args.as_object().unwrap().clone();
    tb.call(name, &obj, &Grant::AllScopes)
        .expect("tool name must be known")
}

fn read_content(tb: &Toolbox, path: &str) -> String {
    let res = call(
        tb,
        "read_memory_note",
        json!({"agent":"jarvis","user":"tony","path":path}),
    )
    .expect("read ok");
    res.structured_content.unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Create the caller's own-scope note `Agents/topics/rust.md` so links resolve.
fn seed_rust(tb: &Toolbox) {
    call(
        tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/topics/rust.md","content":"the rust note"}),
    )
    .unwrap();
}

#[test]
fn write_expands_own_scope_link_on_disk_and_read_strips_it() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    seed_rust(&tb);
    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/index.md","content":"see [[rust]]"}),
    )
    .unwrap();

    // On disk the link carries the caller's suffix so Obsidian can resolve it.
    let physical = tmp
        .path()
        .join("Agents/jarvis.tony/notes/index.jarvis.tony.md");
    let raw = std::fs::read_to_string(&physical).unwrap();
    assert_eq!(raw, "see [[rust.jarvis.tony]]");

    // Read presents only the clean shortest name.
    assert_eq!(read_content(&tb, "Agents/notes/index.md"), "see [[rust]]");
}

/// A `.md`-suffixed wikilink target resolves and persists exactly like the
/// extension-less spelling: suffixed on disk, clean on read.
#[test]
fn md_suffixed_wikilink_persists_extensionless_and_reads_back_clean() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    seed_rust(&tb);
    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/index.md","content":"see [[rust.md|the Rust note]]"}),
    )
    .unwrap();

    // On disk the link carries the caller's suffix and drops the `.md`.
    let physical = tmp
        .path()
        .join("Agents/jarvis.tony/notes/index.jarvis.tony.md");
    let raw = std::fs::read_to_string(&physical).unwrap();
    assert_eq!(raw, "see [[rust.jarvis.tony|the Rust note]]");

    // Read presents the clean extension-less form.
    assert_eq!(
        read_content(&tb, "Agents/notes/index.md"),
        "see [[rust|the Rust note]]"
    );
}

#[test]
fn edit_search_matches_clean_link_form() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    seed_rust(&tb);
    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/index.md","content":"see [[rust]] here"}),
    )
    .unwrap();

    // The agent searches with the clean link form; it must match the suffixed
    // form stored on disk.
    call(
        &tb,
        "edit_memory_note",
        json!({
            "agent":"jarvis","user":"tony","path":"Agents/notes/index.md",
            "search_string":"see [[rust]] here","replace_string":"now [[rust]] there"
        }),
    )
    .expect("edit must match the stored suffixed link");

    assert_eq!(
        read_content(&tb, "Agents/notes/index.md"),
        "now [[rust]] there"
    );
}

#[test]
fn shared_file_linking_to_scoped_note_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Readwrite);
    seed_rust(&tb);
    let res = call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Actions/release.md","content":"ship [[rust]]"}),
    );
    match res {
        Err(e) => assert_eq!(e.code().as_str(), "write_denied"),
        Ok(_) => panic!("expected write_denied for shared->scoped link"),
    }
    // The file was not created.
    assert!(!tmp.path().join("Actions/release.md").exists());
}

#[test]
fn dangling_link_is_preserved() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/index.md","content":"see [[ghost]]"}),
    )
    .unwrap();
    assert_eq!(read_content(&tb, "Agents/notes/index.md"), "see [[ghost]]");
}

#[test]
fn core_file_links_expand_on_disk_and_strip_in_session_context() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    seed_rust(&tb);
    // A MEMORY.md index linking to an own-scope note via evolve_core_persona.
    call(
        &tb,
        "evolve_core_persona",
        json!({"agent":"jarvis","user":"tony","updates":[{"which":"memory","content":"- [[rust]] — the rust note"}]}),
    )
    .unwrap();

    // On disk the core file carries the suffixed link (Obsidian-resolvable).
    let physical = tmp.path().join("Agents/jarvis.tony/MEMORY.jarvis.tony.md");
    assert_eq!(
        std::fs::read_to_string(&physical).unwrap(),
        "- [[rust.jarvis.tony]] — the rust note"
    );

    // load_session_context renders the clean shortest name.
    let rendered = call(
        &tb,
        "load_session_context",
        json!({"agent":"jarvis","user":"tony"}),
    )
    .unwrap()
    .structured_content
    .unwrap()["rendered"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(rendered.contains("- [[rust]] — the rust note"));
    assert!(!rendered.contains("rust.jarvis.tony"));
}

#[test]
fn heartbeat_links_expand_on_disk() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    seed_rust(&tb);
    call(
        &tb,
        "update_task_heartbeat",
        json!({"agent":"jarvis","user":"tony","content":"working on [[rust]]"}),
    )
    .unwrap();
    let physical = tmp
        .path()
        .join("Agents/jarvis.tony/HEARTBEAT.jarvis.tony.md");
    assert_eq!(
        std::fs::read_to_string(&physical).unwrap(),
        "working on [[rust.jarvis.tony]]"
    );
    // Reading it back via read_memory_note strips the suffix.
    assert_eq!(
        read_content(&tb, "Agents/HEARTBEAT.md"),
        "working on [[rust]]"
    );
}

#[test]
fn property_round_trip_matches_body_round_trip() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    seed_rust(&tb);
    // The same frontmatter link, once via a whole-file write...
    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/a.md",
               "content":"---\nrelated: \"[[rust]]\"\n---\nbody\n"}),
    )
    .unwrap();
    // ...and once via the property tool.
    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/b.md","content":"body\n"}),
    )
    .unwrap();
    call(
        &tb,
        "update_note_properties",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/b.md",
               "properties_json": json!({ "related": "[[rust]]" }).to_string()}),
    )
    .unwrap();

    // Both persist the suffixed Obsidian-resolvable form.
    for f in ["a", "b"] {
        let raw = std::fs::read_to_string(
            tmp.path()
                .join(format!("Agents/jarvis.tony/notes/{f}.jarvis.tony.md")),
        )
        .unwrap();
        assert!(raw.contains("[[rust.jarvis.tony]]"), "{f}: {raw}");
    }
    // Both the content view and the property view present the clean form.
    for path in ["Agents/notes/a.md", "Agents/notes/b.md"] {
        let content = read_content(&tb, path);
        assert!(content.contains("[[rust]]"), "{path}: {content}");
        assert!(!content.contains("jarvis.tony"), "{path}: {content}");
        let props = call(
            &tb,
            "read_note_properties",
            json!({"agent":"jarvis","user":"tony","path":path}),
        )
        .unwrap()
        .structured_content
        .unwrap();
        assert_eq!(
            props["properties"],
            json!({ "related": "[[rust]]" }),
            "{path}"
        );
    }
}

#[test]
fn property_only_link_counts_toward_backlinks() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    seed_rust(&tb);
    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/memo.md","content":"no body links"}),
    )
    .unwrap();
    call(
        &tb,
        "update_note_properties",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/memo.md",
               "properties_json": json!({ "related": "[[rust]]" }).to_string()}),
    )
    .unwrap();

    let body = call(
        &tb,
        "read_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/topics/rust.md","backlinks":true}),
    )
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(body["backlinks"], json!(["Agents/notes/memo.md"]));
}

#[test]
fn shared_link_from_own_scope_note_stays_clean() {
    let tmp = TempDir::new().unwrap();
    let tb = toolbox(&tmp, Policy::Namespaced);
    // Seed a shared note outside the agents folder.
    let actions = tmp.path().join("Actions");
    std::fs::create_dir_all(&actions).unwrap();
    std::fs::write(actions.join("release.md"), "shared").unwrap();

    call(
        &tb,
        "write_memory_note",
        json!({"agent":"jarvis","user":"tony","path":"Agents/notes/index.md","content":"see [[release]]"}),
    )
    .unwrap();

    // Persisted without a suffix (shared notes resolve for every scope).
    let physical = tmp
        .path()
        .join("Agents/jarvis.tony/notes/index.jarvis.tony.md");
    assert_eq!(
        std::fs::read_to_string(&physical).unwrap(),
        "see [[release]]"
    );
    assert_eq!(
        read_content(&tb, "Agents/notes/index.md"),
        "see [[release]]"
    );
}
