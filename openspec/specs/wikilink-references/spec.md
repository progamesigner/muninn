# wikilink-references Specification

## Purpose
This capability bidirectionally rewrites link targets between the agent-facing
clean form and the on-disk physical form so that links stay usable by agents and
resolvable by Obsidian at the same time. It covers `[[wikilink]]` targets and
relative markdown link targets. On read, the caller's own scope suffix is stripped
so the agent sees only the shortest unambiguous note name; on write, targets that
resolve into the caller's own scope are expanded to the suffixed, Obsidian-
resolvable physical basename, while targets in the shared region are left clean.
Resolution is performed against the caller's visible set, ambiguous basenames are
qualified with the shortest leading path segments needed to disambiguate, and a
cross-scope leak guard refuses any write that would persist a scope-suffixed link
into a shared file.
## Requirements
### Requirement: Visible-set link resolution

The system SHALL resolve a `[[wikilink]]` target against the caller's visible set
— the union of the caller's own scope (inside the agents folder, suffix stripped)
and the shared region (outside the agents folder) — using Obsidian's basename
matching. The system SHALL NOT resolve a target to any file outside the caller's
visible set.

#### Scenario: Bare name resolves to a unique visible note
- **WHEN** scope is `{agent:"jarvis", user:"tony"}`, the caller's own scope
  contains `topics/rust.md`, and the caller writes a note containing `[[rust]]`
- **THEN** the link resolves to `topics/rust.md` in the caller's own scope

#### Scenario: Target in another scope does not resolve
- **WHEN** the only note with basename `rust` on disk belongs to scope
  `{agent:"jarvis", user:"sam"}` and the caller `{agent:"jarvis", user:"tony"}`
  writes `[[rust]]`
- **THEN** the target does not resolve (it is outside the caller's visible set)
  and the link is treated as dangling and left verbatim

### Requirement: Shortest unambiguous link names

The system SHALL present and accept link targets as the shortest unambiguous note
name. When a bare basename is unique within the caller's visible set, the system
SHALL use the bare basename; when a basename is shared by two or more visible
notes, the system SHALL qualify the target with the shortest leading path segments
that make it unambiguous.

#### Scenario: Unique basename uses the bare name
- **WHEN** the caller's visible set contains exactly one note with basename `rust`
  and the caller reads a note linking to it
- **THEN** the rendered link target is `rust`

#### Scenario: Colliding basename is qualified to the shortest unambiguous path
- **WHEN** the caller's visible set contains both `topics/rust.md` (own scope) and
  `lang/rust.md` (shared) and the caller reads a note linking to the first
- **THEN** the rendered link target is `topics/rust`, not the bare `rust`

### Requirement: Own-scope link expansion on write

On write, for a note inside the caller's own scope, the system SHALL rewrite every
`[[wikilink]]` whose target resolves into the caller's own scope to the suffixed
physical basename, so the persisted link resolves in Obsidian. The system SHALL
leave targets that resolve into the shared region unchanged (no suffix).

#### Scenario: Own-scope target is suffixed on disk
- **WHEN** scope renders to `jarvis.tony`, the caller writes a note inside its own
  scope containing `[[rust]]`, and `rust` resolves to the caller's own
  `topics/rust.md`
- **THEN** the persisted file content contains `[[rust.jarvis.tony]]`

#### Scenario: Shared target is left clean on disk
- **WHEN** the caller writes an own-scope note containing `[[release]]` and
  `release` resolves to the shared `Actions/release.md`
- **THEN** the persisted file content contains `[[release]]` with no suffix

### Requirement: Own-scope suffix stripping on read

On read, the system SHALL strip the caller's own rendered scope suffix from every
`[[wikilink]]` target in the returned content, so the agent sees only clean
shortest names. The agent SHALL never observe a scope suffix in returned content.

#### Scenario: Suffix is stripped on read
- **WHEN** the persisted own-scope note content contains `[[rust.jarvis.tony]]`
  and scope `{agent:"jarvis", user:"tony"}` reads it
- **THEN** the returned content contains `[[rust]]`

#### Scenario: Round-trip is stable
- **WHEN** scope `jarvis.tony` writes content containing `[[rust]]` (resolving in
  its own scope) and then reads the same note
- **THEN** the returned content contains `[[rust]]`, identical to what was written

### Requirement: Cross-scope link leak prevention

The system SHALL reject a write whose content, in a file in the shared region,
contains a `[[wikilink]]` that resolves into the caller's own scope, because the
suffixed physical form would expose the caller's scope to other readers of the
shared file. The error SHALL name the offending target and the file SHALL be left
unchanged.

#### Scenario: Shared file linking to a scoped note is rejected
- **WHEN** policy permits writing the shared file `Actions/release.md`, and the
  caller writes content into it containing `[[rust]]` where `rust` resolves only
  into the caller's own scope
- **THEN** the write is refused with a `write_denied`-class error naming `rust`
  and `Actions/release.md` is unchanged

#### Scenario: Shared file linking to a shared note is allowed
- **WHEN** the caller writes `Actions/release.md` containing `[[changelog]]` where
  `changelog` resolves to the shared `Actions/changelog.md`
- **THEN** the write succeeds and the persisted content contains `[[changelog]]`

### Requirement: Supported link forms

The system SHALL apply the link transform to plain wikilinks `[[target]]`, aliased
wikilinks `[[target|alias]]`, heading links `[[target#heading]]`, embeds
`![[target]]`, and relative markdown links `[text](path.md)`. The system SHALL
rewrite only the target portion and SHALL preserve the alias text, heading anchor,
embed prefix, and markdown link text. The system SHALL leave external
(`http://`, `https://`) and anchor-only (`#section`) markdown link targets
unchanged.

A `[[wikilink]]` target MAY carry a literal `.md` extension (e.g.
`[[file.md]]`, `[[file.md|alias]]`, `[[file.md#heading]]`, `![[file.md]]`).
The system SHALL resolve such a target identically to the same target
without the extension, and SHALL rewrite a resolved wikilink to the
extension-less form regardless of whether the extension was present in the
input, so both spellings converge to the one persisted representation every
other wikilink already uses.

#### Scenario: Alias and heading are preserved
- **WHEN** scope renders to `jarvis.tony` and the caller writes an own-scope note
  containing `[[rust|the Rust note]]` and `[[rust#install]]` resolving to its own
  `rust.md`
- **THEN** the persisted content contains `[[rust.jarvis.tony|the Rust note]]` and
  `[[rust.jarvis.tony#install]]`

#### Scenario: Embed target is rewritten with the prefix preserved
- **WHEN** the caller writes an own-scope note containing `![[rust]]` resolving to
  its own `rust.md` and scope renders to `jarvis.tony`
- **THEN** the persisted content contains `![[rust.jarvis.tony]]`

#### Scenario: Relative markdown link round-trips
- **WHEN** scope renders to `jarvis.tony` and the caller writes an own-scope note
  containing `[see Rust](topics/rust.md)` resolving to its own `topics/rust.md`
- **THEN** the persisted link resolves in Obsidian to the caller's physical file
  and a subsequent read returns `[see Rust](topics/rust.md)`

#### Scenario: External markdown link is untouched
- **WHEN** the caller writes a note containing `[docs](https://example.com)` and
  `[top](#summary)`
- **THEN** both links are persisted and returned verbatim

#### Scenario: Wikilink target with a literal `.md` extension resolves
- **WHEN** scope renders to `jarvis.tony`, the caller's own scope contains
  `topics/rust.md`, and the caller writes a note containing
  `[[rust.md|the Rust note]]`
- **THEN** the target resolves to `topics/rust.md` exactly as `[[rust|the
  Rust note]]` would, and the persisted content contains
  `[[rust.jarvis.tony|the Rust note]]` — the `.md` extension is not carried
  into the rewritten form

#### Scenario: `.md`-suffixed and bare wikilinks to the same note converge
- **WHEN** an own-scope note contains both `[[rust]]` and `[[rust.md]]`,
  both resolving to the same `rust.md`
- **THEN** both are rewritten to the identical suffixed form on disk, and
  both read back as the identical clean form

#### Scenario: `.md`-suffixed wikilink counts as a backlink
- **WHEN** a visible note contains `[[rust.md]]` resolving to `topics/rust.md`
- **THEN** the referring note appears in `topics/rust.md`'s backlinks,
  identical to `[[rust]]`

#### Scenario: `.md`-suffixed wikilink is rewritten on rename
- **WHEN** a visible note contains `![[rust.md]]` resolving to the source of
  a rename
- **THEN** the rename's referrer rewrite retargets it to the destination,
  exactly as it would for `![[rust]]`

### Requirement: Dangling links are preserved

The system SHALL leave a `[[wikilink]]` or relative markdown link whose target
does not resolve in the caller's visible set unchanged on write and on read, and
SHALL NOT retroactively rewrite it if a matching note is later created.

#### Scenario: Unresolved link is preserved verbatim
- **WHEN** the caller writes a note containing `[[not-yet-created]]` with no
  matching note in the visible set
- **THEN** the persisted content contains `[[not-yet-created]]` and a later read
  returns it unchanged, even after a note named `not-yet-created` is created

### Requirement: Backlink resolution is the inverse of forward resolution
The system SHALL be able to compute, for a given target note, the set of visible notes containing at least one link that resolves to that target, using the same resolution rules as the forward link transform: trailing-segment matching against the caller's visible set, own-scope-preferred then lexicographic tie-breaking, and all supported link forms (`[[wikilink]]`, `[[wikilink|alias]]`, `[[wikilink#heading]]`, `![[embed]]`, and relative markdown links ending in `.md`). A link SHALL count toward a target's backlinks only when forward resolution selects exactly that target. Dangling links SHALL count toward no target. Stored own-scope (suffixed) link forms SHALL be interpreted identically to their clean agent-facing forms.

#### Scenario: Suffixed on-disk form resolves like its clean form
- **WHEN** a stored own-scope note contains the on-disk link `[[rust.jarvis.tony]]` (written by the expansion transform for clean target `[[rust]]`)
- **THEN** backlink computation for the note that `[[rust]]` forward-resolves to includes the referring note

#### Scenario: Ambiguous basenames follow the forward tie-break
- **WHEN** two visible notes share the basename `rust` and a note links `[[rust]]`
- **THEN** the link counts as a backlink only for the entry forward resolution selects (own scope preferred, then the lexicographically smallest clean path)

#### Scenario: Dangling links produce no backlinks
- **WHEN** a visible note contains `[[ghost]]` and no visible note resolves that target
- **THEN** the link contributes to no note's backlinks

### Requirement: Incoming references follow a rename
When a note is renamed, the system SHALL rewrite, in every visible referring note, exactly those link targets whose forward resolution selects the renamed note, leaving all other bytes of the referring note untouched. The rewritten target SHALL be derived the same way the write-side transform derives link targets: the shortest unambiguous name for the destination against the post-rename visible set, suffixed for own-scope wikilinks, the vault-root-relative physical path for own-scope markdown links, and the clean form for shared targets. Aliases, headings, and embed markers SHALL be preserved. Links that resolve to other notes — including other notes sharing the old basename — SHALL NOT be modified.

#### Scenario: Only links resolving to the renamed note are touched
- **WHEN** the visible set contains `Agents/topics/rust.md` and `Lang/rust.md`, a referring note links `[[topics/rust]]` and `[[Lang/rust]]`, and `Agents/topics/rust.md` is renamed
- **THEN** only the `[[topics/rust]]` target is rewritten; `[[Lang/rust]]` is byte-identical

#### Scenario: Rewritten names are shortest unambiguous post-rename
- **WHEN** a note is renamed such that its new basename is unique in the visible set
- **THEN** referring wikilinks are rewritten to the bare basename form (with the caller's suffix when the destination is own-scope), not a longer qualified path

#### Scenario: Decorations survive the rewrite
- **WHEN** a referring note links to the renamed note as `![[old]]`, `[[old#h]]`, and `[[old|alias]]`
- **THEN** the rewritten links are `![[new]]`, `[[new#h]]`, and `[[new|alias]]` respectively (with the applicable suffix form on disk)

#### Scenario: Round-trip after rename
- **WHEN** a referring note is rewritten by a rename and subsequently read through `read_memory_note`
- **THEN** the reader sees the clean shortest-name form of the new target, exactly as if the link had been written fresh against the post-rename vault

### Requirement: Frontmatter property values participate in the link transform
The bidirectional link transform SHALL cover link occurrences inside frontmatter property string values uniformly with body content: the same resolution rules (visible-set matching, shortest unambiguous names, own-scope-preferred tie-breaks), the same supported link forms, the same dangling-link preservation, and the same cross-scope leak guard. This SHALL hold regardless of the write path: whole-file writes already scan the literal frontmatter block as content; the property tools (`read_note_properties`, `update_note_properties`) SHALL apply the transform to string values — recursing into arrays and nested objects — so the property surface and the content surface present one consistent agent-facing view in which the caller never observes its own scope suffix.

#### Scenario: Property round-trip matches the body round-trip
- **WHEN** scope `jarvis.tony` writes `related: "[[rust]]"` (resolving to its own `topics/rust.md`) once via `write_memory_note` frontmatter and once via `update_note_properties`
- **THEN** both persist `"[[rust.jarvis.tony]]"` on disk, and both `read_memory_note` (showing the block as content) and `read_note_properties` (showing the parsed value) return the clean `[[rust]]` form

#### Scenario: Obsidian resolvability of property links
- **WHEN** an own-scope note's persisted frontmatter contains `related: "[[rust.jarvis.tony]]"`
- **THEN** the target names the suffixed physical basename, so Obsidian resolves the property link to the caller's physical file

#### Scenario: Property links count toward backlinks
- **WHEN** a visible note's frontmatter contains a property value whose link resolves to a target note
- **THEN** the referring note appears in the target's backlinks, identical to a body link

