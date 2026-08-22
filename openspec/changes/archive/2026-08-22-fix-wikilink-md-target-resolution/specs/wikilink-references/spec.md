## MODIFIED Requirements

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
