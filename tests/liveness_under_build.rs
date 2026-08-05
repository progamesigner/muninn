//! Liveness must survive a blocked tool call.
//!
//! Spawns the real binary against a vault large enough that the cold recall index
//! build takes a visible amount of time, fires a `recall_memory_notes` call that
//! blocks behind that build, and hammers `GET /healthz` throughout. Before tool
//! dispatch moved to the blocking pool this starved the probe on a
//! single-worker runtime — which is how a healthy pod got killed in production.
//!
//! Its own test binary: the vault is big enough that sharing a process with the
//! rest of the HTTP suite would just make everything slower, and the runtime is
//! pinned to two workers to model the container CPU limit that triggered the bug.

use std::process::Stdio;
use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::service::ServiceExt;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::json;
use tokio::process::{Child, Command};

use assert_fs::TempDir;
use assert_fs::prelude::*;

const BIND: &str = "127.0.0.1:18691";
/// Vault size. Tuned so the cold build is long enough to still be running when
/// the first probes land, without making the test slow.
const SCOPES: usize = 6;
const NOTES_PER_SCOPE: usize = 900;
/// The probe budget from the Kubernetes liveness scenario.
const PROBE_BUDGET: Duration = Duration::from_secs(1);

fn spawn(root: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_muninn"))
        .env("MUNINN_ROOT_DIR", root)
        .env("MUNINN_TRANSPORT", "http")
        .env("MUNINN_HTTP_BIND", BIND)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn muninn")
}

/// Write a vault whose notes all share a searchable term.
fn big_vault() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let body = "The borrow checker enforces ownership. ".repeat(12);
    for s in 0..SCOPES {
        let scope = format!("jarvis.user{s}");
        for n in 0..NOTES_PER_SCOPE {
            tmp.child(format!("Agents/{scope}/topics/note-{n}.{scope}.md"))
                .write_str(&format!("---\nkind: topic\n---\n\n# Note {n}\n\n{body}"))
                .unwrap();
        }
    }
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthz_stays_responsive_while_a_recall_blocks_on_the_cold_build() {
    let tmp = big_vault();
    let mut child = spawn(tmp.path());
    let base = format!("http://{BIND}");
    let client = reqwest::Client::builder()
        .timeout(PROBE_BUDGET)
        .build()
        .unwrap();

    // Wait for the listener, not for readiness — the build is still running.
    let mut listening = false;
    for _ in 0..200 {
        if let Ok(resp) = client.get(format!("{base}/healthz")).send().await
            && resp.status().is_success()
        {
            listening = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(listening, "server never started listening on {BIND}");

    // Fire the recall while the build is (almost certainly) still in flight; it
    // will block on the engine lock until the build completes.
    let recall = tokio::spawn({
        let base = base.clone();
        async move {
            let transport = StreamableHttpClientTransport::with_client(
                reqwest::Client::new(),
                StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")),
            );
            let service = ().serve(transport).await.expect("mcp initialize");
            let result = service
                .call_tool(
                    CallToolRequestParams::new("recall_memory_notes").with_arguments(
                        json!({ "agent": "jarvis", "user": "user0", "query": "borrow" })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                )
                .await
                .expect("tools/call round-trip");
            let _ = service.cancel().await;
            result
        }
    });

    // Hammer liveness until the recall returns. Every probe must answer inside the
    // budget — the client's own timeout turns a stalled probe into an error.
    let mut probes = 0_u32;
    let mut probes_before_ready = 0_u32;
    let mut worst = Duration::ZERO;
    let mut saw_indexing = false;
    while !recall.is_finished() {
        let started = Instant::now();
        let resp = client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap_or_else(|err| panic!("liveness probe #{probes} failed: {err}"));
        let elapsed = started.elapsed();
        assert!(resp.status().is_success(), "probe #{probes} was not 200");
        assert!(
            elapsed < PROBE_BUDGET,
            "probe #{probes} took {elapsed:?}, over the {PROBE_BUDGET:?} budget"
        );
        worst = worst.max(elapsed);
        probes += 1;

        // `/readyz` reporting "indexing" proves the build was still running while
        // liveness was being served — the exact overlap the requirement names.
        if !saw_indexing
            && let Ok(ready) = client.get(format!("{base}/readyz")).send().await
            && ready.status().as_u16() == 503
        {
            saw_indexing = true;
            probes_before_ready = probes;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let result = recall.await.expect("recall task");
    assert_ne!(
        result.is_error,
        Some(true),
        "the blocked recall should still succeed: {result:?}"
    );
    assert!(
        saw_indexing,
        "the build finished before any probe landed ({probes} probes, worst {worst:?}); \
         raise SCOPES/NOTES_PER_SCOPE so the overlap is actually exercised"
    );
    assert!(
        probes_before_ready > 0,
        "no probe was answered while the index was still building"
    );

    child.kill().await.unwrap();
}
