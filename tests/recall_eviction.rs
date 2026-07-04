//! Eviction-bound integration test: with `max_resident_scopes` smaller than the
//! number of scopes, sequential cross-scope queries never leave more than the
//! cap resident after a query completes.

use std::sync::Arc;
use std::time::Duration;

use muninn::config::{RecallBackendKind, RecallConfig};
use muninn::path::PathResolver;
use muninn::policy::Region;
use muninn::recall::{RecallEngine, RecallQuery};
use muninn::scheme::Scheme;
use muninn::storage::Storage;

use assert_fs::TempDir;
use assert_fs::prelude::*;

const BOTH: &[Region] = &[Region::InsideAgentsFolder, Region::OutsideAgentsFolder];
const SCOPES: usize = 10;
const NOTES_PER_SCOPE: usize = 4;
const MAX_RESIDENT: usize = 3;

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

#[test]
fn resident_scopes_never_exceed_the_cap() {
    let tmp = TempDir::new().unwrap();
    for s in 0..SCOPES {
        let scope = format!("jarvis.user{s}");
        for n in 0..NOTES_PER_SCOPE {
            tmp.child(format!("Agents/{scope}/topics/note-{n}.{scope}.md"))
                .write_str(&format!("Note {n} of scope {s}: the borrow checker."))
                .unwrap();
        }
    }

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
        max_resident_scopes: MAX_RESIDENT,
        freshness: Duration::from_secs(3600),
    };
    let engine = RecallEngine::new(storage, config).unwrap();
    engine.warm();

    for s in 0..SCOPES {
        let scope = format!("jarvis.user{s}");
        let results = engine.recall(&scope, BOTH, &query("borrow")).unwrap();
        assert_eq!(
            results.hits.len(),
            NOTES_PER_SCOPE,
            "scope {scope} should see exactly its own notes"
        );
        assert!(
            engine.resident_scope_count() <= MAX_RESIDENT,
            "resident scopes {} exceed the cap {MAX_RESIDENT} after querying {scope}",
            engine.resident_scope_count()
        );
    }
}
