# Proposal

## Why

Agents that write Muninn notes keep falling into the same traps, and the fixes so far live in personal notes instead of the server:

- `edit_memory_note` fails with `edit_search_not_found` when the search string cuts through a link (for example it ends in `rust]] b`). The server expands links in the search string, but half a link cannot be expanded, so it never matches the stored form.
- `path_prefix` is relative to the agents folder, while `glob` and every note path are relative to the vault root. Agents pass `Agents/skills` and get nothing back.
- The default layout does not say how to write links or how to rename a note, although the server rewrites links on every write and read.
- A vault that wants to add its own writing rules must copy the whole default layout into `AGENT_MEMORY_LAYOUT.md`. The copy then stops following server updates.

## What Changes

- `edit_memory_note` matches its search string against the clean view a reader sees. Where the match cuts through a link, the edited range widens to the whole link. Only that range is expanded and written back, so every other stored byte is kept.
- `list_memory_notes` and `recall_memory_notes` accept `path_prefix` either relative to the agents folder (`topics/rust`) or relative to the vault root (`Agents/topics/rust`). A trailing `/` is ignored.
- The default layout gains a "Links between notes" section and documents the two `path_prefix` forms. It still does not mention the per-scope filename suffix.
- A new conventions file is appended after the resolved layout: per-scope `AGENT_MEMORY_CONVENTIONS.md`, else the global file at `MUNINN_MEMORY_CONVENTIONS_FILE` (default `<root>/AGENT_MEMORY_CONVENTIONS.md`), else nothing. A blank file adds nothing. A full `AGENT_MEMORY_LAYOUT.md` override keeps working.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `memory-tools`: edit matching, `path_prefix` forms, default layout content, layout resolution with conventions.
- `configuration`: new `MUNINN_MEMORY_CONVENTIONS_FILE` variable and `--memory-conventions-file` flag.

## Impact

Changes `src/wikilink.rs` (link spans and clean-view edit), `src/storage.rs` (locked closure edit), `src/tools.rs` (edit, list, template paths), `src/recall/mod.rs` and `src/path.rs` (`path_prefix`), `src/session_context.rs` (default layout and conventions), `src/config.rs` and `src/mcp.rs` (new setting), README, tool schema snapshots, and tests. No new dependencies and no vault migration.

Investigated and left out: large writes that arrive cut off and writes that time out on the client. A 112 KB write round-trips byte for byte against a local server, so both come from the client side.
