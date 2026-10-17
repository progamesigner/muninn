//! Independent-process tests: a process-local mutex cannot satisfy these races.
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use muninn::error::ErrorCode;
use muninn::path::{PathResolver, VirtualPath};
use muninn::scheme::Scheme;
use muninn::storage::{ExpectedState, Storage, content_version};

fn storage(root: &std::path::Path) -> Storage {
    Storage::new(
        PathResolver::new(
            root.canonicalize().unwrap(),
            Utf8PathBuf::from("Agents"),
            Scheme::parse("").unwrap(),
        ),
        true,
        false,
        &[],
    )
}

#[test]
fn conditional_storage_child() {
    let Ok(root) = std::env::var("MUNINN_CAS_TEST_ROOT") else {
        return;
    };
    let root = std::path::Path::new(&root);
    let identity = std::env::var("MUNINN_CAS_TEST_ID").unwrap();
    let storage = storage(root);
    let physical = storage
        .resolver()
        .resolve("", &VirtualPath::new("Agents/AGENTS.md").unwrap())
        .unwrap();
    let version = std::env::var("MUNINN_CAS_TEST_VERSION").ok();
    let expected = version
        .as_deref()
        .map_or(ExpectedState::Absent, ExpectedState::Version);
    std::fs::write(root.join(format!("ready-{identity}")), "").unwrap();
    let result = storage.write_conditional(&physical, &identity, expected);
    std::process::exit(match result {
        Ok(_) => 0,
        Err(e) if e.code() == ErrorCode::Conflict => 10,
        Err(e) => panic!("unexpected child failure: {e}"),
    });
}

fn race(initial: Option<&str>) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Agents");
    std::fs::create_dir(&dir).unwrap();
    let target = dir.join("AGENTS.md");
    if let Some(initial) = initial {
        std::fs::write(&target, initial).unwrap();
    }
    // Hold the stable sidecar while both independent processes enter their writes.
    // The target itself is replaced, so locking its inode would be insufficient.
    let lock = std::fs::File::create(dir.join(".AGENTS.md.cas-lock")).unwrap();
    lock.lock().unwrap();
    let mut children = Vec::new();
    for identity in ["first", "second"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["conditional_storage_child", "--exact"])
            .env("MUNINN_CAS_TEST_ROOT", tmp.path())
            .env("MUNINN_CAS_TEST_ID", identity);
        if let Some(initial) = initial {
            child.env(
                "MUNINN_CAS_TEST_VERSION",
                content_version(initial.as_bytes()),
            );
        } else {
            child.env_remove("MUNINN_CAS_TEST_VERSION");
        }
        children.push(child.spawn().unwrap());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !["first", "second"]
        .iter()
        .all(|id| tmp.path().join(format!("ready-{id}")).exists())
    {
        assert!(Instant::now() < deadline, "children did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(50));
    for child in &mut children {
        assert!(
            child.try_wait().unwrap().is_none(),
            "write bypassed OS lock"
        );
    }
    lock.unlock().unwrap();
    let statuses: Vec<ExitStatus> = children.iter_mut().map(|p| p.wait().unwrap()).collect();
    assert_eq!(statuses.iter().filter(|s| s.success()).count(), 1);
    assert_eq!(statuses.iter().filter(|s| s.code() == Some(10)).count(), 1);
    let winner = statuses.iter().position(|s| s.success()).unwrap();
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        ["first", "second"][winner]
    );
    assert!(dir.join(".AGENTS.md.cas-lock").is_file());
}

#[test]
fn independent_processes_cannot_overwrite_a_newer_version() {
    race(Some("initial"));
}

#[test]
fn independent_processes_cannot_both_create_an_absent_brief() {
    race(None);
}

#[test]
fn content_versions_identify_bytes_and_conflicts_preserve_the_file() {
    assert_eq!(
        content_version(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let tmp = tempfile::tempdir().unwrap();
    let storage = storage(tmp.path());
    let physical = storage
        .resolver()
        .resolve("", &VirtualPath::new("Agents/AGENTS.md").unwrap())
        .unwrap();
    assert_eq!(
        storage
            .write_conditional(&physical, "lost", ExpectedState::Version("stale"))
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
    storage
        .write_conditional(&physical, "saved", ExpectedState::Absent)
        .unwrap();
    for expected in [ExpectedState::Absent, ExpectedState::Version("stale")] {
        assert_eq!(
            storage
                .write_conditional(&physical, "lost", expected)
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        assert_eq!(storage.read(&physical).unwrap(), "saved");
    }
    storage
        .write_conditional(
            &physical,
            "next",
            ExpectedState::Version(&content_version(b"saved")),
        )
        .unwrap();
    assert_eq!(storage.read(&physical).unwrap(), "next");
}
