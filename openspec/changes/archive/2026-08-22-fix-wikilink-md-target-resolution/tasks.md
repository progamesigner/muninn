## 1. Resolver fix

- [x] 1.1 In `src/wikilink.rs`, `resolve_target`: strip a trailing `.md` from
      a `LinkKind::Wikilink` target before matching, the same way the
      `LinkKind::Markdown` arm already does. Verify by inspection that both
      match arms now perform the same stripping.
- [x] 1.2 Confirm (by reading, no code change expected) that
      `apply_suffix_to_link_target`, `expand_links`, `references_to`, and
      `retarget_links` all render from the resolved index entry rather than
      from the caller's original target text, so the `.md` naturally does
      not survive into the persisted/rendered form. If any of them turns out
      to echo the original target bytes instead, fix that call site too.

## 2. Unit test coverage in `src/wikilink.rs`

- [x] 2.1 Add a `resolve_target` test asserting `[[file.md]]` and `[[file]]`
      resolve to the same `LinkEntry` for a note stored as `file.md`, and
      verify with `cargo test resolve` (or the equivalent full test name).
- [x] 2.2 Extend `expand_links` coverage: a note containing
      `[[rust.md|the Rust note]]` writes out with the same suffixed,
      extension-less form as `[[rust|the Rust note]]` would. Verify with
      `cargo test expand`.
- [x] 2.3 Extend `strip_links`/round-trip coverage: reading back a
      `.md`-suffixed-then-persisted wikilink returns the clean extension-less
      form. Verify with `cargo test strip_of_expand` (extend the existing
      round-trip property test in place rather than duplicating it).
- [x] 2.4 Extend `references_to` coverage: `[[rust.md]]`, `[[rust.md#h]]`,
      `[[rust.md|alias]]`, and `![[rust.md]]` all count as backlinks to the
      note they resolve to, matching the existing non-`.md` cases. Verify
      with `cargo test references_to`.
- [x] 2.5 Extend `retarget_links` coverage: a referrer's `.md`-suffixed
      wikilink is retargeted on rename identically to its extension-less
      equivalent, decorations preserved. Verify with
      `cargo test retarget`.
- [x] 2.6 Add a dangling-link regression case: `[[ghost.md]]` with no
      matching note still resolves to nothing and is left verbatim (same as
      `[[ghost]]` today) — guards against over-eager stripping breaking the
      existing dangling-link contract. Verify with `cargo test dangling` (or
      by name of the extended test).

## 3. Integration-level check

- [x] 3.1 In `tests/wikilinks.rs`, add (or extend an existing) end-to-end
      case exercising `write_memory_note`/`read_memory_note` with a
      `.md`-suffixed wikilink target through the actual tool boundary (not
      just the `wikilink.rs` unit-level API), confirming the persisted file
      contains the suffixed extension-less form and the read-back content is
      clean. Verify with `cargo test --test wikilinks`.

## 4. Whole-suite verification

- [x] 4.1 Run `cargo fmt --check`, `cargo clippy --all-targets`, and
      `cargo test` and confirm all pass before considering the change ready
      to commit (per this repo's `CLAUDE.md` release/commit guidance).
