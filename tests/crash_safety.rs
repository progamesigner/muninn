//! Crash-safety test for the atomic-write procedure (task 6.13).
//!
//! The atomic write is temp-file → fsync → rename. The only operation that
//! mutates the target is the final rename; if the process dies before it, the
//! target must be byte-for-byte unchanged. We verify this by re-executing the
//! test binary as a child that mirrors the write up to — but not including — the
//! rename, then exits via `process::exit` (which skips destructors, so neither
//! the persist nor the temp-file cleanup runs, exactly as in a hard crash). The
//! parent then asserts the target is intact.

use std::io::Write as _;

const CHILD_ENV: &str = "MUNINN_CRASH_TARGET";
const TEST_NAME: &str = "crash_between_temp_write_and_rename_leaves_target_unchanged";

#[test]
fn crash_between_temp_write_and_rename_leaves_target_unchanged() {
    // --- child role: partial write, then "crash" before the rename ---
    if let Ok(target) = std::env::var(CHILD_ENV) {
        let target = std::path::PathBuf::from(target);
        let parent = target.parent().expect("target has a parent");
        let mut temp = tempfile::NamedTempFile::new_in(parent).expect("create temp");
        temp.write_all(b"PARTIAL WRITE THAT IS NEVER RENAMED")
            .unwrap();
        temp.as_file().sync_all().unwrap();
        // Hard exit between temp-write and rename: no persist, no temp cleanup.
        std::process::exit(0);
    }

    // --- parent role ---
    let tmp = assert_fs::TempDir::new().unwrap();
    let target = tmp.path().join("note.md");
    std::fs::write(&target, "original contents").unwrap();

    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(exe)
        .args([TEST_NAME, "--exact"])
        .env(CHILD_ENV, &target)
        .output()
        .expect("spawn crash child");
    assert!(status.status.success(), "child exited non-zero");

    // The rename never happened, so the target is byte-for-byte unchanged.
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "original contents"
    );
}
