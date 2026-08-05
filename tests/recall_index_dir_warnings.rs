//! The two places a configured index directory must speak up instead of failing
//! silently: a backend that cannot use it, and a persisted index that cannot be
//! trusted. Both are `WARN` lines an operator reads to understand why startup did
//! what it did — and in the corruption case, the proof that the process rebuilt
//! rather than exited or served stale results.
//!
//! Like `recall_build_logs.rs`, this lives in its own test binary and uses a single
//! `#[test]`: `tracing` caches callsite interest process-wide, so a sibling test
//! reaching the same callsite before the capturing subscriber is installed can
//! cache it away.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muninn::config::{RecallBackendKind, RecallConfig};
use muninn::path::PathResolver;
use muninn::recall::RecallEngine;
use muninn::scheme::Scheme;
use muninn::storage::Storage;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use tracing_subscriber::layer::SubscriberExt as _;

#[derive(Clone, Default)]
struct CapturedMessages(Arc<Mutex<Vec<String>>>);

#[derive(Default)]
struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturedMessages {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = Message::default();
        event.record(&mut visitor);
        self.0.lock().unwrap().push(visitor.0);
    }
}

/// A vault with one scoped note and one shared note.
fn vault_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
        .write_str("The borrow checker enforces ownership.")
        .unwrap();
    tmp.child("Actions/release.md")
        .write_str("The release process is documented.")
        .unwrap();
    tmp
}

fn engine(
    vault: &std::path::Path,
    backend: RecallBackendKind,
    index_dir: Option<&std::path::Path>,
) -> RecallEngine {
    let resolver = PathResolver::new(
        vault.canonicalize().unwrap(),
        camino::Utf8PathBuf::from("Agents"),
        Scheme::parse("<agent>.<user>").unwrap(),
    );
    let storage = Arc::new(Storage::new(resolver, true, false, &[]));
    let config = RecallConfig {
        backend,
        watch_debounce: Duration::from_secs(3600),
        regex_scan_byte_cap: usize::MAX,
        max_resident_scopes: 256,
        freshness: Duration::from_secs(3600),
        index_dir: index_dir.map(|p| p.to_path_buf()),
    };
    RecallEngine::new(storage, config).unwrap()
}

/// Every regular file under `root`, recursively.
fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn an_unusable_index_directory_warns_and_the_server_carries_on() {
    let captured = CapturedMessages::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(captured.clone()))
        .expect("install the capturing subscriber");

    // 1. A backend with no on-disk form ignores the setting, with a warning.
    let vault = vault_fixture();
    let index = TempDir::new().unwrap();
    let simple = engine(vault.path(), RecallBackendKind::Simple, Some(index.path()));
    simple.warm();
    assert!(simple.is_ready());
    assert!(
        walk_files(index.path()).is_empty(),
        "the simple backend must write nothing to the index directory"
    );
    let messages = captured.0.lock().unwrap().clone();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("MUNINN_RECALL_INDEX_DIR") && m.contains("not tantivy")),
        "expected an ignore warning naming the variable: {messages:?}"
    );

    // 2. A persisted index that cannot be read is discarded with a warning, and
    //    startup continues on a rebuild.
    #[cfg(feature = "recall-tantivy")]
    {
        let tantivy_vault = vault_fixture();
        let index = TempDir::new().unwrap();
        let cold = engine(
            tantivy_vault.path(),
            RecallBackendKind::Tantivy,
            Some(index.path()),
        );
        cold.warm();
        drop(cold);

        for file in walk_files(index.path()) {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name == "meta.json" || name == ".managed.json" || name == "region.id" {
                continue;
            }
            std::fs::write(&file, b"").unwrap();
        }

        let before = captured.0.lock().unwrap().len();
        let warm = engine(
            tantivy_vault.path(),
            RecallBackendKind::Tantivy,
            Some(index.path()),
        );
        warm.warm();
        assert!(warm.is_ready(), "the process must not exit or hang");
        assert!(
            warm.ingested_count() > 0,
            "the vault must have been re-read after the index was discarded"
        );
        let messages: Vec<String> = captured.0.lock().unwrap()[before..].to_vec();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("persisted recall index could not be opened")),
            "expected a corruption warning: {messages:?}"
        );
    }
}
