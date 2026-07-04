//! Criterion benchmarks for the recall engine: eager cold-start build, warm
//! query latency per backend, and the synchronous own-write index update.
//!
//! Numbers are informational (no hard timing asserts). Run with
//! `cargo bench --bench recall` and, for the tantivy scenario,
//! `cargo bench --bench recall --features recall-tantivy`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use muninn::config::{RecallBackendKind, RecallConfig};
use muninn::path::{PathResolver, VirtualPath};
use muninn::policy::Region;
use muninn::recall::{RecallEngine, RecallQuery};
use muninn::scheme::Scheme;
use muninn::storage::Storage;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const NOTES: usize = 10_000;
const SCOPES: usize = 10;
const BOTH: &[Region] = &[Region::InsideAgentsFolder, Region::OutsideAgentsFolder];

fn scope_name(s: usize) -> String {
    format!("jarvis.user{s}")
}

/// Deterministic lorem-like body for note `i`. Every tenth note seeds the
/// keyword `borrow`, so query benches scan a fixed, predictable hit subset.
fn body(i: usize) -> String {
    let keyword = if i.is_multiple_of(10) {
        "borrow"
    } else {
        "lend"
    };
    format!(
        "Note {i}. Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
         The {keyword} checker enforces ownership across function boundaries. \
         Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua, \
         ut enim ad minim veniam ({i} grains of sand)."
    )
}

/// Generate the synthetic vault: `NOTES` notes spread evenly over `SCOPES`
/// scopes, mirroring the unit-test fixture layout.
fn build_vault() -> TempDir {
    let tmp = TempDir::new().unwrap();
    for i in 0..NOTES {
        let scope = scope_name(i % SCOPES);
        tmp.child(format!("Agents/{scope}/topics/note-{i}.{scope}.md"))
            .write_str(&body(i))
            .unwrap();
    }
    tmp
}

fn resolver(vault: &TempDir) -> PathResolver {
    PathResolver::new(
        vault.path().canonicalize().unwrap(),
        camino::Utf8PathBuf::from("Agents"),
        Scheme::parse("<agent>.<user>").unwrap(),
    )
}

/// Build an engine over `vault` for the given backend. The freshness window is
/// long enough that queries never trigger a stat-diff reconcile mid-bench, so
/// the timed routines isolate the backend scan.
fn build_engine(vault: &TempDir, backend: RecallBackendKind) -> RecallEngine {
    let storage = Arc::new(Storage::new(resolver(vault), true, false, &[]));
    let config = RecallConfig {
        backend,
        watch_debounce: Duration::from_secs(3600),
        regex_scan_byte_cap: usize::MAX,
        max_resident_scopes: 256,
        freshness: Duration::from_secs(3600),
    };
    RecallEngine::new(storage, config).unwrap()
}

fn query(text: &str) -> RecallQuery {
    RecallQuery {
        text: Some(text.to_string()),
        regex: None,
        filters: Vec::new(),
        path_prefix: None,
        limit: 100,
        offset: 0,
        modified_after: None,
        modified_before: None,
    }
}

fn cold_start(c: &mut Criterion) {
    let vault = build_vault();
    let mut group = c.benchmark_group("recall/cold_start");
    // Each iteration reads all 10 000 files; keep the run in the low minutes.
    group.sample_size(10);
    group.bench_function("10k_notes", |b| {
        b.iter_batched(
            || build_engine(&vault, RecallBackendKind::Simple),
            |engine| {
                engine.warm();
                black_box(engine)
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

fn warm_query(c: &mut Criterion) {
    let vault = build_vault();
    let scope = scope_name(0);
    let mut group = c.benchmark_group("recall/warm_query");

    let simple = build_engine(&vault, RecallBackendKind::Simple);
    simple.warm();
    group.bench_function("simple", |b| {
        b.iter(|| black_box(simple.recall(&scope, BOTH, &query("borrow")).unwrap()))
    });

    #[cfg(feature = "recall-tantivy")]
    {
        let tantivy = build_engine(&vault, RecallBackendKind::Tantivy);
        tantivy.warm();
        group.bench_function("tantivy", |b| {
            b.iter(|| black_box(tantivy.recall(&scope, BOTH, &query("borrow")).unwrap()))
        });
    }

    group.finish();
}

fn own_write_update(c: &mut Criterion) {
    let vault = build_vault();
    let scope = scope_name(0);
    let engine = build_engine(&vault, RecallBackendKind::Simple);
    engine.warm();
    // Note 0 lives in scope 0; `on_write` re-reads it and upserts in place,
    // which is exactly the server's synchronous post-write index update.
    let vpath = VirtualPath::new("Agents/topics/note-0.md").unwrap();
    let physical = resolver(&vault).resolve(&scope, &vpath).unwrap();
    c.bench_function("recall/own_write_update", |b| {
        b.iter(|| engine.on_write(&scope, Region::InsideAgentsFolder, black_box(&physical)))
    });
}

criterion_group!(benches, cold_start, warm_query, own_write_update);
criterion_main!(benches);
