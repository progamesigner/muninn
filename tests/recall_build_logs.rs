//! The eager index build must be observable: an INFO line when it starts, and an
//! INFO line on completion carrying the backend, the scope count, and the elapsed
//! duration — so operators can watch build time trend against their startup-probe
//! budget.
//!
//! This lives in its own test binary on purpose. `tracing` caches callsite
//! interest process-wide, so a sibling test hitting the same `info!` callsite with
//! no subscriber installed can cache it away before the capturing subscriber is in
//! place. One test per process makes the capture deterministic.

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

/// One captured event: its message, plus the names of its other fields.
type Captured = (String, Vec<String>);

#[derive(Clone, Default)]
struct CapturedEvents(Arc<Mutex<Vec<Captured>>>);

#[derive(Default)]
struct EventFields {
    message: String,
    fields: Vec<String>,
}

impl tracing::field::Visit for EventFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(field.name().to_string());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(field.name().to_string());
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturedEvents {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = EventFields::default();
        event.record(&mut visitor);
        self.0
            .lock()
            .unwrap()
            .push((visitor.message, visitor.fields));
    }
}

#[test]
fn the_build_logs_a_start_line_and_a_ready_line_carrying_elapsed() {
    let captured = CapturedEvents::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(captured.clone()))
        .expect("install the capturing subscriber");

    let tmp = TempDir::new().unwrap();
    tmp.child("Agents/jarvis.tony/topics/rust.jarvis.tony.md")
        .write_str("The borrow checker enforces ownership.")
        .unwrap();
    tmp.child("Agents/jarvis.sam/topics/notes.jarvis.sam.md")
        .write_str("Sam's notes live here.")
        .unwrap();
    tmp.child("Actions/release.md")
        .write_str("The release process is documented.")
        .unwrap();

    let resolver = PathResolver::new(
        tmp.path().canonicalize().unwrap(),
        camino::Utf8PathBuf::from("Agents"),
        Scheme::parse("<agent>.<user>").unwrap(),
    );
    let storage = Arc::new(Storage::new(resolver, true, false, &[]));
    let config = RecallConfig {
        backend: RecallBackendKind::Simple,
        watch_debounce: Duration::from_secs(3600),
        regex_scan_byte_cap: usize::MAX,
        max_resident_scopes: 256,
        freshness: Duration::from_secs(3600),
    };
    let engine = RecallEngine::new(storage, config).unwrap();

    engine.warm();
    // The second call short-circuits on `built`, so it must stay silent.
    engine.warm();
    assert!(engine.is_ready());

    let events = captured.0.lock().unwrap();
    let starts = events
        .iter()
        .filter(|(msg, _)| msg == "recall index build started")
        .count();
    assert_eq!(starts, 1, "expected exactly one start line: {events:?}");

    let ready: Vec<&Vec<String>> = events
        .iter()
        .filter(|(msg, _)| msg == "recall index ready")
        .map(|(_, fields)| fields)
        .collect();
    assert_eq!(
        ready.len(),
        1,
        "expected exactly one ready line: {events:?}"
    );
    for expected in ["backend", "scopes", "elapsed"] {
        assert!(
            ready[0].iter().any(|name| name == expected),
            "ready line is missing the '{expected}' field: {:?}",
            ready[0]
        );
    }

    // Ordering: the start line precedes the ready line.
    let start_at = events
        .iter()
        .position(|(msg, _)| msg == "recall index build started")
        .unwrap();
    let ready_at = events
        .iter()
        .position(|(msg, _)| msg == "recall index ready")
        .unwrap();
    assert!(
        start_at < ready_at,
        "start line must precede ready: {events:?}"
    );
}
