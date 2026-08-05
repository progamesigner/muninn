//! The on-disk storage layer: atomic writes, search/replace edits, deletes,
//! directory walking with visibility filters, listing, and opaque pagination.
//!
//! All full-file writes use the temp-file + fsync + rename pattern for crash
//! safety. Concurrent writes to the same target within this process are
//! serialised by a per-target advisory lock. Cross-process races are tolerated
//! (last-writer-wins via the atomic rename), per design decision D5.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use camino::{Utf8Component, Utf8Path};
use dashmap::DashMap;
use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::error::MuninnError;
use crate::path::{PathResolver, PhysicalPath, VirtualPath};
use crate::policy::Region;

/// An opaque pagination cursor — a base64-encoded byte offset into the
/// deterministic listing order.
pub struct Cursor;

impl Cursor {
    pub fn encode(offset: u64) -> String {
        BASE64.encode(offset.to_string().as_bytes())
    }

    pub fn decode(cursor: &str) -> Result<u64, MuninnError> {
        let bytes = BASE64
            .decode(cursor)
            .map_err(|_| MuninnError::InvalidArgument {
                message: "cursor is not valid".to_string(),
            })?;
        let text = std::str::from_utf8(&bytes).map_err(|_| MuninnError::InvalidArgument {
            message: "cursor is not valid".to_string(),
        })?;
        text.parse::<u64>()
            .map_err(|_| MuninnError::InvalidArgument {
                message: "cursor is not valid".to_string(),
            })
    }
}

/// One visible note in a [`LinkIndex`]: its clean, vault-root-relative path with
/// the `.md` extension stripped, and the region it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEntry {
    /// Clean path, extension stripped (e.g. `Agents/topics/rust`, `Actions/release`).
    pub clean_path: String,
    /// Whether the note is in the caller's own scope or the shared region.
    pub region: Region,
}

/// An index of the caller's visible notes, keyed by clean basename (final path
/// segment with the `.md` extension stripped), for resolving `[[wikilink]]`
/// targets the way Obsidian does. Only notes that pass own-scope filtering and
/// the visibility filters appear here.
#[derive(Debug, Default)]
pub struct LinkIndex {
    by_basename: std::collections::BTreeMap<String, Vec<LinkEntry>>,
}

impl LinkIndex {
    /// Record one visible note from its clean vault-root-relative virtual path.
    pub(crate) fn insert(&mut self, clean_vpath: &str, region: Region) {
        let clean_path = clean_vpath
            .strip_suffix(".md")
            .unwrap_or(clean_vpath)
            .to_string();
        let basename = clean_path
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(&clean_path)
            .to_string();
        self.by_basename
            .entry(basename)
            .or_default()
            .push(LinkEntry { clean_path, region });
    }

    /// Sort each basename's entries by clean path for deterministic resolution
    /// and rendering, regardless of filesystem walk order.
    pub(crate) fn sort(&mut self) {
        for entries in self.by_basename.values_mut() {
            entries.sort_by(|a, b| a.clean_path.cmp(&b.clean_path));
        }
    }

    /// All visible notes sharing `basename` (its final segment), in deterministic
    /// order.
    pub fn entries_for_basename(&self, basename: &str) -> &[LinkEntry] {
        self.by_basename
            .get(basename)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Every indexed entry, across all basenames.
    pub fn all_entries(&self) -> impl Iterator<Item = &LinkEntry> {
        self.by_basename.values().flatten()
    }
}

/// Compile an include-hidden glob list into a single `Gitignore` matcher, rooted
/// at the vault root. Each pattern is a gitignore-style glob; an entry that
/// matches a path (or, via `matched_path_or_any_parents`, one of its ancestors)
/// exempts that path from hidden-segment filtering. An empty list yields a
/// matcher that matches nothing.
pub(crate) fn compile_include_globs(
    root: &Path,
    patterns: &[String],
) -> Result<Gitignore, ignore::Error> {
    let mut builder = GitignoreBuilder::new(root);
    for pattern in patterns {
        builder.add_line(None, pattern)?;
    }
    builder.build()
}

/// The on-disk storage layer.
pub struct Storage {
    resolver: PathResolver,
    honor_ignore_files: bool,
    include_hidden: bool,
    /// Glob matcher whose matches (path or any parent) exempt a dot-path from
    /// hidden filtering. Matches nothing when no patterns are configured.
    include_hidden_globs: Gitignore,
    /// The glob patterns as configured. Retained because the recall index
    /// fingerprint hashes the visibility settings that shape what gets ingested,
    /// and a compiled `Gitignore` cannot be read back.
    include_hidden_glob_patterns: Vec<String>,
    locks: DashMap<PathBuf, Arc<Mutex<()>>>,
}

impl Storage {
    pub fn new(
        resolver: PathResolver,
        honor_ignore_files: bool,
        include_hidden: bool,
        include_hidden_globs: &[String],
    ) -> Storage {
        // Patterns are validated at config-load time; if compilation somehow
        // fails here, fall back to an empty matcher (no exemptions) rather than
        // panicking inside the storage layer.
        let include_hidden_glob_patterns = include_hidden_globs.to_vec();
        let include_hidden_globs =
            compile_include_globs(resolver.vault_root(), include_hidden_globs)
                .unwrap_or_else(|_| Gitignore::empty());
        Storage {
            resolver,
            honor_ignore_files,
            include_hidden,
            include_hidden_globs,
            include_hidden_glob_patterns,
            locks: DashMap::new(),
        }
    }

    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    /// Whether ignore files are honoured when listing.
    pub fn honor_ignore_files(&self) -> bool {
        self.honor_ignore_files
    }

    /// Whether hidden (dot) paths are visible.
    pub fn include_hidden(&self) -> bool {
        self.include_hidden
    }

    /// The configured hidden-exemption glob patterns, as written.
    pub fn include_hidden_glob_patterns(&self) -> &[String] {
        &self.include_hidden_glob_patterns
    }

    /// Read a file's UTF-8 contents.
    pub fn read(&self, physical: &PhysicalPath) -> Result<String, MuninnError> {
        match std::fs::read(physical.as_path()) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| MuninnError::Io {
                kind: std::io::ErrorKind::InvalidData,
                context: "reading note (not valid UTF-8)",
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(MuninnError::NotFound {
                virtual_path: self.display_path(physical),
            }),
            Err(e) => Err(MuninnError::io("reading note", &e)),
        }
    }

    /// Atomically write the full contents of a file: temp file in the same
    /// directory, fsync, then rename over the target. Returns the byte count.
    pub fn write_atomic(
        &self,
        physical: &PhysicalPath,
        content: &str,
    ) -> Result<usize, MuninnError> {
        self.with_target_lock(physical.as_path(), || {
            self.write_atomic_locked(physical, content)
        })
    }

    /// Read the current contents (treating a missing file as absent), hand them
    /// to `f` to compute the new contents, and persist atomically — all under the
    /// per-target lock so concurrent callers serialise. An `Err` from `f` aborts
    /// with the file unchanged. Used by the diary append, the verbatim append,
    /// and the frontmatter property merge.
    pub fn read_modify_write(
        &self,
        physical: &PhysicalPath,
        f: impl FnOnce(Option<String>) -> Result<String, MuninnError>,
    ) -> Result<usize, MuninnError> {
        self.with_target_lock(physical.as_path(), || {
            let current = match std::fs::read_to_string(physical.as_path()) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(MuninnError::io("reading note", &e)),
            };
            let next = f(current)?;
            self.write_atomic_locked(physical, &next)
        })
    }

    fn write_atomic_locked(
        &self,
        physical: &PhysicalPath,
        content: &str,
    ) -> Result<usize, MuninnError> {
        self.mkdirs_for(physical)?;
        let parent = physical.as_path().parent().ok_or(MuninnError::Io {
            kind: std::io::ErrorKind::InvalidInput,
            context: "resolving parent directory",
        })?;

        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| MuninnError::io("creating temp file", &e))?;
        temp.write_all(content.as_bytes())
            .map_err(|e| MuninnError::io("writing temp file", &e))?;
        temp.as_file()
            .sync_all()
            .map_err(|e| MuninnError::io("syncing temp file", &e))?;
        temp.persist(physical.as_path())
            .map_err(|e| MuninnError::io("renaming temp file", &e.error))?;
        Ok(content.len())
    }

    /// Replace the unique occurrence of `search` with `replace`, persisting
    /// atomically. The search string must occur exactly once.
    pub fn edit_search_replace(
        &self,
        physical: &PhysicalPath,
        search: &str,
        replace: &str,
    ) -> Result<usize, MuninnError> {
        self.with_target_lock(physical.as_path(), || {
            let current = self.read(physical)?;
            let count = current.matches(search).count();
            match count {
                0 => Err(MuninnError::EditSearchNotFound),
                1 => {
                    let updated = current.replacen(search, replace, 1);
                    self.write_atomic_locked(physical, &updated)?;
                    Ok(search.len())
                }
                n => Err(MuninnError::EditSearchAmbiguous { count: n }),
            }
        })
    }

    /// Delete a single file. Never removes directories; leaves an emptied parent
    /// in place.
    pub fn delete(&self, physical: &PhysicalPath) -> Result<(), MuninnError> {
        self.with_target_lock(physical.as_path(), || {
            match std::fs::remove_file(physical.as_path()) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(MuninnError::NotFound {
                    virtual_path: self.display_path(physical),
                }),
                Err(e) => Err(MuninnError::io("deleting note", &e)),
            }
        })
    }

    /// Create any missing parent directories for a write target.
    pub fn mkdirs_for(&self, physical: &PhysicalPath) -> Result<(), MuninnError> {
        if let Some(parent) = physical.as_path().parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MuninnError::io("creating parent directories", &e))?;
        }
        Ok(())
    }

    /// Whether a resolved physical path is visible (not hidden, not ignored)
    /// under the active filters. Direct read/write/edit/delete call this to
    /// reject excluded paths with `path_not_permitted` before any IO.
    pub fn is_visible(&self, physical: &PhysicalPath) -> bool {
        let root = self.resolver.vault_root();
        let Ok(rel_std) = physical.as_path().strip_prefix(root) else {
            return false;
        };
        let Some(rel) = Utf8Path::from_path(rel_std) else {
            return false;
        };

        if !self.include_hidden && self.is_hidden(rel) {
            return false;
        }
        if self.honor_ignore_files && self.is_ignored(rel) {
            return false;
        }
        true
    }

    /// List the caller's own-scope files inside the agents folder as clean
    /// virtual paths.
    pub fn list_inside_agents_folder(
        &self,
        rendered_scope: &str,
    ) -> Result<Vec<VirtualPath>, MuninnError> {
        let agents_root = self.agents_root();
        let mut out = Vec::new();
        for (physical, _rel) in self.walk_files(&agents_root) {
            if let Some(clean) = self.resolver.strip_suffix(&physical, rendered_scope) {
                out.push(clean);
            }
        }
        Ok(out)
    }

    /// List files outside the agents folder (but inside the vault root) as clean
    /// virtual paths.
    pub fn list_outside_agents_folder(&self) -> Result<Vec<VirtualPath>, MuninnError> {
        if self.resolver.agents_dir().as_str().is_empty() {
            // The agents folder is the vault root; there is no outside region.
            return Ok(Vec::new());
        }
        let root = self.resolver.vault_root().to_path_buf();
        let agents_dir = self.resolver.agents_dir().to_owned();
        let mut out = Vec::new();
        for (_physical, rel) in self.walk_files(&root) {
            if rel.starts_with(&agents_dir) {
                continue;
            }
            out.push(VirtualPath::new(rel.as_str())?);
        }
        Ok(out)
    }

    /// All virtual paths visible to a scope across the supplied regions,
    /// deduplicated and deterministically ordered.
    pub fn list_visible(
        &self,
        rendered_scope: &str,
        regions: &[Region],
    ) -> Result<Vec<VirtualPath>, MuninnError> {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for region in regions {
            let paths = match region {
                Region::InsideAgentsFolder => self.list_inside_agents_folder(rendered_scope)?,
                Region::OutsideAgentsFolder => self.list_outside_agents_folder()?,
            };
            for p in paths {
                set.insert(p.as_str().to_string());
            }
        }
        set.into_iter()
            .map(|s| VirtualPath::new(&s))
            .collect::<Result<Vec<_>, _>>()
    }

    /// Build a [`LinkIndex`] of the caller's visible notes across `regions`, for
    /// wikilink resolution. Reuses the same own-scope filtering and visibility
    /// rules as listing, so an excluded (hidden/ignored) or other-scope note is
    /// never a resolution candidate.
    pub fn build_link_index(
        &self,
        rendered_scope: &str,
        regions: &[Region],
    ) -> Result<LinkIndex, MuninnError> {
        let mut index = LinkIndex::default();
        for region in regions {
            let paths = match region {
                Region::InsideAgentsFolder => self.list_inside_agents_folder(rendered_scope)?,
                Region::OutsideAgentsFolder => self.list_outside_agents_folder()?,
            };
            for p in paths {
                index.insert(p.as_str(), *region);
            }
        }
        index.sort();
        Ok(index)
    }

    /// The rendered-scope directory names directly under the agents folder (e.g.
    /// `jarvis.tony`). Each names one per-scope region; used by the recall engine
    /// to enumerate every scope for its eager startup build. Returns an empty list
    /// when the agents folder is the vault root (single-tenant: there are no
    /// per-scope directories).
    pub fn list_scope_dirs(&self) -> Vec<String> {
        if self.resolver.agents_dir().as_str().is_empty() {
            return Vec::new();
        }
        let agents_root = self.agents_root();
        let Ok(entries) = std::fs::read_dir(&agents_root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && let Some(name) = entry.file_name().to_str()
            {
                out.push(name.to_string());
            }
        }
        out.sort();
        out
    }

    // --- internals ---

    fn agents_root(&self) -> PathBuf {
        let dir = self.resolver.agents_dir();
        if dir.as_str().is_empty() {
            self.resolver.vault_root().to_path_buf()
        } else {
            self.resolver.vault_root().join(dir.as_str())
        }
    }

    /// Walk a start directory, applying ignore-file filtering (via the `ignore`
    /// crate) and hidden filtering (by hand, so the agents folder is exempt even
    /// when it begins with `.`). Returns `(physical, rel_to_root)` for each file.
    fn walk_files(&self, start: &Path) -> Vec<(PathBuf, camino::Utf8PathBuf)> {
        if !start.exists() {
            return Vec::new();
        }
        let root = self.resolver.vault_root().to_path_buf();
        let mut builder = WalkBuilder::new(start);
        builder
            .hidden(false) // hidden filtering done by hand (agents-folder exemption)
            .parents(self.honor_ignore_files)
            .git_global(self.honor_ignore_files)
            .git_ignore(self.honor_ignore_files)
            .git_exclude(self.honor_ignore_files)
            .ignore(self.honor_ignore_files)
            .require_git(false)
            .follow_links(false);
        if self.honor_ignore_files {
            builder.add_custom_ignore_filename(".obsidianignore");
        }

        let mut out = Vec::new();
        for entry in builder.build().flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            let Ok(rel_std) = path.strip_prefix(&root) else {
                continue;
            };
            let Some(rel) = Utf8Path::from_path(rel_std) else {
                continue;
            };
            if !self.include_hidden && self.is_hidden(rel) {
                continue;
            }
            out.push((path.to_path_buf(), rel.to_owned()));
        }
        out
    }

    /// A path is hidden if any segment begins with `.`, except segments that are
    /// part of the agents-folder prefix (so a `.agents` folder stays visible) or
    /// paths exempted by the include-hidden glob list.
    fn is_hidden(&self, rel: &Utf8Path) -> bool {
        let agents = self.resolver.agents_dir();
        let to_check: &Utf8Path = if !agents.as_str().is_empty() && rel.starts_with(agents) {
            rel.strip_prefix(agents).unwrap_or(rel)
        } else {
            rel
        };
        let dotted = to_check
            .components()
            .any(|c| matches!(c, Utf8Component::Normal(s) if s.starts_with('.')));
        if !dotted {
            return false;
        }
        // Dot-prefixed, so hidden by default — unless an include-hidden glob
        // matches the path or one of its parents (whole-subtree exemption).
        !self.is_hidden_exempt(rel)
    }

    /// Whether an include-hidden glob matches `rel` (relative to the vault root)
    /// or any of its parent directories, un-hiding it and its whole subtree.
    fn is_hidden_exempt(&self, rel: &Utf8Path) -> bool {
        let abs = self.resolver.vault_root().join(rel.as_str());
        self.include_hidden_globs
            .matched_path_or_any_parents(&abs, false)
            .is_ignore()
    }

    /// Whether `rel` (relative to the vault root) is matched by a `.ignore`,
    /// `.gitignore`, or `.obsidianignore` rule, assembled from the vault root
    /// down to the file's parent directory.
    fn is_ignored(&self, rel: &Utf8Path) -> bool {
        let root = self.resolver.vault_root();
        let mut builder = GitignoreBuilder::new(root);

        // Collect ignore files from root down to the file's parent directory, so
        // nested ignore files compose per-directory exactly like `git` (matching
        // the walker's `WalkBuilder` behaviour).
        let mut dir = root.to_path_buf();
        let add_for = |b: &mut GitignoreBuilder, d: &Path| {
            b.add(d.join(".ignore"));
            b.add(d.join(".gitignore"));
            b.add(d.join(".obsidianignore"));
        };
        add_for(&mut builder, &dir);
        if let Some(parent) = rel.parent() {
            for comp in parent.components() {
                if let Utf8Component::Normal(seg) = comp {
                    dir.push(seg);
                    add_for(&mut builder, &dir);
                }
            }
        }

        let Ok(gitignore) = builder.build() else {
            return false;
        };
        let abs = root.join(rel.as_str());
        gitignore
            .matched_path_or_any_parents(&abs, false)
            .is_ignore()
    }

    /// Run `f` while holding the per-target advisory lock. The backing `Mutex`
    /// lives in the `DashMap` entry; we clone its `Arc` so the guard's lifetime is
    /// sound regardless of map churn.
    fn with_target_lock<R>(&self, target: &Path, f: impl FnOnce() -> R) -> R {
        let arc = self
            .locks
            .entry(target.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = arc.lock().unwrap_or_else(|p| p.into_inner());
        f()
    }

    fn display_path(&self, physical: &PhysicalPath) -> String {
        physical
            .as_path()
            .strip_prefix(self.resolver.vault_root())
            .ok()
            .and_then(Utf8Path::from_path)
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "<note>".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::Scheme;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use camino::Utf8PathBuf;

    fn storage(tmp: &TempDir, agents: &str, scheme: &str, honor: bool, hidden: bool) -> Storage {
        storage_with_globs(tmp, agents, scheme, honor, hidden, &[])
    }

    fn storage_with_globs(
        tmp: &TempDir,
        agents: &str,
        scheme: &str,
        honor: bool,
        hidden: bool,
        include_hidden_globs: &[String],
    ) -> Storage {
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            Utf8PathBuf::from(agents),
            Scheme::parse(scheme).unwrap(),
        );
        Storage::new(resolver, honor, hidden, include_hidden_globs)
    }

    fn vp(s: &str) -> VirtualPath {
        VirtualPath::new(s).unwrap()
    }

    #[test]
    fn cursor_round_trips() {
        for offset in [0u64, 1, 50, 12345, u64::MAX] {
            assert_eq!(Cursor::decode(&Cursor::encode(offset)).unwrap(), offset);
        }
        assert!(Cursor::decode("!!!not base64!!!").is_err());
    }

    #[test]
    fn write_then_read_round_trips() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "<agent>.<user>", true, false);
        let physical = s
            .resolver
            .resolve("jarvis.tony", &vp("Agents/PERSONA.md"))
            .unwrap();
        let n = s.write_atomic(&physical, "hello").unwrap();
        assert_eq!(n, 5);
        assert_eq!(s.read(&physical).unwrap(), "hello");
    }

    #[test]
    fn read_missing_is_not_found() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "<agent>.<user>", true, false);
        let physical = s
            .resolver
            .resolve("jarvis.tony", &vp("Agents/missing.md"))
            .unwrap();
        assert!(matches!(
            s.read(&physical),
            Err(MuninnError::NotFound { .. })
        ));
    }

    #[test]
    fn write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "<agent>.<user>", true, false);
        let physical = s
            .resolver
            .resolve("jarvis.tony", &vp("Agents/deep/nested/note.md"))
            .unwrap();
        s.write_atomic(&physical, "x").unwrap();
        assert!(physical.as_path().exists());
    }

    #[test]
    fn edit_unique_succeeds_missing_and_ambiguous_fail() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "<agent>.<user>", true, false);
        let physical = s
            .resolver
            .resolve("jarvis.tony", &vp("Agents/n.md"))
            .unwrap();

        s.write_atomic(&physical, "alpha beta gamma").unwrap();
        s.edit_search_replace(&physical, "beta", "BETA").unwrap();
        assert_eq!(s.read(&physical).unwrap(), "alpha BETA gamma");

        assert!(matches!(
            s.edit_search_replace(&physical, "zeta", "z"),
            Err(MuninnError::EditSearchNotFound)
        ));

        s.write_atomic(&physical, "dup dup").unwrap();
        assert!(matches!(
            s.edit_search_replace(&physical, "dup", "x"),
            Err(MuninnError::EditSearchAmbiguous { count: 2 })
        ));
    }

    #[test]
    fn delete_removes_file_and_missing_is_not_found() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "<agent>.<user>", true, false);
        let physical = s
            .resolver
            .resolve("jarvis.tony", &vp("Agents/d.md"))
            .unwrap();
        s.write_atomic(&physical, "x").unwrap();
        s.delete(&physical).unwrap();
        assert!(!physical.as_path().exists());
        assert!(matches!(
            s.delete(&physical),
            Err(MuninnError::NotFound { .. })
        ));
    }

    #[test]
    fn listing_shows_only_own_scope() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "<agent>.<user>", true, false);
        for (scope, name) in [
            ("jarvis.tony", "Agents/notes.md"),
            ("jarvis.sam", "Agents/notes.md"),
            ("friday.tony", "Agents/notes.md"),
        ] {
            let p = s.resolver.resolve(scope, &vp(name)).unwrap();
            s.write_atomic(&p, "x").unwrap();
        }
        let listed = s.list_inside_agents_folder("jarvis.tony").unwrap();
        let strs: Vec<_> = listed.iter().map(|p| p.as_str().to_string()).collect();
        assert_eq!(strs, vec!["Agents/notes.md"]);
    }

    #[test]
    fn hidden_files_excluded_but_agents_dot_dir_visible() {
        let tmp = TempDir::new().unwrap();
        // agents folder begins with '.': must stay visible.
        let s = storage(&tmp, ".agents", "", true, false);
        let visible = s.resolver.resolve("", &vp(".agents/notes.md")).unwrap();
        let hidden = s.resolver.resolve("", &vp(".agents/.tmp.md")).unwrap();
        s.write_atomic(&visible, "x").unwrap();
        s.write_atomic(&hidden, "x").unwrap();

        let listed = s.list_inside_agents_folder("").unwrap();
        let strs: Vec<_> = listed.iter().map(|p| p.as_str().to_string()).collect();
        assert!(strs.contains(&".agents/notes.md".to_string()));
        assert!(!strs.iter().any(|s| s.contains(".tmp.md")));

        assert!(s.is_visible(&visible));
        assert!(!s.is_visible(&hidden));
    }

    #[test]
    fn gitignore_excludes_matched_files() {
        let tmp = TempDir::new().unwrap();
        tmp.child(".gitignore").write_str("*.wip.md\n").unwrap();
        let s = storage(&tmp, "Agents", "", true, false);

        let kept = s.resolver.resolve("", &vp("Agents/keep.md")).unwrap();
        let ignored = s.resolver.resolve("", &vp("Agents/draft.wip.md")).unwrap();
        s.write_atomic(&kept, "x").unwrap();
        s.write_atomic(&ignored, "x").unwrap();

        let listed = s.list_inside_agents_folder("").unwrap();
        let strs: Vec<_> = listed.iter().map(|p| p.as_str().to_string()).collect();
        assert!(strs.contains(&"Agents/keep.md".to_string()));
        assert!(!strs.iter().any(|s| s.contains("draft.wip.md")));
        assert!(!s.is_visible(&ignored));
        assert!(s.is_visible(&kept));
    }

    /// All virtual paths visible across both regions, as strings.
    fn visible_strs(s: &Storage) -> Vec<String> {
        s.list_visible(
            "",
            &[Region::InsideAgentsFolder, Region::OutsideAgentsFolder],
        )
        .unwrap()
        .iter()
        .map(|p| p.as_str().to_string())
        .collect()
    }

    /// A `.ignore`-matched file is excluded from listings AND rejected by the
    /// direct-access check — the visible set and the addressable set agree.
    #[test]
    fn dot_ignore_file_excludes_consistently() {
        let tmp = TempDir::new().unwrap();
        tmp.child("Agents/scratch/.ignore")
            .write_str("*.md\n")
            .unwrap();
        let s = storage(&tmp, "Agents", "", true, false);

        let ignored = s
            .resolver
            .resolve("", &vp("Agents/scratch/wip.md"))
            .unwrap();
        let kept = s.resolver.resolve("", &vp("Agents/keep.md")).unwrap();
        s.write_atomic(&ignored, "x").unwrap();
        s.write_atomic(&kept, "x").unwrap();

        let listed = visible_strs(&s);
        assert!(!listed.iter().any(|p| p.contains("scratch/wip.md")));
        assert!(listed.contains(&"Agents/keep.md".to_string()));
        assert!(!s.is_visible(&ignored));
        assert!(s.is_visible(&kept));
    }

    /// Disabling ignore-file enforcement turns off `.ignore` too.
    #[test]
    fn dot_ignore_disabled_when_not_honoring() {
        let tmp = TempDir::new().unwrap();
        tmp.child(".ignore").write_str("*.wip.md\n").unwrap();
        let s = storage(&tmp, "Agents", "", false, false);

        let was_ignored = s.resolver.resolve("", &vp("Agents/draft.wip.md")).unwrap();
        s.write_atomic(&was_ignored, "x").unwrap();
        assert!(s.is_visible(&was_ignored));
    }

    /// Nested ignore files compose per-directory like `git`: a rule in a
    /// subfolder applies only to that subtree, across all three ignore-file
    /// kinds, on both the listing and direct-access paths.
    #[test]
    fn nested_ignore_files_compose_per_directory() {
        for filename in [".ignore", ".gitignore", ".obsidianignore"] {
            let tmp = TempDir::new().unwrap();
            tmp.child(format!("Agents/drafts/{filename}"))
                .write_str("*.tmp.md\n")
                .unwrap();
            let s = storage(&tmp, "Agents", "", true, false);

            let nested = s
                .resolver
                .resolve("", &vp("Agents/drafts/wip.tmp.md"))
                .unwrap();
            let outside = s.resolver.resolve("", &vp("Agents/keep.tmp.md")).unwrap();
            s.write_atomic(&nested, "x").unwrap();
            s.write_atomic(&outside, "x").unwrap();

            let listed = visible_strs(&s);
            assert!(
                !listed.iter().any(|p| p.contains("drafts/wip.tmp.md")),
                "{filename}: nested rule should hide drafts/wip.tmp.md"
            );
            assert!(
                listed.contains(&"Agents/keep.tmp.md".to_string()),
                "{filename}: rule must not leak outside its subtree"
            );
            assert!(!s.is_visible(&nested), "{filename}: direct access agrees");
            assert!(s.is_visible(&outside));
        }
    }

    /// An include-hidden glob un-hides a dot-directory and its whole subtree,
    /// while unrelated dot-paths stay excluded.
    #[test]
    fn include_hidden_glob_unhides_subtree() {
        let tmp = TempDir::new().unwrap();
        let s = storage_with_globs(
            &tmp,
            "Agents",
            "",
            true,
            false,
            &[".obsidian/**".to_string()],
        );

        let app = s.resolver.resolve("", &vp(".obsidian/app.json")).unwrap();
        let plugin = s
            .resolver
            .resolve("", &vp(".obsidian/plugins/x/data.json"))
            .unwrap();
        let other = s.resolver.resolve("", &vp(".cache/tmp.md")).unwrap();
        s.write_atomic(&app, "x").unwrap();
        s.write_atomic(&plugin, "x").unwrap();
        s.write_atomic(&other, "x").unwrap();

        assert!(s.is_visible(&app));
        assert!(s.is_visible(&plugin));
        assert!(!s.is_visible(&other));

        let listed = visible_strs(&s);
        assert!(listed.contains(&".obsidian/app.json".to_string()));
        assert!(listed.contains(&".obsidian/plugins/x/data.json".to_string()));
        assert!(!listed.iter().any(|p| p.contains(".cache")));
    }

    /// With no globs configured, every dot-path stays excluded (today's default).
    #[test]
    fn empty_include_glob_list_excludes_all_dot_paths() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "", true, false);
        let hidden = s.resolver.resolve("", &vp(".obsidian/app.json")).unwrap();
        s.write_atomic(&hidden, "x").unwrap();
        assert!(!s.is_visible(&hidden));
    }

    /// `include_hidden = true` makes the glob list a no-op (everything visible).
    #[test]
    fn include_hidden_true_makes_globs_a_noop() {
        let tmp = TempDir::new().unwrap();
        let s = storage_with_globs(
            &tmp,
            "Agents",
            "",
            true,
            true,
            &[".obsidian/**".to_string()],
        );
        let app = s.resolver.resolve("", &vp(".obsidian/app.json")).unwrap();
        let other = s.resolver.resolve("", &vp(".cache/tmp.md")).unwrap();
        s.write_atomic(&app, "x").unwrap();
        s.write_atomic(&other, "x").unwrap();
        assert!(s.is_visible(&app));
        assert!(s.is_visible(&other));
    }

    /// An exempted dot-path is still subject to ignore-file rules unless ignore
    /// enforcement is disabled.
    #[test]
    fn exempted_dot_path_still_honors_ignore_files() {
        let tmp = TempDir::new().unwrap();
        tmp.child(".gitignore")
            .write_str(".obsidian/secret.json\n")
            .unwrap();
        let globs = [".obsidian/**".to_string()];

        let s = storage_with_globs(&tmp, "Agents", "", true, false, &globs);
        let app = s.resolver.resolve("", &vp(".obsidian/app.json")).unwrap();
        let secret = s
            .resolver
            .resolve("", &vp(".obsidian/secret.json"))
            .unwrap();
        s.write_atomic(&app, "x").unwrap();
        s.write_atomic(&secret, "x").unwrap();
        assert!(s.is_visible(&app));
        assert!(
            !s.is_visible(&secret),
            "ignore rule applies despite exemption"
        );

        // With enforcement off, the ignored file becomes visible.
        let s2 = storage_with_globs(&tmp, "Agents", "", false, false, &globs);
        assert!(s2.is_visible(&secret));
    }

    /// The agents-folder exemption is independent of the include-hidden glob
    /// list: a `.agents` folder stays visible even with an unrelated glob set.
    #[test]
    fn agents_folder_exemption_unaffected_by_globs() {
        let tmp = TempDir::new().unwrap();
        let s = storage_with_globs(
            &tmp,
            ".agents",
            "",
            true,
            false,
            &[".obsidian/**".to_string()],
        );
        let note = s.resolver.resolve("", &vp(".agents/notes.md")).unwrap();
        s.write_atomic(&note, "x").unwrap();
        assert!(s.is_visible(&note));
    }

    /// The link index contains own-scope and shared notes (keyed by clean
    /// basename) but excludes other scopes' files.
    #[test]
    fn link_index_spans_own_scope_and_shared_excludes_others() {
        let tmp = TempDir::new().unwrap();
        let s = storage(&tmp, "Agents", "<agent>.<user>", true, false);
        // Own scope, another scope, and a shared note.
        for (scope, name) in [
            ("jarvis.tony", "Agents/topics/rust.md"),
            ("jarvis.sam", "Agents/topics/rust.md"),
        ] {
            let p = s.resolver.resolve(scope, &vp(name)).unwrap();
            s.write_atomic(&p, "x").unwrap();
        }
        let shared = s.resolver.resolve("", &vp("Actions/release.md")).unwrap();
        s.write_atomic(&shared, "x").unwrap();

        let index = s
            .build_link_index(
                "jarvis.tony",
                &[Region::InsideAgentsFolder, Region::OutsideAgentsFolder],
            )
            .unwrap();

        // Own-scope `rust` is present and tagged inside; sam's is not a candidate.
        let rust = index.entries_for_basename("rust");
        assert_eq!(rust.len(), 1, "only the caller's own rust.md is indexed");
        assert_eq!(rust[0].clean_path, "Agents/topics/rust");
        assert_eq!(rust[0].region, Region::InsideAgentsFolder);

        // Shared note is present and tagged outside.
        let release = index.entries_for_basename("release");
        assert_eq!(release.len(), 1);
        assert_eq!(release[0].clean_path, "Actions/release");
        assert_eq!(release[0].region, Region::OutsideAgentsFolder);
    }

    /// An ignored note is not a resolution candidate in the link index.
    #[test]
    fn link_index_excludes_ignored_notes() {
        let tmp = TempDir::new().unwrap();
        tmp.child(".gitignore").write_str("*.wip.md\n").unwrap();
        let s = storage(&tmp, "Agents", "", true, false);
        let kept = s.resolver.resolve("", &vp("Agents/keep.md")).unwrap();
        let ignored = s.resolver.resolve("", &vp("Agents/draft.wip.md")).unwrap();
        s.write_atomic(&kept, "x").unwrap();
        s.write_atomic(&ignored, "x").unwrap();

        let index = s
            .build_link_index("", &[Region::InsideAgentsFolder])
            .unwrap();
        assert_eq!(index.entries_for_basename("keep").len(), 1);
        assert!(index.entries_for_basename("draft.wip").is_empty());
    }

    /// Concurrent writes to the same diary file are serialised by the per-target
    /// lock so no append is lost or interleaved.
    #[test]
    fn concurrent_appends_are_serialised() {
        let tmp = TempDir::new().unwrap();
        let s = Arc::new(storage(&tmp, "Agents", "<agent>.<user>", true, false));
        let physical = Arc::new(
            s.resolver
                .resolve("jarvis.tony", &vp("Agents/diary/d.md"))
                .unwrap(),
        );

        let mut handles = Vec::new();
        for i in 0..16 {
            let s = s.clone();
            let physical = physical.clone();
            handles.push(std::thread::spawn(move || {
                s.read_modify_write(&physical, |cur| {
                    Ok(format!("{}line{i}\n", cur.unwrap_or_default()))
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let final_contents = s.read(&physical).unwrap();
        assert_eq!(final_contents.lines().count(), 16);
    }
}
