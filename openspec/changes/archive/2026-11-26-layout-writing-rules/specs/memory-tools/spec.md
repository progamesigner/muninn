# Spec Delta

## MODIFIED Requirements

### Requirement: Memory-layout render and default content
The system SHALL provide a layout render for a scope that resolves the layout template (see *Memory-layout template resolution*) and renders it through the template engine with the scope context, so `{{scope.<key>}}` placeholders in an operator-supplied layout resolve (the compiled-in default contains none). The default layout content SHALL carry the vault-mechanics guidance formerly embedded in the session-context `<MUNINN:LAYOUT>` section: the suggested (non-enforced) memory layout with each entry's purpose (root core files `MEMORY.md`, `RULES.md`, `PERSONA.md`, `PROMPT.md`, `USER.md`, `HEARTBEAT.md`; and subfolders `diary/<YYYY-MM-DD>.md`, `workspaces/INDEX.md` + `workspaces/<project>/<item>.md`, `topics/INDEX.md` + `topics/LOG.md` + `topics/<topic>/<fact>.md`, `skills/<skill>/SKILL.md` + `skills/<skill>/references/<name>.md`, `agents/<subagent>/PROMPT.md` + `agents/<subagent>/<context>.md`); the distinction between **core files** (changed only through the dedicated wrapper tools and subject to the documented caps) and all other paths (an ordinary filesystem the agent reads, writes, and organizes freely), without exposing any internal per-scope filename-suffix mechanism; the path-addressing rule that wrapper tools prepend the agents-folder name automatically while the generic note tools require the agents-folder name as the leading segment of a vault-root-relative path, conveyed without hardcoding a specific agents-folder name (so it stays correct under any `MUNINN_AGENTS_DIR`); the tool-managed-file instructions; and the documented caps `USER.md` ≤ 100 lines and `MEMORY.md` ≤ 200 lines. The default layout content SHALL also state that the `path_prefix` filter accepts both the agents-folder-relative and the vault-root-relative form, and SHALL carry a "Links between notes" section covering: the supported link forms; writing the shortest unambiguous name as reads return it; that the server resolves targets on write and shows clean names on read, keeping unmatched targets as dangling links; that a note outside the agents folder cannot link into it (`write_denied`); that `edit_memory_note` matches against the text as read; and that renaming or moving a note goes through `rename_memory_note`. None of this SHALL mention the per-scope filename suffix. The default layout content SHALL NOT include the missing-files onboarding guidance — that is the renderer's `{{onboarding_directive}}`.

#### Scenario: Layout presents the suggested layout with purposes
- **WHEN** the compiled-in default layout is rendered
- **THEN** it lists the suggested core files and subfolders with each entry's purpose, as non-enforced guidance

#### Scenario: Layout distinguishes core files from a free-form filesystem
- **WHEN** the compiled-in default layout is rendered
- **THEN** it states that core files are changed only through their dedicated wrapper tools and are subject to the documented caps, while every other path behaves like an ordinary filesystem, and it does NOT mention or rely on any internal per-scope filename suffix

#### Scenario: Layout documents the path-addressing rule without hardcoding the agents folder
- **WHEN** the compiled-in default layout is rendered
- **THEN** it states that wrapper tools add the agents-folder prefix automatically and that the generic note tools require the agents-folder name as the leading segment of a vault-root-relative path, with a worked example, and it conveys this without hardcoding a specific agents-folder name so it stays correct under any `MUNINN_AGENTS_DIR`

#### Scenario: Layout omits the onboarding guidance
- **WHEN** the compiled-in default layout is rendered
- **THEN** it does NOT contain the missing-files interview/`evolve_core_persona` guidance, which is rendered instead by the `{{onboarding_directive}}` in the context and bootstrap renders

#### Scenario: Layout documents how to write links
- **WHEN** the compiled-in default layout is rendered
- **THEN** it contains a "Links between notes" section with the shortest-name rule, dangling links, the `write_denied` case, edit matching, and `rename_memory_note`, and it still does NOT contain the word "suffix"

### Requirement: Memory-layout template resolution
The system SHALL resolve the active layout template for a scope using a layered lookup, returning the first layer that exists: (1) a per-scope template file `AGENT_MEMORY_LAYOUT.md` resolved through the scope suffix mechanism inside the agents folder; (2) the global template file at the path configured by `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE` (default `<root>/AGENT_MEMORY_LAYOUT.md`); (3) the compiled-in default layout content. The system SHALL then resolve a conventions file the same way, without a compiled-in default: (1) a per-scope `AGENT_MEMORY_CONVENTIONS.md`; (2) the global file at the path configured by `MUNINN_MEMORY_CONVENTIONS_FILE` (default `<root>/AGENT_MEMORY_CONVENTIONS.md`). When the first conventions file found is not blank, the system SHALL append it after the layout, separated by one blank line, before placeholder rendering. Absence of any layer SHALL never be an error.

#### Scenario: Per-scope layout overrides global
- **WHEN** both a per-scope `AGENT_MEMORY_LAYOUT.md` for the scope and a global layout template file exist
- **THEN** the renderer uses the per-scope layout

#### Scenario: Default layout when nothing exists
- **WHEN** neither a per-scope layout nor the global layout template file exists
- **THEN** the renderer uses the compiled-in default layout content

#### Scenario: Conventions append after the default layout
- **WHEN** no layout file exists and the global conventions file contains `# Vault conventions`
- **THEN** the render is the compiled-in default layout, a blank line, and `# Vault conventions`

#### Scenario: Per-scope conventions override global conventions
- **WHEN** both a per-scope `AGENT_MEMORY_CONVENTIONS.md` and the global conventions file exist
- **THEN** only the per-scope conventions are appended

#### Scenario: Blank conventions add nothing
- **WHEN** the first conventions file found contains only whitespace
- **THEN** the render equals the layout alone

### Requirement: Edit matches the persisted link form

`edit_memory_note` SHALL match its `search_string` against the clean view of the
note, the same text `read_memory_note` returns, and SHALL require exactly one
occurrence in that view. Where the match starts or ends inside a link whose stored
form differs from its clean form, the edited range SHALL widen to the whole link.
The system SHALL apply the write-side link transform to that range with
`replace_string` in place, and SHALL keep every stored byte outside the range.

#### Scenario: Edit search containing a link matches on disk
- **WHEN** the persisted note contains `[[rust.jarvis.tony]]` and
  `edit_memory_note` is called for scope `jarvis.tony` with `search_string`
  containing `[[rust]]`
- **THEN** the search matches the stored line and the edit is applied (it does NOT
  fail with `edit_search_not_found`)

#### Scenario: Edit search may cut through a link
- **WHEN** the persisted note contains `a [[rust.jarvis.tony]] b` and
  `edit_memory_note` is called with `search_string` `rust]] b` and
  `replace_string` `rust]] B`
- **THEN** the persisted content is `a [[rust.jarvis.tony]] B`

#### Scenario: Edit replacement is expanded
- **WHEN** `edit_memory_note` replaces a line with a `replace_string` containing
  `[[guide]]` resolving to the caller's own `guide.md`
- **THEN** the persisted content contains `[[guide.jarvis.tony]]`

#### Scenario: Bytes outside the edit are kept
- **WHEN** the persisted note contains an unexpanded own-scope link `[[rust]]` on
  one line and the edit changes only another line
- **THEN** the `[[rust]]` line is persisted unchanged

#### Scenario: Ambiguity is counted in the clean view
- **WHEN** the note reads `[[rust]] and [[rust]]` and the `search_string` is `rust]]`
- **THEN** the response is an MCP error with code `edit_search_ambiguous`

## ADDED Requirements

### Requirement: `path_prefix` accepts both path forms
`list_memory_notes` and `recall_memory_notes` SHALL accept `path_prefix` either relative to the agents folder (`topics/rust`) or relative to the vault root with the agents-folder name as its first segment (`Agents/topics/rust`). When a value could be read both ways, the vault-root reading SHALL win. A trailing `/` SHALL be ignored. When the agents folder is the vault root, the value SHALL be used as given.

#### Scenario: Agents-folder-relative prefix
- **WHEN** either tool is called with `path_prefix="topics"` and the agents folder is `Agents`
- **THEN** only entries under `Agents/topics` are returned

#### Scenario: Vault-root-relative prefix
- **WHEN** either tool is called with `path_prefix="Agents/topics/"` and the agents folder is `Agents`
- **THEN** the same entries are returned as for `path_prefix="topics"`
