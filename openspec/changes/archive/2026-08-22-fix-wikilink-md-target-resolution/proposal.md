## Why

`resolve_target` (`src/wikilink.rs`) strips a trailing `.md` from a relative
markdown link target (`LinkKind::Markdown`) before matching it against the
visible-note index, but does not do the same for a `[[wikilink]]` target
(`LinkKind::Wikilink`). Obsidian treats `[[file]]` and `[[file.md]]` as
equivalent, so a wikilink written with the extension — e.g.
`[[file.md|Title]]`, `[[file.md#heading]]`, `![[file.md]]` — can never match
an index entry, whose basenames are always stored extension-stripped. The
link is silently treated as dangling: no error, no scope suffix ever
applied, and (since backlink computation calls the same resolver) invisible
to `read_memory_note`'s `backlinks` and to `rename_memory_note`'s referrer
rewrite. The link then renders as unresolved in Obsidian, permanently,
with no diagnostic pointing at the cause.

## What Changes

- `resolve_target` strips a trailing `.md` from a `LinkKind::Wikilink`
  target before matching, the same way it already does for
  `LinkKind::Markdown`, so `[[file.md]]`, `[[file.md|alias]]`,
  `[[file.md#heading]]`, and `![[file.md]]` resolve identically to their
  extension-less equivalents.
- The persisted/rendered form of such a link drops the `.md` — a resolved
  wikilink is always rewritten to the same shape every other wikilink
  already takes (bare or suffixed basename, no extension), so
  `[[file.md|Title]]` and `[[file|Title]]` converge to one on-disk
  representation instead of the extension becoming a second, inconsistent
  spelling of the same link.
- Backlink computation (`references_to`, used by both
  `read_memory_note(backlinks: true)` and `rename_memory_note`'s referrer
  scan) picks up the fix automatically, since it resolves targets through
  the same `resolve_target` function.

## Capabilities

### New Capabilities
(none)

### Modified Capabilities
- `wikilink-references`: the "Supported link forms" requirement gains an
  explicit rule that a `[[wikilink]]` target carrying a literal `.md`
  extension resolves and rewrites identically to the same target without
  the extension.

## Impact

- `src/wikilink.rs`: `resolve_target` (the fix itself); `wikilink.rs` unit
  tests gain coverage for `.md`-suffixed wikilink targets across
  `expand_links`, `strip_links`, `references_to`, and `retarget_links`.
- No changes to `src/tools.rs`, `src/storage.rs`, or `src/path.rs` — the fix
  is isolated to target normalization inside the resolver; every caller
  (`expand_links`, `references_to`, `retarget_links`) already routes through
  `resolve_target`.
- No API/tool schema changes. No migration needed: existing on-disk links
  that already carry `.md` (if any) simply start resolving correctly the
  next time the referring note is written, edited, or its target is
  renamed; nothing rewrites them proactively.
