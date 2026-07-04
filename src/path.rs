//! Virtual ↔ physical path resolution, containment, and the own-scope suffix
//! transform.
//!
//! ## Addressing model
//!
//! Virtual paths are **relative to the vault root**. A path is *inside the agents
//! folder* when its leading component equals the configured agents-folder name
//! (or always, when the agents folder is the vault root). Inside the agents
//! folder, with a non-empty scheme, the resolver inserts the caller's rendered
//! scope as the first directory segment beneath the agents folder AND appends it
//! to the file stem — making another scope's file structurally unaddressable.
//!
//! The specs reference both root-relative (`Agents/topics/rust.md`,
//! `Actions/release.md`) and bare (`PERSONA.md`) virtual paths. We resolve the
//! inconsistency in favour of root-relative addressing; the ergonomic wrapper
//! tools build their paths by prepending the agents-folder name, so an agent
//! never has to spell the scope segment or suffix itself.

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::MuninnError;
use crate::policy::Region;
use crate::scheme::Scheme;

/// A validated, vault-root-relative virtual path with no traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualPath(Utf8PathBuf);

/// A resolved absolute path on the host filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalPath(std::path::PathBuf);

impl VirtualPath {
    /// Validate and normalise a client-supplied virtual path.
    ///
    /// Rejects empty input, embedded NUL bytes, absolute paths, and any `..`
    /// component. Bare `.` components are dropped. The result is always relative
    /// and traversal-free.
    pub fn new(raw: &str) -> Result<VirtualPath, MuninnError> {
        if raw.is_empty() {
            return Err(MuninnError::InvalidArgument {
                message: "path must not be empty".to_string(),
            });
        }
        if raw.contains('\0') {
            return Err(MuninnError::InvalidArgument {
                message: "path must not contain NUL bytes".to_string(),
            });
        }

        let candidate = Utf8Path::new(raw);
        if candidate.is_absolute() || raw.starts_with('/') || raw.starts_with('\\') {
            return Err(MuninnError::PathEscapesRoot {
                virtual_path: raw.to_string(),
            });
        }

        let mut normalised = Utf8PathBuf::new();
        for component in candidate.components() {
            use camino::Utf8Component::*;
            match component {
                CurDir => {}
                Normal(seg) => normalised.push(seg),
                ParentDir | RootDir | Prefix(_) => {
                    return Err(MuninnError::PathEscapesRoot {
                        virtual_path: raw.to_string(),
                    });
                }
            }
        }

        if normalised.as_str().is_empty() {
            return Err(MuninnError::InvalidArgument {
                message: "path must name a file".to_string(),
            });
        }

        Ok(VirtualPath(normalised))
    }

    /// Construct from already-trusted components (internal use, e.g. listing).
    fn from_relative(path: Utf8PathBuf) -> VirtualPath {
        VirtualPath(path)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_path(&self) -> &Utf8Path {
        &self.0
    }
}

impl std::fmt::Display for VirtualPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl PhysicalPath {
    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }

    pub fn into_path_buf(self) -> std::path::PathBuf {
        self.0
    }
}

impl AsRef<std::path::Path> for PhysicalPath {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

/// Resolves virtual paths to physical paths under a fixed vault root, agents
/// folder, and scheme.
#[derive(Debug, Clone)]
pub struct PathResolver {
    /// Canonical absolute vault root.
    vault_root: std::path::PathBuf,
    /// Agents folder relative to the root. Empty means "the agents folder is the
    /// vault root".
    agents_dir: Utf8PathBuf,
    scheme: Scheme,
}

impl PathResolver {
    pub fn new(
        vault_root: std::path::PathBuf,
        agents_dir: Utf8PathBuf,
        scheme: Scheme,
    ) -> PathResolver {
        PathResolver {
            vault_root,
            agents_dir,
            scheme,
        }
    }

    pub fn vault_root(&self) -> &std::path::Path {
        &self.vault_root
    }

    pub fn agents_dir(&self) -> &Utf8Path {
        &self.agents_dir
    }

    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    /// `true` when the agents folder is the vault root itself.
    fn agents_is_root(&self) -> bool {
        self.agents_dir.as_str().is_empty()
    }

    /// Classify a virtual path as inside or outside the agents folder.
    pub fn detect_region(&self, vpath: &VirtualPath) -> Region {
        if self.agents_is_root() {
            return Region::InsideAgentsFolder;
        }
        if vpath.0.starts_with(&self.agents_dir) {
            Region::InsideAgentsFolder
        } else {
            Region::OutsideAgentsFolder
        }
    }

    /// `true` when `vpath` is inside the agents folder and names a file directly
    /// at the per-scope root — i.e. the remainder beneath the agents folder has
    /// no subfolder segment. Such root-level paths (e.g. `MEMORY.md`,
    /// `PERSONA.md`, `HEARTBEAT.md`) are reserved for the dedicated wrapper tools.
    /// Subfolder files like `diary/2026-01-01.md` are NOT root-level.
    pub fn is_agents_root_level(&self, vpath: &VirtualPath) -> bool {
        if self.detect_region(vpath) != Region::InsideAgentsFolder {
            return false;
        }
        let remainder = self.agents_remainder(vpath);
        // Root-level when the remainder is a single filename component (its parent
        // is empty) — there is no intervening subfolder segment.
        remainder.file_name().is_some()
            && remainder
                .parent()
                .map(|p| p.as_str().is_empty())
                .unwrap_or(true)
    }

    /// The portion of an inside-agents virtual path beneath the agents folder.
    fn agents_remainder(&self, vpath: &VirtualPath) -> Utf8PathBuf {
        if self.agents_is_root() {
            vpath.0.clone()
        } else {
            vpath
                .0
                .strip_prefix(&self.agents_dir)
                .map(|p| p.to_owned())
                .unwrap_or_else(|_| vpath.0.clone())
        }
    }

    /// Resolve a virtual path to a physical path, applying the scope transform
    /// inside the agents folder. Does not touch the filesystem beyond the
    /// containment check.
    ///
    /// `rendered_scope` is the already-validated rendered suffix string (empty
    /// when the scheme is empty).
    pub fn resolve(
        &self,
        rendered_scope: &str,
        vpath: &VirtualPath,
    ) -> Result<PhysicalPath, MuninnError> {
        let region = self.detect_region(vpath);

        let relative: Utf8PathBuf = match region {
            Region::OutsideAgentsFolder => vpath.0.clone(),
            Region::InsideAgentsFolder => {
                let remainder = self.agents_remainder(vpath);
                if self.scheme.is_empty() {
                    // No per-scope segment, no suffix.
                    self.join_agents(&remainder)
                } else {
                    let transformed = apply_scope_to_relative(&remainder, rendered_scope)
                        .ok_or_else(|| MuninnError::InvalidArgument {
                            message: "path must name a file".to_string(),
                        })?;
                    // <agents_dir>/<scope>/<transformed remainder>
                    let mut scoped = Utf8PathBuf::from(rendered_scope);
                    scoped.push(transformed);
                    self.join_agents(&scoped)
                }
            }
        };

        let physical = self.vault_root.join(relative.as_std_path());
        self.assert_contained(&physical, vpath)?;
        Ok(PhysicalPath(physical))
    }

    /// Join a path beneath the agents folder (or the root when agents IS root).
    fn join_agents(&self, rest: &Utf8Path) -> Utf8PathBuf {
        if self.agents_is_root() {
            rest.to_owned()
        } else {
            self.agents_dir.join(rest)
        }
    }

    /// Recover the clean (suffix-stripped, root-relative) virtual path for a
    /// physical file inside the agents folder, or `None` if the file does not
    /// belong to `rendered_scope`.
    pub fn strip_suffix(
        &self,
        physical: &std::path::Path,
        rendered_scope: &str,
    ) -> Option<VirtualPath> {
        let rel_std = physical.strip_prefix(&self.vault_root).ok()?;
        let rel = Utf8Path::from_path(rel_std)?;

        // Strip the agents-folder prefix to get the in-agents remainder.
        let in_agents: &Utf8Path = if self.agents_is_root() {
            rel
        } else {
            rel.strip_prefix(&self.agents_dir).ok()?
        };

        if self.scheme.is_empty() {
            // No scope filtering: present the file at its root-relative path.
            return Some(VirtualPath::from_relative(rel.to_owned()));
        }

        // First component must be the caller's scope segment.
        let mut comps = in_agents.components();
        let first = comps.next()?;
        if first.as_str() != rendered_scope {
            return None;
        }
        let beneath: Utf8PathBuf = comps.collect();
        if beneath.as_str().is_empty() {
            return None;
        }

        // The filename must carry the caller's suffix; strip it.
        let dir = beneath.parent().map(|p| p.to_owned());
        let filename = beneath.file_name()?;
        let clean_name = strip_scope_from_filename(filename, rendered_scope)?;

        let mut clean = Utf8PathBuf::new();
        clean.push(&self.agents_dir);
        if let Some(dir) = dir
            && !dir.as_str().is_empty()
        {
            clean.push(dir);
        }
        clean.push(clean_name);
        Some(VirtualPath::from_relative(clean))
    }

    /// Reject any physical path whose nearest existing ancestor canonicalises to
    /// somewhere outside the vault root (catches `..` survivors and symlink
    /// escapes).
    fn assert_contained(
        &self,
        physical: &std::path::Path,
        vpath: &VirtualPath,
    ) -> Result<(), MuninnError> {
        let escapes = || MuninnError::PathEscapesRoot {
            virtual_path: vpath.as_str().to_string(),
        };

        // Canonicalise the deepest existing ancestor (the target may not exist
        // yet). A symlink anywhere along the existing prefix is resolved here.
        let mut probe = physical;
        loop {
            match probe.canonicalize() {
                Ok(real) => {
                    if real.starts_with(&self.vault_root) {
                        return Ok(());
                    } else {
                        return Err(escapes());
                    }
                }
                Err(_) => match probe.parent() {
                    Some(parent) => probe = parent,
                    None => return Err(escapes()),
                },
            }
        }
    }
}

/// Apply the scope suffix to the filename of a relative path, leaving directory
/// components untouched. Returns `None` when the path has no filename.
fn apply_scope_to_relative(relative: &Utf8Path, suffix: &str) -> Option<Utf8PathBuf> {
    let filename = relative.file_name()?;
    let new_name = apply_suffix_to_filename(filename, suffix);
    let mut out = relative.parent().map(|p| p.to_owned()).unwrap_or_default();
    out.push(new_name);
    Some(out)
}

/// Insert `.<suffix>` between a filename's stem and its extension:
/// `plan.md` + `jarvis.tony` → `plan.jarvis.tony.md`; `NOTES` +
/// `jarvis` → `NOTES.jarvis` (extensionless names get the suffix appended).
fn apply_suffix_to_filename(filename: &str, suffix: &str) -> String {
    let path = Utf8Path::new(filename);
    match (path.file_stem(), path.extension()) {
        (Some(stem), Some(ext)) => format!("{stem}.{suffix}.{ext}"),
        _ => format!("{filename}.{suffix}"),
    }
}

/// Append the scope suffix to a link target, leaving any leading directory
/// segments untouched. A wikilink target (`rust`, `topics/rust`) has no extension
/// so the suffix is appended verbatim; a markdown target carrying a `.md`
/// extension (`topics/rust.md`) gets the suffix inserted before the extension.
///
/// Link targets only ever carry a `.md` extension or none, so — unlike
/// [`apply_suffix_to_filename`] — this does not consult `Utf8Path::extension`,
/// which would mistake the dotted suffix itself (`.jarvis.tony`) for an extension.
pub fn apply_suffix_to_link_target(target: &str, suffix: &str) -> String {
    match target.strip_suffix(".md") {
        Some(stem) => format!("{stem}.{suffix}.md"),
        None => format!("{target}.{suffix}"),
    }
}

/// Inverse of [`apply_suffix_to_link_target`]: recover the clean link target if
/// it carries `suffix`, else `None`.
pub fn strip_suffix_from_link_target(target: &str, suffix: &str) -> Option<String> {
    let needle = format!(".{suffix}");
    match target.strip_suffix(".md") {
        Some(stem) => stem
            .strip_suffix(&needle)
            .map(|clean| format!("{clean}.md")),
        None => target.strip_suffix(&needle).map(|s| s.to_string()),
    }
}

/// Inverse of [`apply_suffix_to_filename`]: recover the original filename if it
/// carries `suffix`, else `None`.
fn strip_scope_from_filename(filename: &str, suffix: &str) -> Option<String> {
    let path = Utf8Path::new(filename);
    let needle = format!(".{suffix}");
    match path.extension() {
        Some(ext) => {
            let stem = path.file_stem()?;
            let base = stem.strip_suffix(&needle)?;
            if base.is_empty() {
                return None;
            }
            Some(format!("{base}.{ext}"))
        }
        None => {
            let base = filename.strip_suffix(&needle)?;
            if base.is_empty() {
                return None;
            }
            Some(base.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;

    fn resolver(root: &std::path::Path, agents: &str, scheme: &str) -> PathResolver {
        PathResolver::new(
            root.canonicalize().unwrap(),
            Utf8PathBuf::from(agents),
            Scheme::parse(scheme).unwrap(),
        )
    }

    #[test]
    fn rejects_empty_absolute_and_traversal() {
        assert!(matches!(
            VirtualPath::new(""),
            Err(MuninnError::InvalidArgument { .. })
        ));
        assert!(matches!(
            VirtualPath::new("/etc/passwd"),
            Err(MuninnError::PathEscapesRoot { .. })
        ));
        assert!(matches!(
            VirtualPath::new("../../etc/passwd"),
            Err(MuninnError::PathEscapesRoot { .. })
        ));
        assert!(matches!(
            VirtualPath::new("a/../../b"),
            Err(MuninnError::PathEscapesRoot { .. })
        ));
        assert!(matches!(
            VirtualPath::new("a\0b"),
            Err(MuninnError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn current_dir_components_are_dropped() {
        assert_eq!(VirtualPath::new("./a/./b.md").unwrap().as_str(), "a/b.md");
    }

    #[test]
    fn default_scheme_resolves_agent_and_user() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<agent>.<user>");
        let vp = VirtualPath::new("Agents/tasks/plan.md").unwrap();
        let physical = r.resolve("jarvis.tony", &vp).unwrap();
        assert!(
            physical
                .as_path()
                .ends_with("Agents/jarvis.tony/tasks/plan.jarvis.tony.md")
        );
    }

    #[test]
    fn single_key_scheme_suffixes_extensionless_friendly() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<agent>");
        let vp = VirtualPath::new("Agents/TASK-STATE.md").unwrap();
        let physical = r.resolve("jarvis", &vp).unwrap();
        assert!(
            physical
                .as_path()
                .ends_with("Agents/jarvis/TASK-STATE.jarvis.md")
        );
    }

    #[test]
    fn agents_root_level_detection() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<agent>.<user>");
        // Root-level core files inside the agents folder.
        for f in [
            "Agents/MEMORY.md",
            "Agents/PERSONA.md",
            "Agents/HEARTBEAT.md",
        ] {
            assert!(
                r.is_agents_root_level(&VirtualPath::new(f).unwrap()),
                "{f} should be root-level"
            );
        }
        // Subfolder files are NOT root-level.
        for f in ["Agents/diary/2026-01-01.md", "Agents/topics/auth/jwt.md"] {
            assert!(
                !r.is_agents_root_level(&VirtualPath::new(f).unwrap()),
                "{f} should not be root-level"
            );
        }
        // Outside the agents folder is never root-level.
        assert!(!r.is_agents_root_level(&VirtualPath::new("Actions/release.md").unwrap()));
    }

    #[test]
    fn agents_root_level_when_agents_is_vault_root() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "", "<agent>.<user>");
        assert!(r.is_agents_root_level(&VirtualPath::new("MEMORY.md").unwrap()));
        assert!(!r.is_agents_root_level(&VirtualPath::new("diary/2026-01-01.md").unwrap()));
    }

    #[test]
    fn multi_key_scheme() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<team>.<agent>.<env>.<user>");
        let vp = VirtualPath::new("Agents/tasks/plan.md").unwrap();
        let physical = r.resolve("platform.jarvis.prod.tony", &vp).unwrap();
        assert!(
            physical.as_path().ends_with(
                "Agents/platform.jarvis.prod.tony/tasks/plan.platform.jarvis.prod.tony.md"
            )
        );
    }

    #[test]
    fn empty_scheme_applies_no_suffix() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "");
        let vp = VirtualPath::new("Agents/notes.md").unwrap();
        let physical = r.resolve("", &vp).unwrap();
        assert!(physical.as_path().ends_with("Agents/notes.md"));
    }

    #[test]
    fn vault_root_as_agents_folder() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "", "<agent>.<user>");
        let vp = VirtualPath::new("tasks/plan.md").unwrap();
        assert_eq!(r.detect_region(&vp), Region::InsideAgentsFolder);
        let physical = r.resolve("jarvis.tony", &vp).unwrap();
        assert!(
            physical
                .as_path()
                .ends_with("jarvis.tony/tasks/plan.jarvis.tony.md")
        );
    }

    #[test]
    fn region_detection() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<agent>.<user>");
        assert_eq!(
            r.detect_region(&VirtualPath::new("Agents/topics/rust.md").unwrap()),
            Region::InsideAgentsFolder
        );
        assert_eq!(
            r.detect_region(&VirtualPath::new("Actions/release.md").unwrap()),
            Region::OutsideAgentsFolder
        );
    }

    /// Symlink escape: a symlink inside the vault pointing outside must be refused.
    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let outside = assert_fs::TempDir::new().unwrap();
        outside.child("secret.md").write_str("top secret").unwrap();

        let tmp = assert_fs::TempDir::new().unwrap();
        let agents = tmp.child("Agents");
        agents.create_dir_all().unwrap();
        // Agents/escape -> <outside>
        std::os::unix::fs::symlink(outside.path(), agents.child("escape").path()).unwrap();

        let r = resolver(tmp.path(), "Agents", "");
        let vp = VirtualPath::new("Agents/escape/secret.md").unwrap();
        assert!(matches!(
            r.resolve("", &vp),
            Err(MuninnError::PathEscapesRoot { .. })
        ));
    }

    /// Cross-scope unreachability: a crafted path bearing another scope's suffix
    /// still resolves under the caller's own scope, never to the other file.
    #[test]
    fn crafted_path_cannot_reach_other_scope() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<agent>.<user>");
        let vp = VirtualPath::new("Agents/tasks/plan.jarvis.sam.md").unwrap();
        let physical = r.resolve("jarvis.tony", &vp).unwrap();
        let s = physical.as_path().to_string_lossy();
        // Always under the caller's own scope directory + suffix.
        assert!(s.contains("Agents/jarvis.tony/"));
        assert!(s.ends_with(".jarvis.tony.md"));
        assert!(!s.contains("jarvis.sam/"));
    }

    #[test]
    fn strip_suffix_recovers_clean_path_for_own_scope() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<agent>.<user>");
        let physical = tmp
            .path()
            .canonicalize()
            .unwrap()
            .join("Agents/jarvis.tony/tasks/plan.jarvis.tony.md");
        let clean = r.strip_suffix(&physical, "jarvis.tony").unwrap();
        assert_eq!(clean.as_str(), "Agents/tasks/plan.md");
    }

    #[test]
    fn strip_suffix_rejects_other_scope() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path(), "Agents", "<agent>.<user>");
        let physical = tmp
            .path()
            .canonicalize()
            .unwrap()
            .join("Agents/jarvis.sam/tasks/plan.jarvis.sam.md");
        assert_eq!(r.strip_suffix(&physical, "jarvis.tony"), None);
    }

    #[test]
    fn link_target_suffix_round_trips_for_wikilinks() {
        // Bare basename (no extension): suffix appended verbatim.
        assert_eq!(
            apply_suffix_to_link_target("rust", "jarvis.tony"),
            "rust.jarvis.tony"
        );
        assert_eq!(
            strip_suffix_from_link_target("rust.jarvis.tony", "jarvis.tony").as_deref(),
            Some("rust")
        );
        // Path-qualified target: only the final segment is suffixed.
        assert_eq!(
            apply_suffix_to_link_target("topics/rust", "jarvis.tony"),
            "topics/rust.jarvis.tony"
        );
        assert_eq!(
            strip_suffix_from_link_target("topics/rust.jarvis.tony", "jarvis.tony").as_deref(),
            Some("topics/rust")
        );
    }

    #[test]
    fn link_target_suffix_round_trips_for_markdown() {
        // A `.md` extension: suffix is inserted before the extension.
        assert_eq!(
            apply_suffix_to_link_target("topics/rust.md", "jarvis.tony"),
            "topics/rust.jarvis.tony.md"
        );
        assert_eq!(
            strip_suffix_from_link_target("topics/rust.jarvis.tony.md", "jarvis.tony").as_deref(),
            Some("topics/rust.md")
        );
    }

    #[test]
    fn link_target_strip_requires_exact_suffix() {
        // A target not carrying the suffix is left unrecovered (None).
        assert_eq!(strip_suffix_from_link_target("rust", "jarvis.tony"), None);
        assert_eq!(
            strip_suffix_from_link_target("rust.jarvis.sam", "jarvis.tony"),
            None
        );
    }

    #[test]
    fn link_target_collision_case_is_exact_match() {
        // A note literally named `x.jarvis.tony` collides with the suffix shape:
        // the exact-match strip recovers `x`, mirroring the filename transform.
        // Applying then stripping round-trips through the doubled suffix.
        assert_eq!(
            apply_suffix_to_link_target("x.jarvis.tony", "jarvis.tony"),
            "x.jarvis.tony.jarvis.tony"
        );
        assert_eq!(
            strip_suffix_from_link_target("x.jarvis.tony.jarvis.tony", "jarvis.tony").as_deref(),
            Some("x.jarvis.tony")
        );
    }
}
