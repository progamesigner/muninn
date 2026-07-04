//! Bidirectional rewriting of `[[wikilink]]` and relative markdown link targets
//! between the agent-facing clean shortest-name form and the on-disk
//! suffixed/Obsidian-resolvable form.
//!
//! This is the content-level counterpart of the filename suffix transform in
//! [`crate::path`]. On **write**, a link an agent writes (`[[rust]]`) is resolved
//! against the caller's visible set and rewritten to the physical form that
//! resolves in Obsidian (`[[rust.jarvis.tony]]` for an own-scope target, or a
//! vault-root-relative physical path for a markdown link). On **read**, the
//! caller's own suffix is stripped so the agent only ever sees clean shortest
//! names and never another scope's existence.
//!
//! Resolution mirrors Obsidian: a target matches a visible note when the note's
//! clean path ends with the target's path segments and their basenames agree; the
//! shortest unambiguous trailing path is used as the rendered name. A link that
//! resolves to nothing is a dangling link and is left verbatim.

use camino::Utf8Path;

use crate::error::MuninnError;
use crate::path::{
    PathResolver, VirtualPath, apply_suffix_to_link_target, strip_suffix_from_link_target,
};
use crate::policy::Region;
use crate::storage::{LinkEntry, LinkIndex};

/// Which syntactic form a link target came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// `[[target]]`, `[[target|alias]]`, `[[target#heading]]`, or `![[target]]`.
    /// The target is a basename (no `.md` extension).
    Wikilink,
    /// `[text](path.md)` — the target is a relative path carrying `.md`.
    Markdown,
}

/// Expand every link target in `content` to its on-disk form. Own-scope targets
/// are rewritten with the caller's suffix; shared targets are left clean; targets
/// that do not resolve in the visible set are left verbatim (dangling).
///
/// When the file being written is in the shared region and a link resolves into
/// the caller's own scope, the write is refused — persisting the suffixed form
/// would leak the scope's existence to other readers of the shared file.
pub fn expand_links(
    content: &str,
    rendered_scope: &str,
    file_region: Region,
    resolver: &PathResolver,
    index: &LinkIndex,
) -> Result<String, MuninnError> {
    rewrite_links(content, |kind, target| {
        let Some(entry) = resolve_target(index, kind, target) else {
            return Ok(None); // dangling — leave verbatim
        };

        // Leak guard: a shared file must not embed a suffixed link to a scoped note.
        if file_region == Region::OutsideAgentsFolder && entry.region == Region::InsideAgentsFolder
        {
            return Err(MuninnError::CrossScopeLink {
                target: target.to_string(),
            });
        }

        let rendered = shortest_name(index, entry);
        match (kind, entry.region) {
            // Own-scope wikilink → append the suffix to the (possibly qualified) name.
            (LinkKind::Wikilink, Region::InsideAgentsFolder) => {
                Ok(Some(apply_suffix_to_link_target(&rendered, rendered_scope)))
            }
            // Own-scope markdown link → the full vault-root-relative physical path.
            (LinkKind::Markdown, Region::InsideAgentsFolder) => {
                let vpath = VirtualPath::new(&format!("{}.md", entry.clean_path))?;
                let physical = resolver.resolve(rendered_scope, &vpath)?;
                let rel = physical
                    .as_path()
                    .strip_prefix(resolver.vault_root())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| entry.clean_path.clone());
                Ok(Some(rel))
            }
            // Shared target → leave clean (resolves for every scope, no suffix).
            (LinkKind::Wikilink, Region::OutsideAgentsFolder) => Ok(Some(rendered)),
            (LinkKind::Markdown, Region::OutsideAgentsFolder) => {
                Ok(Some(format!("{}.md", entry.clean_path)))
            }
        }
    })
}

/// Strip the caller's own scope suffix from every link target in `content`, so a
/// reader sees only clean shortest names. Targets without the caller's suffix
/// (shared notes, dangling links) are returned unchanged.
pub fn strip_links(content: &str, rendered_scope: &str, resolver: &PathResolver) -> String {
    // strip_links never errors: an unrecognised target is simply left as-is.
    rewrite_links(content, |kind, target| {
        Ok::<_, MuninnError>(strip_target(kind, target, rendered_scope, resolver))
    })
    // The closure is infallible, so unwrap is safe.
    .unwrap_or_else(|_| content.to_string())
}

/// Expand every link occurrence in every string leaf of `value` to its on-disk
/// form, recursing into arrays and object values — the [`expand_links`]
/// transform applied to a parsed frontmatter property tree. Object keys and
/// non-string leaves are untouched data. Carries the same cross-scope leak
/// guard: the first offending leaf aborts and propagates the error.
pub fn expand_value_links(
    value: &serde_json::Value,
    rendered_scope: &str,
    file_region: Region,
    resolver: &PathResolver,
    index: &LinkIndex,
) -> Result<serde_json::Value, MuninnError> {
    map_string_leaves(value, &mut |s| {
        expand_links(s, rendered_scope, file_region, resolver, index)
    })
}

/// Strip the caller's own scope suffix from every link target in every string
/// leaf of `value`, recursing into arrays and object values — the
/// [`strip_links`] transform applied to a parsed frontmatter property tree.
pub fn strip_value_links(
    value: &serde_json::Value,
    rendered_scope: &str,
    resolver: &PathResolver,
) -> serde_json::Value {
    map_string_leaves(value, &mut |s| {
        Ok::<_, MuninnError>(strip_links(s, rendered_scope, resolver))
    })
    // The closure is infallible, so unwrap is safe.
    .unwrap_or_else(|_| value.clone())
}

/// Rebuild `value` with `f` applied to every string leaf, recursing into arrays
/// and object values. Object keys and non-string leaves are copied verbatim.
/// The first `Err` aborts and propagates.
fn map_string_leaves<F>(
    value: &serde_json::Value,
    f: &mut F,
) -> Result<serde_json::Value, MuninnError>
where
    F: FnMut(&str) -> Result<String, MuninnError>,
{
    use serde_json::Value;
    Ok(match value {
        Value::String(s) => Value::String(f(s)?),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| map_string_leaves(v, f))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), map_string_leaves(v, f)?);
            }
            Value::Object(out)
        }
        other => other.clone(),
    })
}

/// Strip the caller's suffix from one stored link target, per kind. `None` when
/// the target does not carry the caller's suffix (shared and dangling targets
/// are stored clean).
fn strip_target(
    kind: LinkKind,
    target: &str,
    rendered_scope: &str,
    resolver: &PathResolver,
) -> Option<String> {
    match kind {
        // Wikilinks store the suffixed basename/qualified name directly.
        LinkKind::Wikilink => strip_suffix_from_link_target(target, rendered_scope),
        // Markdown links store the vault-root-relative physical path; reverse it
        // via the resolver and drop the agents-folder prefix to the clean form.
        LinkKind::Markdown => strip_markdown_physical(target, rendered_scope, resolver),
    }
}

/// Whether `content` (in its stored, on-disk form) contains at least one link
/// that forward-resolves to the note at `target_clean_path` (its clean,
/// vault-root-relative path with the `.md` extension stripped). Each collected
/// target is stripped of the caller's suffix and resolved against `index` with
/// the same rules as the forward transform, so backlinks are the exact inverse
/// of link navigation: a dangling link counts toward nothing, and an ambiguous
/// basename counts only toward the entry forward resolution selects.
pub(crate) fn references_to(
    content: &str,
    target_clean_path: &str,
    rendered_scope: &str,
    resolver: &PathResolver,
    index: &LinkIndex,
) -> bool {
    let mut found = false;
    // Collector mode: the callback never rewrites and never errors.
    let _ = rewrite_links(content, |kind, target| {
        if !found {
            let stripped = strip_target(kind, target, rendered_scope, resolver);
            let clean = stripped.as_deref().unwrap_or(target);
            found = resolve_target(index, kind, clean)
                .is_some_and(|entry| entry.clean_path == target_clean_path);
        }
        Ok::<_, MuninnError>(None)
    });
    found
}

/// The visible set as it will look after a rename: every entry of `index` with
/// the note at `source_clean_path` replaced by `destination`. Link-target
/// derivation (shortest names, ambiguity) for rename rewrites runs against this
/// index so the output is identical to what the forward transform would produce
/// against the post-rename vault.
pub(crate) fn post_rename_index(
    index: &LinkIndex,
    source_clean_path: &str,
    destination: &LinkEntry,
) -> LinkIndex {
    let mut post = LinkIndex::default();
    for entry in index.all_entries() {
        if entry.clean_path == source_clean_path {
            post.insert(&destination.clean_path, destination.region);
        } else {
            post.insert(&entry.clean_path, entry.region);
        }
    }
    post.sort();
    post
}

/// The visible set extended with pending notes that do not exist yet: every
/// entry of `index` plus one entry per `pending` item whose clean path is not
/// already indexed. Link-target derivation for a batch write runs against this
/// index, so a link whose target is created by another entry of the same batch
/// resolves — and participates in shortest-name disambiguation — exactly as it
/// would after the batch lands. (Generalizes the [`post_rename_index`] pattern.)
pub(crate) fn seed_index(index: &LinkIndex, pending: &[LinkEntry]) -> LinkIndex {
    let mut seeded = LinkIndex::default();
    for entry in index.all_entries() {
        seeded.insert(&entry.clean_path, entry.region);
    }
    for entry in pending {
        let already_indexed = index
            .entries_for_basename(last_segment(&entry.clean_path))
            .iter()
            .any(|e| e.clean_path == entry.clean_path);
        if !already_indexed {
            seeded.insert(&entry.clean_path, entry.region);
        }
    }
    seeded.sort();
    seeded
}

/// Rewrite, in `content` (its stored on-disk form), exactly those link targets
/// whose forward resolution selects the note at `source_clean_path` (clean,
/// vault-root-relative, `.md` stripped), re-pointing them at `destination`. The
/// new target text is derived the same way the write-side transform derives it —
/// shortest unambiguous name against the post-rename index, suffixed for
/// own-scope wikilinks, vault-root-relative physical path for own-scope markdown
/// links, clean for shared targets — so a subsequent read round-trips. Aliases,
/// headings, embed markers, and every non-matching byte are untouched. Returns
/// the rewritten content and the number of links re-targeted.
///
/// `index` is the **pre-rename** index (it still contains the source), so
/// targets resolve exactly as they were authored.
///
/// When `file_region` is the shared region and `destination` is scoped, a
/// matching link is refused with the cross-scope leak guard — persisting the
/// suffixed form would leak the scope's existence.
pub(crate) fn retarget_links(
    content: &str,
    source_clean_path: &str,
    destination: &LinkEntry,
    rendered_scope: &str,
    file_region: Region,
    resolver: &PathResolver,
    index: &LinkIndex,
) -> Result<(String, usize), MuninnError> {
    let post = post_rename_index(index, source_clean_path, destination);
    let mut count = 0usize;
    let rewritten = rewrite_links(content, |kind, target| {
        let stripped = strip_target(kind, target, rendered_scope, resolver);
        let clean = stripped.as_deref().unwrap_or(target);
        let selects_source = resolve_target(index, kind, clean)
            .is_some_and(|entry| entry.clean_path == source_clean_path);
        if !selects_source {
            return Ok(None); // resolves elsewhere or dangles — leave verbatim
        }
        if file_region == Region::OutsideAgentsFolder
            && destination.region == Region::InsideAgentsFolder
        {
            return Err(MuninnError::CrossScopeLink {
                target: target.to_string(),
            });
        }
        count += 1;
        let rendered = shortest_name(&post, destination);
        match (kind, destination.region) {
            (LinkKind::Wikilink, Region::InsideAgentsFolder) => {
                // An empty rendered scope (empty scheme) carries no suffix.
                if rendered_scope.is_empty() {
                    Ok(Some(rendered))
                } else {
                    Ok(Some(apply_suffix_to_link_target(&rendered, rendered_scope)))
                }
            }
            (LinkKind::Markdown, Region::InsideAgentsFolder) => {
                let vpath = VirtualPath::new(&format!("{}.md", destination.clean_path))?;
                let physical = resolver.resolve(rendered_scope, &vpath)?;
                let rel = physical
                    .as_path()
                    .strip_prefix(resolver.vault_root())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| destination.clean_path.clone());
                Ok(Some(rel))
            }
            (LinkKind::Wikilink, Region::OutsideAgentsFolder) => Ok(Some(rendered)),
            (LinkKind::Markdown, Region::OutsideAgentsFolder) => {
                Ok(Some(format!("{}.md", destination.clean_path)))
            }
        }
    })?;
    Ok((rewritten, count))
}

/// Reverse the own-scope markdown physical form back to the agents-folder-relative
/// clean path, or `None` when the target is not an own-scope physical path.
fn strip_markdown_physical(
    target: &str,
    rendered_scope: &str,
    resolver: &PathResolver,
) -> Option<String> {
    let abs = resolver.vault_root().join(target);
    let clean_vpath = resolver.strip_suffix(&abs, rendered_scope)?;
    // clean_vpath is e.g. `Agents/topics/rust.md`; drop the agents-folder prefix.
    let agents = resolver.agents_dir();
    let clean = clean_vpath.as_str();
    if agents.as_str().is_empty() {
        Some(clean.to_string())
    } else {
        clean
            .strip_prefix(&format!("{agents}/"))
            .map(|s| s.to_string())
    }
}

/// Resolve a link target against the visible index, Obsidian-style: the target
/// matches a note when the note's clean path ends with the target's path segments
/// and their basenames agree. A unique match wins; ties prefer the caller's own
/// scope, then the lexicographically smallest clean path. Returns `None` when no
/// visible note matches (a dangling link).
pub(crate) fn resolve_target<'a>(
    index: &'a LinkIndex,
    kind: LinkKind,
    target: &str,
) -> Option<&'a LinkEntry> {
    let clean = match kind {
        LinkKind::Markdown => target.strip_suffix(".md").unwrap_or(target),
        LinkKind::Wikilink => target,
    };
    let basename = last_segment(clean);
    let candidates = index.entries_for_basename(basename);

    let mut matches: Vec<&LinkEntry> = candidates
        .iter()
        .filter(|e| path_ends_with_segments(&e.clean_path, clean))
        .collect();
    if matches.is_empty() {
        return None;
    }
    // Deterministic tie-break: own scope first, then smallest clean path.
    matches.sort_by(|a, b| {
        let region_rank = |r: Region| match r {
            Region::InsideAgentsFolder => 0,
            Region::OutsideAgentsFolder => 1,
        };
        region_rank(a.region)
            .cmp(&region_rank(b.region))
            .then_with(|| a.clean_path.cmp(&b.clean_path))
    });
    Some(matches[0])
}

/// The shortest trailing path of `entry.clean_path` that no other visible note
/// sharing its basename also ends with — the agent-facing clean name. Falls back
/// to the full clean path if every trailing segment collides.
fn shortest_name(index: &LinkIndex, entry: &LinkEntry) -> String {
    let basename = last_segment(&entry.clean_path);
    let others: Vec<&LinkEntry> = index
        .entries_for_basename(basename)
        .iter()
        .filter(|e| e.clean_path != entry.clean_path)
        .collect();

    let segments: Vec<&str> = entry.clean_path.split('/').collect();
    for k in 1..=segments.len() {
        let candidate = segments[segments.len() - k..].join("/");
        let collides = others
            .iter()
            .any(|o| path_ends_with_segments(&o.clean_path, &candidate));
        if !collides {
            return candidate;
        }
    }
    entry.clean_path.clone()
}

/// The final path segment of a `/`-separated clean path.
fn last_segment(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, name)| name).unwrap_or(path)
}

/// Whether `haystack` ends with `needle`'s `/`-separated segments, segment-aligned.
fn path_ends_with_segments(haystack: &str, needle: &str) -> bool {
    let h: Vec<&str> = haystack.split('/').collect();
    let n: Vec<&str> = needle.split('/').collect();
    if n.len() > h.len() {
        return false;
    }
    h[h.len() - n.len()..] == n[..]
}

/// Scan `content` for `[[wikilinks]]`, `![[embeds]]`, and `[text](markdown.md)`
/// links, calling `f(kind, target)` for each. When `f` returns `Ok(Some(new))`,
/// the target portion is replaced with `new` (alias, heading, embed prefix, and
/// link text are preserved); `Ok(None)` leaves the link unchanged. The first
/// `Err` aborts and propagates.
fn rewrite_links<F>(content: &str, mut f: F) -> Result<String, MuninnError>
where
    F: FnMut(LinkKind, &str) -> Result<Option<String>, MuninnError>,
{
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < bytes.len() {
        // Wikilink or embed: optional leading '!' then "[[".
        let embed = bytes[i] == b'!' && bytes[i + 1..].starts_with(b"[[");
        if bytes[i..].starts_with(b"[[") || embed {
            let open = if embed { i + 1 } else { i };
            if let Some(close_rel) = find(&bytes[open + 2..], b"]]") {
                let inner = &content[open + 2..open + 2 + close_rel];
                let (target, rest) = split_wikilink_inner(inner);
                match f(LinkKind::Wikilink, target)? {
                    Some(new) => {
                        if embed {
                            out.push('!');
                        }
                        out.push_str("[[");
                        out.push_str(&new);
                        out.push_str(rest);
                        out.push_str("]]");
                    }
                    None => out.push_str(&content[i..open + 2 + close_rel + 2]),
                }
                i = open + 2 + close_rel + 2;
                continue;
            }
        }

        // Markdown link: "[text](target)". A leading '!' (image) is preserved.
        if bytes[i] == b'['
            && !bytes[i..].starts_with(b"[[")
            && let Some(parsed) = parse_markdown_link(content, i)
        {
            let (text_end, target, link_end) = parsed;
            if is_rewritable_markdown_target(target) {
                match f(LinkKind::Markdown, target)? {
                    Some(new) => {
                        out.push_str(&content[i..text_end]); // "[text]("
                        out.push_str(&new);
                        out.push(')');
                    }
                    None => out.push_str(&content[i..link_end]),
                }
            } else {
                out.push_str(&content[i..link_end]);
            }
            i = link_end;
            continue;
        }

        // Default: copy one UTF-8 char.
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&content[i..i + ch_len]);
        i += ch_len;
    }
    Ok(out)
}

/// Split a wikilink body into its target and the preserved remainder
/// (`#heading`, `|alias`, or both). `rust#h|a` → (`rust`, `#h|a`).
fn split_wikilink_inner(inner: &str) -> (&str, &str) {
    let cut = inner.find(['#', '|']).unwrap_or(inner.len());
    (&inner[..cut], &inner[cut..])
}

/// Parse a markdown link starting at `start` (a `[`). Returns
/// `(byte index just after "](", target, byte index just after ")")` or `None`
/// if the bytes at `start` are not a well-formed `[...](...)`.
fn parse_markdown_link(content: &str, start: usize) -> Option<(usize, &str, usize)> {
    let bytes = content.as_bytes();
    // Find the closing ']' of the link text (no nesting handling — markdown text
    // rarely contains unescaped brackets).
    let close_text = start + 1 + find(&bytes[start + 1..], b"]")?;
    if bytes.get(close_text + 1) != Some(&b'(') {
        return None;
    }
    let target_start = close_text + 2;
    let close_paren = target_start + find(&bytes[target_start..], b")")?;
    let target = &content[target_start..close_paren];
    Some((target_start, target, close_paren + 1))
}

/// Whether a markdown target should be rewritten: a relative `.md` path, not an
/// external URL (`scheme://`, `mailto:`) and not an anchor-only (`#...`) link.
fn is_rewritable_markdown_target(target: &str) -> bool {
    if target.is_empty() || target.starts_with('#') {
        return false;
    }
    if target.contains("://") || target.starts_with("mailto:") {
        return false;
    }
    // Only plain note links (ending in `.md`, no `#heading` fragment) are
    // rewritten; fragments and non-note targets are left untouched.
    Utf8Path::new(target).extension() == Some("md")
}

/// Find the first occurrence of `needle` in `haystack`, returning its byte offset.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The byte length of the UTF-8 character whose leading byte is `b`.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::Scheme;
    use camino::Utf8PathBuf;

    fn resolver(root: &std::path::Path) -> PathResolver {
        PathResolver::new(
            root.canonicalize().unwrap(),
            Utf8PathBuf::from("Agents"),
            Scheme::parse("<agent>.<user>").unwrap(),
        )
    }

    fn index(entries: &[(&str, Region)]) -> LinkIndex {
        let mut idx = LinkIndex::default();
        for (path, region) in entries {
            idx.insert(path, *region);
        }
        idx
    }

    // --- parser ---

    #[test]
    fn parser_collects_targets_and_preserves_decorations() {
        let seen = std::cell::RefCell::new(Vec::new());
        let out = rewrite_links(
            "see [[rust]], [[topics/rust#install|the note]], embed ![[guide]] and \
             [link](topics/rust.md) plus [web](https://x.com) and [a](#top)",
            |kind, target| {
                seen.borrow_mut().push((kind, target.to_string()));
                Ok::<_, MuninnError>(None)
            },
        )
        .unwrap();
        // Content is untouched when the callback returns None.
        assert!(out.contains("[web](https://x.com)"));
        assert!(out.contains("[a](#top)"));
        let seen = seen.into_inner();
        // External and anchor-only markdown links are not offered to the callback.
        assert_eq!(
            seen,
            vec![
                (LinkKind::Wikilink, "rust".to_string()),
                (LinkKind::Wikilink, "topics/rust".to_string()),
                (LinkKind::Wikilink, "guide".to_string()),
                (LinkKind::Markdown, "topics/rust.md".to_string()),
            ]
        );
    }

    #[test]
    fn parser_replaces_target_only() {
        let out = rewrite_links("x [[rust#h|alias]] y ![[g]] z [t](a.md)", |kind, _t| {
            Ok::<_, MuninnError>(Some(match kind {
                LinkKind::Wikilink => "NEW".to_string(),
                LinkKind::Markdown => "NEW.md".to_string(),
            }))
        })
        .unwrap();
        assert_eq!(out, "x [[NEW#h|alias]] y ![[NEW]] z [t](NEW.md)");
    }

    // --- resolution + shortest name ---

    #[test]
    fn resolve_unique_basename() {
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let e = resolve_target(&idx, LinkKind::Wikilink, "rust").unwrap();
        assert_eq!(e.clean_path, "Agents/topics/rust");
        assert_eq!(shortest_name(&idx, e), "rust");
    }

    #[test]
    fn resolve_collision_qualifies_shortest_path() {
        let idx = index(&[
            ("Agents/topics/rust.md", Region::InsideAgentsFolder),
            ("Lang/rust.md", Region::OutsideAgentsFolder),
        ]);
        let own = resolve_target(&idx, LinkKind::Wikilink, "topics/rust").unwrap();
        assert_eq!(own.region, Region::InsideAgentsFolder);
        assert_eq!(shortest_name(&idx, own), "topics/rust");
        let shared = resolve_target(&idx, LinkKind::Wikilink, "Lang/rust").unwrap();
        assert_eq!(shared.region, Region::OutsideAgentsFolder);
        assert_eq!(shortest_name(&idx, shared), "Lang/rust");
    }

    #[test]
    fn resolve_dangling_is_none() {
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        assert!(resolve_target(&idx, LinkKind::Wikilink, "missing").is_none());
    }

    // --- expand / strip round-trip ---

    #[test]
    fn expand_own_scope_wikilink_suffixes() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let out = expand_links(
            "see [[rust]]",
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        assert_eq!(out, "see [[rust.jarvis.tony]]");
        // Round-trip: strip recovers the clean name.
        assert_eq!(strip_links(&out, "jarvis.tony", &r), "see [[rust]]");
    }

    #[test]
    fn expand_shared_wikilink_stays_clean() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Actions/release.md", Region::OutsideAgentsFolder)]);
        let out = expand_links(
            "[[release]]",
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        assert_eq!(out, "[[release]]");
    }

    #[test]
    fn expand_dangling_left_verbatim() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[]);
        let out = expand_links(
            "[[not-yet]] and [x](future.md)",
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        assert_eq!(out, "[[not-yet]] and [x](future.md)");
    }

    #[test]
    fn expand_rejects_shared_file_linking_to_scoped_note() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let err = expand_links(
            "[[rust]]",
            "jarvis.tony",
            Region::OutsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap_err();
        assert!(matches!(err, MuninnError::CrossScopeLink { .. }));
        assert_eq!(err.code(), crate::error::ErrorCode::WriteDenied);
    }

    #[test]
    fn expand_and_strip_markdown_round_trip() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let out = expand_links(
            "[see Rust](topics/rust.md)",
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        // The persisted link is the full vault-root-relative physical path.
        assert_eq!(
            out,
            "[see Rust](Agents/jarvis.tony/topics/rust.jarvis.tony.md)"
        );
        // Read strips the scope dir + suffix back to the agents-relative path.
        assert_eq!(
            strip_links(&out, "jarvis.tony", &r),
            "[see Rust](topics/rust.md)"
        );
    }

    /// Property: for own-scope content across every supported link form,
    /// stripping an expansion recovers the normalized clean content.
    #[test]
    fn strip_of_expand_recovers_clean_content() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[
            ("Agents/rust.md", Region::InsideAgentsFolder),
            ("Agents/topics/guide.md", Region::InsideAgentsFolder),
            ("Actions/release.md", Region::OutsideAgentsFolder),
        ]);
        let clean = "# notes\n\
             plain [[rust]], aliased [[rust|R]], heading [[rust#install]], \
             both [[guide#h|G]], embed ![[guide]], shared [[release]], \
             md [doc](topics/guide.md), shared md [r](Actions/release.md), \
             external [w](https://x.com), anchor [a](#top), dangling [[ghost]].";
        let expanded =
            expand_links(clean, "jarvis.tony", Region::InsideAgentsFolder, &r, &idx).unwrap();
        // The expanded form differs (own-scope links carry the suffix)...
        assert!(expanded.contains("[[rust.jarvis.tony]]"));
        assert!(expanded.contains("(Agents/jarvis.tony/topics/guide.jarvis.tony.md)"));
        // ...but stripping it recovers exactly the clean content.
        assert_eq!(strip_links(&expanded, "jarvis.tony", &r), clean);
    }

    // --- references_to (backlink reverse resolution) ---

    #[test]
    fn references_to_interprets_suffixed_on_disk_forms() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        // The stored own-scope form of `[[rust]]`.
        assert!(references_to(
            "see [[rust.jarvis.tony]]",
            "Agents/topics/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
        // The clean form (as a shared file would store it) resolves identically.
        assert!(references_to(
            "see [[rust]]",
            "Agents/topics/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
    }

    #[test]
    fn references_to_counts_every_link_form() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let stored_forms = [
            "[[rust.jarvis.tony|the Rust note]]",
            "[[rust.jarvis.tony#install]]",
            "![[rust.jarvis.tony]]",
            // The stored own-scope markdown form: vault-root-relative physical path.
            "[doc](Agents/jarvis.tony/topics/rust.jarvis.tony.md)",
        ];
        for content in stored_forms {
            assert!(
                references_to(content, "Agents/topics/rust", "jarvis.tony", &r, &idx),
                "{content} should count as a backlink"
            );
        }
        assert!(!references_to(
            "no links here",
            "Agents/topics/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
    }

    #[test]
    fn references_to_ambiguous_basename_follows_forward_tie_break() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[
            ("Agents/topics/rust.md", Region::InsideAgentsFolder),
            ("Lang/rust.md", Region::OutsideAgentsFolder),
        ]);
        // `[[rust]]` forward-resolves to the own-scope entry (own scope preferred),
        // so it is a backlink only for that entry.
        let forward = resolve_target(&idx, LinkKind::Wikilink, "rust").unwrap();
        assert_eq!(forward.clean_path, "Agents/topics/rust");
        assert!(references_to(
            "[[rust]]",
            "Agents/topics/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
        assert!(!references_to(
            "[[rust]]",
            "Lang/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
        // A qualified link to the shared entry counts only for it.
        assert!(references_to(
            "[[Lang/rust]]",
            "Lang/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
        assert!(!references_to(
            "[[Lang/rust]]",
            "Agents/topics/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
    }

    #[test]
    fn references_to_dangling_links_resolve_to_nothing() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        assert!(!references_to(
            "[[ghost]] and [g](ghost.md)",
            "Agents/topics/rust",
            "jarvis.tony",
            &r,
            &idx,
        ));
    }

    // --- retarget_links (rename rewrite) ---

    fn entry(clean_path: &str, region: Region) -> LinkEntry {
        LinkEntry {
            clean_path: clean_path.to_string(),
            region,
        }
    }

    #[test]
    fn retarget_preserves_decorations() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let dest = entry("Agents/topics/rust-lang", Region::InsideAgentsFolder);
        let (out, n) = retarget_links(
            "[[rust.jarvis.tony#install|the note]] and ![[rust.jarvis.tony]]",
            "Agents/topics/rust",
            &dest,
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        assert_eq!(
            out,
            "[[rust-lang.jarvis.tony#install|the note]] and ![[rust-lang.jarvis.tony]]"
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn retarget_leaves_non_matching_same_basename_links_untouched() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[
            ("Agents/topics/rust.md", Region::InsideAgentsFolder),
            ("Lang/rust.md", Region::OutsideAgentsFolder),
        ]);
        let dest = entry("Agents/topics/rust-lang", Region::InsideAgentsFolder);
        let (out, n) = retarget_links(
            "[[topics/rust.jarvis.tony]] vs [[Lang/rust]]",
            "Agents/topics/rust",
            &dest,
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        // Only the link resolving to the source is rewritten; the shared
        // same-basename link is byte-identical.
        assert_eq!(out, "[[rust-lang.jarvis.tony]] vs [[Lang/rust]]");
        assert_eq!(n, 1);
    }

    /// The rewritten name is the shortest unambiguous form against the
    /// post-rename set: the qualified `topics/rust` collapses to the bare new
    /// basename once it no longer collides with `Lang/rust`.
    #[test]
    fn retarget_rederives_shortest_name_post_rename() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[
            ("Agents/topics/rust.md", Region::InsideAgentsFolder),
            ("Lang/rust.md", Region::OutsideAgentsFolder),
        ]);
        let dest = entry("Agents/topics/rust-lang", Region::InsideAgentsFolder);
        let (out, n) = retarget_links(
            "see [[topics/rust.jarvis.tony]]",
            "Agents/topics/rust",
            &dest,
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        assert_eq!(out, "see [[rust-lang.jarvis.tony]]");
        assert_eq!(n, 1);
    }

    #[test]
    fn retarget_rewrites_markdown_physical_path() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let dest = entry("Agents/topics/rust-lang", Region::InsideAgentsFolder);
        let (out, n) = retarget_links(
            "[doc](Agents/jarvis.tony/topics/rust.jarvis.tony.md)",
            "Agents/topics/rust",
            &dest,
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        assert_eq!(
            out,
            "[doc](Agents/jarvis.tony/topics/rust-lang.jarvis.tony.md)"
        );
        assert_eq!(n, 1);
    }

    /// A shared referrer cannot be re-pointed at a scoped destination: the
    /// suffixed link would leak the scope's existence into the shared region.
    #[test]
    fn retarget_shared_referrer_to_scoped_destination_is_leak_guarded() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Actions/release.md", Region::OutsideAgentsFolder)]);
        let dest = entry("Agents/topics/release", Region::InsideAgentsFolder);
        let err = retarget_links(
            "ship [[release]]",
            "Actions/release",
            &dest,
            "jarvis.tony",
            Region::OutsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap_err();
        assert!(matches!(err, MuninnError::CrossScopeLink { .. }));
        assert_eq!(err.code(), crate::error::ErrorCode::WriteDenied);
    }

    // --- seed_index (batch-write pending entries) ---

    #[test]
    fn seeded_entries_resolve_and_participate_in_disambiguation() {
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let pending = [
            entry("Agents/lang/rust", Region::InsideAgentsFolder),
            entry("Agents/topics/go", Region::InsideAgentsFolder),
        ];
        let seeded = seed_index(&idx, &pending);
        // A pending entry resolves like a visible note.
        let go = resolve_target(&seeded, LinkKind::Wikilink, "go").unwrap();
        assert_eq!(go.clean_path, "Agents/topics/go");
        assert_eq!(shortest_name(&seeded, go), "go");
        // And it participates in shortest-name disambiguation: the bare `rust`
        // basename now collides, so both entries render qualified.
        let existing = resolve_target(&seeded, LinkKind::Wikilink, "topics/rust").unwrap();
        assert_eq!(shortest_name(&seeded, existing), "topics/rust");
        let pending_rust = resolve_target(&seeded, LinkKind::Wikilink, "lang/rust").unwrap();
        assert_eq!(shortest_name(&seeded, pending_rust), "lang/rust");
    }

    #[test]
    fn seeding_an_already_visible_path_does_not_duplicate_it() {
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let seeded = seed_index(
            &idx,
            &[entry("Agents/topics/rust", Region::InsideAgentsFolder)],
        );
        // Still a single entry: the bare basename stays unambiguous.
        assert_eq!(seeded.entries_for_basename("rust").len(), 1);
        let e = resolve_target(&seeded, LinkKind::Wikilink, "rust").unwrap();
        assert_eq!(shortest_name(&seeded, e), "rust");
    }

    // --- value walkers (frontmatter property trees) ---

    #[test]
    fn value_expand_and_strip_recurse_into_arrays_and_objects() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let clean = serde_json::json!({
            "related": "[[rust]]",
            "links": ["[[rust]]", { "see": "see [[rust]] too" }],
            "dangling": "[[ghost]]",
            "priority": 2,
            "done": false,
        });
        let expanded =
            expand_value_links(&clean, "jarvis.tony", Region::InsideAgentsFolder, &r, &idx)
                .unwrap();
        assert_eq!(
            expanded,
            serde_json::json!({
                "related": "[[rust.jarvis.tony]]",
                "links": ["[[rust.jarvis.tony]]", { "see": "see [[rust.jarvis.tony]] too" }],
                "dangling": "[[ghost]]",
                "priority": 2,
                "done": false,
            })
        );
        // Round-trip: stripping the expansion recovers the clean tree.
        assert_eq!(strip_value_links(&expanded, "jarvis.tony", &r), clean);
    }

    #[test]
    fn value_expand_does_not_touch_object_keys() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        // A key that looks like a link is data layout, not a link.
        let value = serde_json::json!({ "[[rust]]": "[[rust]]" });
        let expanded =
            expand_value_links(&value, "jarvis.tony", Region::InsideAgentsFolder, &r, &idx)
                .unwrap();
        assert_eq!(
            expanded,
            serde_json::json!({ "[[rust]]": "[[rust.jarvis.tony]]" })
        );
    }

    #[test]
    fn value_expand_leak_guard_propagates_from_nested_leaf() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/topics/rust.md", Region::InsideAgentsFolder)]);
        let value = serde_json::json!({ "links": [{ "see": "[[rust]]" }] });
        let err = expand_value_links(&value, "jarvis.tony", Region::OutsideAgentsFolder, &r, &idx)
            .unwrap_err();
        assert!(matches!(err, MuninnError::CrossScopeLink { .. }));
        assert_eq!(err.code(), crate::error::ErrorCode::WriteDenied);
    }

    #[test]
    fn expand_preserves_alias_and_heading() {
        let tmp = assert_fs::TempDir::new().unwrap();
        let r = resolver(tmp.path());
        let idx = index(&[("Agents/rust.md", Region::InsideAgentsFolder)]);
        let out = expand_links(
            "[[rust#install|the Rust note]] and ![[rust]]",
            "jarvis.tony",
            Region::InsideAgentsFolder,
            &r,
            &idx,
        )
        .unwrap();
        assert_eq!(
            out,
            "[[rust.jarvis.tony#install|the Rust note]] and ![[rust.jarvis.tony]]"
        );
    }
}
