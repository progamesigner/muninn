## Context

See `proposal.md` — Why. The relevant code is `src/wikilink.rs`:
`resolve_target` (the resolver), `rewrite_links`/`split_wikilink_inner` (the
parser, which does not — and should not — care whether a target carries
`.md`; it just extracts whatever text sits before the first `#`/`|`), and
`apply_suffix_to_link_target` (the renderer, which already assumes a bare
basename or a `.md`-suffixed markdown target — see below). Three call sites
route every wikilink target through `resolve_target`: `expand_links`'s
closure, `references_to`, and `retarget_links`. Fixing `resolve_target`
alone fixes all three.

## Goals / Non-Goals

**Goals:**
- Make `resolve_target` treat a `LinkKind::Wikilink` target's trailing
  `.md` the same way it already treats one on `LinkKind::Markdown`: strip
  before matching.
- Make the rewritten/persisted form drop the extension, so the on-disk
  representation of a resolved wikilink is uniform regardless of how the
  agent originally typed it.

**Non-Goals:**
- No change to markdown-link (`[text](path.md)`) handling — already
  correct.
- No retroactive rewrite of already-dangling `.md`-suffixed wikilinks
  already on disk. Per the existing "Dangling links are preserved"
  requirement, nothing proactively rewrites a note just because a target it
  references becomes resolvable — that has always been true of any
  previously-dangling link (e.g. one written before its target existed) and
  this fix doesn't special-case `.md` links differently. They resolve
  correctly the next time that note is written, edited, or a rename touches
  them.
- No change to case sensitivity (`.MD`, `.Md`) — out of scope; Obsidian
  itself treats markdown extensions case-sensitively on most platforms, and
  neither the existing `Markdown`-kind handling nor this fix special-cases
  it.

## Decisions

**Strip in `resolve_target`, not in the parser.** `rewrite_links` splits a
wikilink's inner text into `target` and `rest` (`#heading`/`|alias`) with
no opinion about what `target` contains — that's the right layer for
syntax, not semantics. Extension handling is about how a target is
*matched and rendered*, which is `resolve_target`'s and the render-side
job. Concretely:

```rust
let clean = match kind {
    LinkKind::Markdown => target.strip_suffix(".md").unwrap_or(target),
    LinkKind::Wikilink => target.strip_suffix(".md").unwrap_or(target), // was: target
};
```

Alternative considered: strip `.md` once, up front, for both kinds before
the `match` even exists (collapse the two arms). Rejected only because it
would touch the function signature/shape more than necessary for a
one-line fix — this is a call for whoever implements it, not a hard
constraint.

**Rendered form always drops the extension.** `apply_suffix_to_link_target`
already special-cases `.md` for the *markdown* form (insert the suffix
before the extension) versus the bare wikilink form (append the suffix
verbatim, no extension ever appended). Its own doc comment already flags
the reason it doesn't consult `Utf8Path::extension()`: doing so "would
mistake the dotted suffix itself (`.jarvis.tony`) for an extension." Since
`resolve_target` will now hand back a clean (extension-stripped) target
string to every caller — `expand_links`, `references_to`, `retarget_links`
all resolve against `clean`, then re-derive the rendered form from the
*matched index entry*, not from the caller's original target text — no
change to `apply_suffix_to_link_target` is needed at all: the `.md` never
reaches it. The extension is dropped simply by virtue of resolution
happening against the stripped string and rendering happening from the
entry, not from the original bytes.

**No parser-level rejection of unusual targets.** A target that happens to
end in `.md` but doesn't resolve to anything is still a normal dangling
link (unchanged behavior) — the fix only changes what *does* resolve, not
what counts as an error.

## Risks / Trade-offs

- [Risk] A note that legitimately has a basename ending in the literal
  four characters `.md` as part of its name (not an extension) — e.g. a
  note about markdown itself, if someone genuinely names it `writing.md.md`
  or similar — could see a `[[writing.md]]` link now resolve to
  `writing.md` (stripped) instead of to a hypothetical
  `writing.md.md`-named note. → This mirrors exactly the existing markdown-
  link behavior and Obsidian's own resolution rules; it is not a new class
  of ambiguity, and basenames containing a literal `.md` substring are
  already an edge case the existing markdown-link stripping lives with. Not
  mitigated further.
- [Risk] Existing notes with an already-dangling `[[file.md|...]]` link
  stay dangling until next touched (see Non-Goals). → Acceptable per the
  existing "Dangling links are preserved" requirement; a proactive sweep is
  a much larger, separate change (compare `batch-rename-recall-commit`,
  which is the mechanism that *would* need to run vault-wide if a backfill
  were ever wanted).

## Migration Plan

None required. Pure bugfix in a pure function; no data migration, no
schema change, no config flag. Ships in the next release; takes effect on
the next write/edit/rename touching an affected link.
