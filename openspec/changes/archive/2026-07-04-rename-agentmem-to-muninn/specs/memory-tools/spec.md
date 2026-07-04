## MODIFIED Requirements

### Requirement: Common tool input contract
The system SHALL ensure every tool's input schema includes the scope parameters whose names are the placeholder idents of `MUNINN_VFS_SCHEME`, and SHALL reject calls whose scope arguments do not satisfy that contract.

#### Scenario: All scheme keys required
- **WHEN** scheme is `<agent>.<user>` and a tool is called with `agent` set but `user` missing
- **THEN** the call is rejected with code `missing_scope` and the message names `user`

#### Scenario: Unexpected scope parameter
- **WHEN** scheme is `<agent>` and a tool is called with both `agent` and `user`
- **THEN** the call is rejected at schema validation because the input schema does NOT include `user` under this scheme

#### Scenario: Custom scheme keys are honoured
- **WHEN** scheme is `<team>.<agent>.<env>.<user>` and a tool is called with exactly those four fields
- **THEN** the call proceeds to resolution with the rendered suffix `<team>.<agent>.<env>.<user>`

#### Scenario: Empty scheme requires no scope arguments
- **WHEN** scheme is the empty string and a tool is called with no scope fields
- **THEN** the call proceeds; if any scope field is supplied, the call is rejected at schema validation

### Requirement: Session-context template
The system SHALL treat the (full) session-context template as an operator-authored markdown document that may contain `{{files.<name>}}`, `{{scope.<key>}}`, `{{scope_directive}}`, and `{{onboarding_directive}}` placeholders, where `<name>` is one of `persona`, `prompt`, `rules`, `user`, `memory` and `<key>` is a scheme placeholder. It SHALL NOT define a `{{tools_guide}}` placeholder. The placeholder namespace SHALL keep file contents (`{{files.user}}`) distinct from scope values (`{{scope.user}}`). The system SHALL ship a compiled-in default `context` template that delimits each section with XML-style tags rather than `##` headings, so that embedded foundational-file markdown (which typically begins at H2) does not collide with the template's own structure. The default template SHALL lead, directly under the `# Session Context` heading and before the first tag, with the `{{scope_directive}}` placeholder rendered as a **bare banner** (not wrapped in any XML tag). The default template SHALL wrap the foundational (agent-owned) slots in bare tags — `<PERSONA>{{files.persona}}</PERSONA>`, `<RULES>{{files.rules}}</RULES>`, `<MEMORY>{{files.memory}}</MEMORY>`, `<USER>{{files.user}}</USER>`, `<PROMPT>{{files.prompt}}</PROMPT>` — in the section order `PERSONA`, `RULES`, `MEMORY`, `USER`, `PROMPT` after the leading scope banner, followed by the `{{onboarding_directive}}` slot and a single-line pointer directing the agent to the layout surface (`muninn://session-layout` / `GET /v1/layout`) for vault mechanics. The default `context` template SHALL NOT embed the `<MUNINN:TOOLS>` tools-guide section nor the `<MUNINN:LAYOUT>` prose; the layout guidance lives in the memory-layout capability. The default template SHALL leave the internal organization of `MEMORY.md` to the agent/user rather than prescribing a skeleton.

#### Scenario: Default context template leads with a bare scope banner
- **WHEN** the compiled-in default `context` template is rendered
- **THEN** the `{{scope_directive}}` banner appears directly under the `# Session Context` heading and before the `<PERSONA>` tag, rendered as bare markdown that is not enclosed in any XML tag

#### Scenario: Default context template no longer embeds tools guide or layout
- **WHEN** the compiled-in default `context` template is rendered
- **THEN** the output delimits its foundational sections with XML-style tags in the order `<PERSONA>`, `<RULES>`, `<MEMORY>`, `<USER>`, `<PROMPT>`, includes the `{{onboarding_directive}}` slot and a one-line pointer to the layout surface, and does NOT contain an `<MUNINN:TOOLS>` section, a `{{tools_guide}}` value, or the `<MUNINN:LAYOUT>` prose

#### Scenario: Sections are delimited by tags, not H2 headings
- **WHEN** the compiled-in default `context` template is rendered
- **THEN** each foundational file's contents are wrapped in a bare tag (`<PERSONA>…</PERSONA>`, `<RULES>…</RULES>`, `<MEMORY>…</MEMORY>`, `<USER>…</USER>`, `<PROMPT>…</PROMPT>`), so embedded H2 markdown in a foundational file does not collide with the template's section delimiters

#### Scenario: File and scope placeholders are distinct
- **WHEN** a template uses both `{{files.user}}` and `{{scope.user}}`
- **THEN** the former renders the contents of `USER.md` and the latter renders the `user` scope key value

#### Scenario: Memory file placeholder is recognised
- **WHEN** a template uses `{{files.memory}}`
- **THEN** the renderer substitutes the contents of `MEMORY.md` (or the missing sentinel when absent)

### Requirement: Session-bootstrap render and default template
The system SHALL provide a lean `bootstrap` render whose compiled-in default template is compact and ordered server-owned-content-first, containing, in order: a `# Session Bootstrap` heading (distinct from the full `context` render's `# Session Context` heading, so the two surfaces are visibly different documents); the bare `{{scope_directive}}` banner; a single-line pointer directing the agent to call `load_session_context` for the rest of the foundational context (persona, working memory, user profile, workflow prompt) and to read the layout surface (`muninn://session-layout` / `GET /v1/layout`) for vault mechanics; the `{{onboarding_directive}}` slot; and finally the `{{files.rules}}` slot rendered without any wrapping tag, as the last content in the document. The default `bootstrap` template SHALL NOT include the `{{files.persona}}`, `{{files.memory}}`, `{{files.user}}`, or `{{files.prompt}}` slots, any `<PERSONA>`/`<RULES>` wrapper tag, the tools guide, the layout prose, or any server-defined memory-loop or recall/diary directive. The `bootstrap` render SHALL report the same absent-foundational-files list as the `context` render for the same scope.

#### Scenario: Bootstrap render carries the compact core in order
- **WHEN** the `bootstrap` render is produced for a scope whose `RULES.md` exists
- **THEN** the output begins with the `# Session Bootstrap` heading, followed by the bare scope banner and a single-line pointer to `load_session_context` and the layout surface, and the `RULES.md` contents appear last with no wrapping tag

#### Scenario: Bootstrap render omits persona and the heavier sections
- **WHEN** the compiled-in default `bootstrap` template is rendered
- **THEN** the output does NOT contain a `<PERSONA>`, `<RULES>`, `<MEMORY>`, `<USER>`, or `<PROMPT>` tag, does NOT contain the persona contents, and does NOT contain an `<MUNINN:TOOLS>` section or the layout prose

#### Scenario: Bootstrap heading is distinct from the full context render
- **WHEN** the compiled-in default `bootstrap` template and the compiled-in default `context` template are each rendered for the same scope
- **THEN** the `bootstrap` render leads with `# Session Bootstrap` and the `context` render leads with `# Session Context`

#### Scenario: Rules are the final content
- **WHEN** the `bootstrap` render is produced for a scope whose `RULES.md` exists
- **THEN** the `RULES.md` contents appear after the scope banner, the `load_session_context`/layout pointer, and the `{{onboarding_directive}}` slot, with no template content following them

#### Scenario: Bootstrap carries no server-defined memory loop
- **WHEN** the compiled-in default `bootstrap` template is rendered for any scope
- **THEN** the output contains no server-authored recall/capture/diary directive — any such memory-discipline guidance present comes solely from the inlined `RULES.md` contents

#### Scenario: Bootstrap render surfaces the onboarding directive
- **WHEN** the `bootstrap` render is produced for a scope with one or more absent foundational files
- **THEN** the `{{onboarding_directive}}` slot renders the interview-and-`evolve_core_persona` directive; for a scope whose files all exist it renders empty

### Requirement: Session-bootstrap template resolution
The system SHALL resolve the active `bootstrap` template for a scope using a layered lookup, returning the first layer that exists: (1) a per-scope template file `AGENT_SESSION_BOOTSTRAP.md` resolved through the scope suffix mechanism inside the agents folder; (2) the global template file at the path configured by `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE` (default `<root>/AGENT_SESSION_BOOTSTRAP.md`); (3) the compiled-in default `bootstrap` template. Absence of any layer SHALL never be an error.

#### Scenario: Per-scope bootstrap template overrides global
- **WHEN** both a per-scope `AGENT_SESSION_BOOTSTRAP.md` for the scope and a global bootstrap template file exist
- **THEN** the renderer uses the per-scope template

#### Scenario: Default bootstrap template when nothing exists
- **WHEN** neither a per-scope bootstrap template nor the global bootstrap template file exists
- **THEN** the renderer uses the compiled-in default `bootstrap` template

### Requirement: Memory-layout render and default content
The system SHALL provide a layout render for a scope that resolves the layout template (see *Memory-layout template resolution*) and renders it through the template engine with the scope context, so `{{scope.<key>}}` placeholders in an operator-supplied layout resolve (the compiled-in default contains none). The default layout content SHALL carry the vault-mechanics guidance formerly embedded in the session-context `<MUNINN:LAYOUT>` section: the suggested (non-enforced) memory layout with each entry's purpose (root core files `MEMORY.md`, `RULES.md`, `PERSONA.md`, `PROMPT.md`, `USER.md`, `HEARTBEAT.md`; and subfolders `diary/<YYYY-MM-DD>.md`, `workspaces/INDEX.md` + `workspaces/<project>/<item>.md`, `topics/INDEX.md` + `topics/LOG.md` + `topics/<topic>/<fact>.md`, `skills/<skill>/SKILL.md` + `skills/<skill>/references/<name>.md`, `agents/<subagent>/PROMPT.md` + `agents/<subagent>/<context>.md`); the distinction between **core files** (changed only through the dedicated wrapper tools and subject to the documented caps) and all other paths (an ordinary filesystem the agent reads, writes, and organizes freely), without exposing any internal per-scope filename-suffix mechanism; the path-addressing rule that wrapper tools prepend the agents-folder name automatically while the generic note tools require the agents-folder name as the leading segment of a vault-root-relative path, conveyed without hardcoding a specific agents-folder name (so it stays correct under any `MUNINN_AGENTS_DIR`); the tool-managed-file instructions; and the documented caps `USER.md` ≤ 100 lines and `MEMORY.md` ≤ 200 lines. The default layout content SHALL NOT include the missing-files onboarding guidance — that is the renderer's `{{onboarding_directive}}`.

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

### Requirement: Memory-layout template resolution
The system SHALL resolve the active layout template for a scope using a layered lookup, returning the first layer that exists: (1) a per-scope template file `AGENT_MEMORY_LAYOUT.md` resolved through the scope suffix mechanism inside the agents folder; (2) the global template file at the path configured by `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE` (default `<root>/AGENT_MEMORY_LAYOUT.md`); (3) the compiled-in default layout content. Absence of any layer SHALL never be an error.

#### Scenario: Per-scope layout overrides global
- **WHEN** both a per-scope `AGENT_MEMORY_LAYOUT.md` for the scope and a global layout template file exist
- **THEN** the renderer uses the per-scope layout

#### Scenario: Default layout when nothing exists
- **WHEN** neither a per-scope layout nor the global layout template file exists
- **THEN** the renderer uses the compiled-in default layout content

### Requirement: Session-context template resolution
The system SHALL resolve the active session-context template for a scope using a layered lookup, returning the first layer that exists: (1) a per-scope template file `AGENT_SESSION_CONTEXT.md` resolved through the scope suffix mechanism inside the agents folder; (2) the global template file at the path configured by `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE` (default `<root>/AGENT_SESSION_CONTEXT.md`); (3) the compiled-in default template. Absence of any layer SHALL never be an error.

#### Scenario: Per-scope template overrides global
- **WHEN** both a per-scope `AGENT_SESSION_CONTEXT.md` for the scope and a global template file exist
- **THEN** the renderer uses the per-scope template

#### Scenario: Global template used when no per-scope template
- **WHEN** no per-scope template exists for the scope but the global template file exists
- **THEN** the renderer uses the global template file

#### Scenario: Default used when nothing exists
- **WHEN** neither a per-scope template nor the global template file exists
- **THEN** the renderer uses the compiled-in default template
