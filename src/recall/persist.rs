//! On-disk layout for persistent recall indexes.
//!
//! Everything a persisted index depends on beyond the vault's own bytes lives in
//! its *fingerprint*: an index-format constant plus a hash of the configuration
//! that shapes what gets ingested. The fingerprint is a directory layer —
//! `<index_dir>/<fingerprint>/<region>/` — so a mismatch is handled by "this
//! directory isn't ours, remove it" rather than per-region marker checks, and a
//! half-migrated state is impossible.
//!
//! Region directory names are hashed, because a rendered scope may contain path
//! separators, unicode, or more bytes than a filename allows. A hash is not
//! injective, and two scopes sharing a directory would mean one scope reading the
//! other's notes — so each region directory also carries a plain-text identity
//! file that is verified on open. A mismatch is treated as corruption (wipe and
//! rebuild), which keeps the cross-scope isolation structural.

use std::path::{Path, PathBuf};

use crate::storage::Storage;

use super::IndexRegion;

/// The persisted index layout/schema version. Bump on any change to the tantivy
/// schema or to how note content is ingested; the bump invalidates every
/// previously persisted index.
pub(crate) const INDEX_FORMAT_VERSION: u32 = 1;

/// The file inside a region directory naming the region it belongs to.
const REGION_ID_FILE: &str = "region.id";

/// The persisted-index root for one engine: `<index_dir>/<fingerprint>`.
pub(crate) struct PersistRoot {
    root: PathBuf,
}

impl PersistRoot {
    /// Compute the fingerprint root under `index_dir` for this storage
    /// configuration, and remove any sibling fingerprint directory left behind by
    /// a different format version or configuration.
    pub(crate) fn new(index_dir: &Path, storage: &Storage) -> PersistRoot {
        let root = index_dir.join(fingerprint(storage));
        prune_stale_fingerprints(index_dir, &root);
        PersistRoot { root }
    }

    /// The directory holding one region's index, created if absent. `None` when
    /// the directory cannot be created — the caller then stays in memory.
    pub(crate) fn region_dir(&self, region: &IndexRegion) -> Option<PathBuf> {
        let dir = self.root.join(region_dir_name(region));
        if let Err(err) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                dir = %dir.display(),
                %err,
                "could not create the persistent recall index directory; this region stays in memory"
            );
            return None;
        }
        match verify_region_identity(&dir, region) {
            Ok(()) => Some(dir),
            Err(err) => {
                // A hash collision, or a directory written by an older layout:
                // discard it rather than serve another region's notes.
                tracing::warn!(
                    dir = %dir.display(),
                    %err,
                    "persistent recall index directory belongs to a different region; rebuilding it"
                );
                if std::fs::remove_dir_all(&dir).is_err() {
                    return None;
                }
                std::fs::create_dir_all(&dir).ok()?;
                verify_region_identity(&dir, region).ok()?;
                Some(dir)
            }
        }
    }
}

/// Delete the index inside a region directory, keeping the directory itself and its
/// identity marker. The marker must survive: a rebuild that dropped it would leave
/// an unclaimed directory holding one region's documents, which a colliding region
/// could then adopt as its own.
pub(crate) fn wipe_region_index(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == REGION_ID_FILE {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// The canonical identity string of a region, as stored in `region.id`.
fn region_identity(region: &IndexRegion) -> String {
    match region {
        IndexRegion::Shared => "shared".to_string(),
        IndexRegion::Scoped(scope) => format!("scope:{scope}"),
    }
}

/// Write the region identity if the directory is new, or check it if not.
fn verify_region_identity(dir: &Path, region: &IndexRegion) -> std::io::Result<()> {
    let marker = dir.join(REGION_ID_FILE);
    let want = region_identity(region);
    match std::fs::read_to_string(&marker) {
        Ok(found) if found == want => Ok(()),
        Ok(found) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("index directory holds region {found:?}, expected {want:?}"),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // A fresh (or wiped) directory: claim it. Empty of an index, so a
            // torn write here is corrected by the next open.
            std::fs::write(&marker, want.as_bytes())
        }
        Err(err) => Err(err),
    }
}

/// The directory name for one region under the fingerprint root.
fn region_dir_name(region: &IndexRegion) -> String {
    match region {
        IndexRegion::Shared => "shared".to_string(),
        IndexRegion::Scoped(scope) => format!("scope-{:016x}", fnv1a64(scope.as_bytes())),
    }
}

/// The fingerprint directory name: the format version plus a hash of the
/// configuration that decides which files are ingested and how their content is
/// transformed. Tuning knobs (freshness, eviction caps) are deliberately absent —
/// they do not change ingested content — and so is the tantivy crate version,
/// which a schema-mismatch error already catches at open.
fn fingerprint(storage: &Storage) -> String {
    let resolver = storage.resolver();
    let canonical = format!(
        "v{INDEX_FORMAT_VERSION}\nscheme={scheme}\nagents_dir={agents}\nhonor_ignore_files={ignore}\ninclude_hidden={hidden}\ninclude_hidden_globs={globs}\n",
        scheme = resolver.scheme(),
        agents = resolver.agents_dir(),
        ignore = storage.honor_ignore_files(),
        hidden = storage.include_hidden(),
        globs = storage.include_hidden_glob_patterns().join(","),
    );
    format!(
        "fp-{INDEX_FORMAT_VERSION}-{:016x}",
        fnv1a64(canonical.as_bytes())
    )
}

/// Remove every fingerprint directory under `index_dir` other than `keep`, so
/// disk use does not accumulate across format or configuration changes. Only
/// entries that look like fingerprint directories are touched — an operator's
/// unrelated files in a shared directory are left alone.
fn prune_stale_fingerprints(index_dir: &Path, keep: &Path) {
    let entries = match std::fs::read_dir(index_dir) {
        Ok(entries) => entries,
        // Not created yet (or unreadable): nothing to prune. A real problem here
        // surfaces when the region directory is created.
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let is_fingerprint = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("fp-"));
        if !is_fingerprint {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => tracing::info!(
                dir = %path.display(),
                "removed a stale persistent recall index (different format version or configuration)"
            ),
            Err(err) => tracing::warn!(
                dir = %path.display(),
                %err,
                "could not remove a stale persistent recall index directory"
            ),
        }
    }
}

/// FNV-1a, 64-bit. Hand-rolled rather than `DefaultHasher` because the digest
/// lands in directory names: it must stay identical across Rust releases, or every
/// upgrade would silently invalidate every persisted index.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::PathResolver;
    use crate::scheme::Scheme;

    use assert_fs::TempDir;

    fn storage(scheme: &str, agents: &str, hidden: bool) -> Storage {
        let tmp = TempDir::new().unwrap();
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            camino::Utf8PathBuf::from(agents),
            Scheme::parse(scheme).unwrap(),
        );
        // The temp dir is dropped here; nothing in these tests touches the vault.
        Storage::new(resolver, true, hidden, &[])
    }

    #[test]
    fn fnv1a_matches_the_published_vectors() {
        // The reference FNV-1a 64-bit digests, so a refactor cannot silently
        // change every persisted index's directory name.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn fingerprint_changes_with_ingestion_shaping_config() {
        let base = fingerprint(&storage("<agent>.<user>", "Agents", false));
        assert_ne!(base, fingerprint(&storage("<agent>", "Agents", false)));
        assert_ne!(
            base,
            fingerprint(&storage("<agent>.<user>", "Vault", false))
        );
        assert_ne!(
            base,
            fingerprint(&storage("<agent>.<user>", "Agents", true))
        );
        // Stable for identical configuration.
        assert_eq!(
            base,
            fingerprint(&storage("<agent>.<user>", "Agents", false))
        );
        assert!(base.starts_with("fp-1-"));
    }

    #[test]
    fn region_dirs_are_distinct_and_identity_checked() {
        let tmp = TempDir::new().unwrap();
        let storage = storage("<agent>.<user>", "Agents", false);
        let root = PersistRoot::new(tmp.path(), &storage);

        let shared = root.region_dir(&IndexRegion::Shared).unwrap();
        let tony = root
            .region_dir(&IndexRegion::Scoped("jarvis.tony".into()))
            .unwrap();
        let sam = root
            .region_dir(&IndexRegion::Scoped("jarvis.sam".into()))
            .unwrap();
        assert_ne!(shared, tony);
        assert_ne!(tony, sam);
        // Reopening the same region is stable and keeps its contents.
        std::fs::write(tony.join("payload"), b"x").unwrap();
        let tony_again = root
            .region_dir(&IndexRegion::Scoped("jarvis.tony".into()))
            .unwrap();
        assert_eq!(tony, tony_again);
        assert!(tony_again.join("payload").exists());

        // A directory claimed by another region is wiped rather than reused.
        std::fs::write(tony.join(REGION_ID_FILE), b"scope:jarvis.sam").unwrap();
        let reclaimed = root
            .region_dir(&IndexRegion::Scoped("jarvis.tony".into()))
            .unwrap();
        assert_eq!(reclaimed, tony);
        assert!(!reclaimed.join("payload").exists());
        assert_eq!(
            std::fs::read_to_string(reclaimed.join(REGION_ID_FILE)).unwrap(),
            "scope:jarvis.tony"
        );
    }

    #[test]
    fn wiping_a_region_index_keeps_its_identity_marker() {
        let tmp = TempDir::new().unwrap();
        let storage = storage("<agent>.<user>", "Agents", false);
        let root = PersistRoot::new(tmp.path(), &storage);
        let dir = root
            .region_dir(&IndexRegion::Scoped("jarvis.tony".into()))
            .unwrap();
        std::fs::write(dir.join("segment.store"), b"payload").unwrap();
        std::fs::create_dir_all(dir.join("nested")).unwrap();

        wipe_region_index(&dir).unwrap();

        assert!(!dir.join("segment.store").exists());
        assert!(!dir.join("nested").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(REGION_ID_FILE)).unwrap(),
            "scope:jarvis.tony",
            "a rebuilt directory must stay claimed by its own region"
        );
    }

    #[test]
    fn stale_fingerprint_directories_are_pruned() {
        let tmp = TempDir::new().unwrap();
        let stale = tmp.path().join("fp-1-deadbeefdeadbeef");
        std::fs::create_dir_all(stale.join("shared")).unwrap();
        // An unrelated entry an operator may have put in a shared directory.
        let unrelated = tmp.path().join("operator-notes");
        std::fs::create_dir_all(&unrelated).unwrap();

        let storage = storage("<agent>.<user>", "Agents", false);
        let root = PersistRoot::new(tmp.path(), &storage);
        assert!(!stale.exists(), "the stale fingerprint dir must be removed");
        assert!(unrelated.exists(), "unrelated entries must be left alone");

        // The current root survives its own prune on a second construction.
        let dir = root.region_dir(&IndexRegion::Shared).unwrap();
        std::fs::write(dir.join("payload"), b"x").unwrap();
        let again = PersistRoot::new(tmp.path(), &storage);
        assert!(
            again
                .region_dir(&IndexRegion::Shared)
                .unwrap()
                .join("payload")
                .exists()
        );
    }
}
