## MODIFIED Requirements

### Requirement: `evolve_core_persona` tool
The system SHALL expose an `evolve_core_persona` tool that performs atomic full-file writes to the five foundational session files via a single required argument, `updates`: an array of 1 to 5 `{ which, content }` entries with no duplicate `which` values — including for what was previously the "single form," which is now expressed as a one-element `updates` array. `which` SHALL be one of `persona`, `prompt`, `rules`, `user`, `memory`, and SHALL be schema-enforced as an `enum` — it is always required within its entry and is never `null`, so no enum/`null` conflict ever arises for this field. An empty `updates` array or a duplicate `which` SHALL be rejected with `invalid_argument`. The corresponding target file for each entry is the matching `.md` file (e.g. `which=persona` → `PERSONA.md`, `which=memory` → `MEMORY.md`) resolved relative to the agents folder for the active scope.

The tool SHALL enforce a hard line-count cap on the content for the capped files: `which=rules` content MUST NOT exceed 40 lines, `which=user` content MUST NOT exceed 100 lines, and `which=memory` content MUST NOT exceed 200 lines (counted as newline-separated lines). Every entry SHALL be validated — `which` domain, duplicates, line caps, and the write-side link transform — before any file is written; a failing entry SHALL reject the whole call and leave every foundational file unchanged. After validation, each selected file SHALL be replaced atomically. The response SHALL always carry a `results` array of `{ which, bytes_written }` entries in request order, one per update, regardless of how many entries `updates` contains — including exactly one. The call is not transactional across files: a crash mid-apply may leave a prefix of the entries applied, but never a partially written single file.

#### Scenario: Persona update
- **WHEN** the tool is called with `updates=[{which:"persona",content:"..."}]` for the active scope
- **THEN** the scope's `PERSONA.md` is replaced atomically and the response is a success result carrying `results: [{which:"persona", bytes_written: N}]`

#### Scenario: Prompt update
- **WHEN** the tool is called with `updates=[{which:"prompt",content:"..."}]`
- **THEN** the scope's `PROMPT.md` is replaced atomically

#### Scenario: Rules update within cap
- **WHEN** the tool is called with `updates=[{which:"rules",content:"..."}]` and content of 40 lines or fewer
- **THEN** the scope's `RULES.md` is replaced atomically

#### Scenario: Rules over cap rejected
- **WHEN** the tool is called with `updates=[{which:"rules",content:"..."}]` and content of 41 lines
- **THEN** the response is an MCP error with code `invalid_argument` naming the 40-line limit, and `RULES.md` is not changed

#### Scenario: User update within cap
- **WHEN** the tool is called with `updates=[{which:"user",content:"..."}]` and content of 100 lines or fewer
- **THEN** the scope's `USER.md` is replaced atomically

#### Scenario: Memory update within cap
- **WHEN** the tool is called with `updates=[{which:"memory",content:"..."}]` and content of 200 lines or fewer
- **THEN** the scope's `MEMORY.md` is replaced atomically

#### Scenario: A single-entry call still returns the results array shape
- **WHEN** the tool is called with `updates` containing exactly one entry
- **THEN** the response carries `results: [{ which, bytes_written }]` — a one-element array, never the bare `{ bytes_written }` shape the tool returned before this change

#### Scenario: Batch update writes several foundational files in one call
- **WHEN** the tool is called with `updates=[{which:"persona",…},{which:"user",…},{which:"memory",…}]`, every entry within its cap
- **THEN** `PERSONA.md`, `USER.md`, and `MEMORY.md` are each replaced atomically and the response carries `results` with one `{ which, bytes_written }` entry per update, in request order

#### Scenario: One over-cap entry rejects the whole batch
- **WHEN** the tool is called with `updates` containing a valid `persona` entry and a `rules` entry of 41 lines
- **THEN** the response is an MCP error with code `invalid_argument` naming the 40-line limit, and neither `PERSONA.md` nor `RULES.md` is changed

#### Scenario: Duplicate which in a batch is rejected
- **WHEN** the tool is called with `updates` containing two entries with `which="rules"`
- **THEN** the response is an MCP error with code `invalid_argument` and no file is changed

#### Scenario: Empty updates array is rejected
- **WHEN** the tool is called with `updates=[]`
- **THEN** the response is an MCP error with code `invalid_argument` and no file is changed

#### Scenario: User content over the line cap is rejected
- **WHEN** the tool is called with `updates=[{which:"user",content:"..."}]` and content exceeding 100 lines
- **THEN** the response is an MCP error with code `invalid_argument`, the message states the 100-line limit, and `USER.md` is unchanged

#### Scenario: Memory content over the line cap is rejected
- **WHEN** the tool is called with `updates=[{which:"memory",content:"..."}]` and content exceeding 200 lines
- **THEN** the response is an MCP error with code `invalid_argument`, the message states the 200-line limit, and `MEMORY.md` is unchanged

#### Scenario: Invalid `which`
- **WHEN** the tool is called with an `updates` entry whose `which` is set to any value other than the five accepted strings
- **THEN** the call is rejected at schema validation (`which` is a schema-level enum on every entry, since it is always required and never `null`)

#### Scenario: Exactly one argument form
- **WHEN** a client attempts to call the tool with top-level `which`/`content` arguments — the shape this tool accepted before this change, alongside `updates`, as two alternative forms
- **THEN** the call is rejected at schema validation, since `which` and `content` are no longer part of the input schema at all; `updates` is the only argument form the tool now accepts, for both what used to be the single form and the batch form

#### Scenario: Path argument is rejected
- **WHEN** a client attempts to pass a `path` argument to override the hardcoded targets
- **THEN** the call is rejected at schema validation because the input schema does NOT include a path field

#### Scenario: Refused under readonly policy
- **WHEN** policy is `readonly` and `evolve_core_persona` is invoked with a valid `updates` array
- **THEN** the response is an MCP error with code `write_denied`

### Requirement: `update_note_properties` tool
The system SHALL expose an `update_note_properties` tool, available on every build, that merges a JSON object into the frontmatter of the note at the given **vault-root-relative** virtual path and persists atomically under the per-target lock. The object to merge SHALL be supplied as `properties_json`, a required string argument whose content is that object, JSON-encoded; the tool SHALL parse `properties_json` and, if it is not syntactically valid JSON or does not decode to a JSON object, SHALL reject the call with `invalid_argument` and leave the file unchanged. Each key of the decoded object SHALL be upserted with its JSON value (strings, numbers, booleans, arrays, and objects round-trip); a key supplied with an explicit `null` SHALL be deleted. The write-side link transform SHALL be applied to every string value of the decoded properties, recursing into arrays and nested objects: link targets resolving into the caller's own scope are expanded to the suffixed physical form, shared targets are left clean, dangling targets are left verbatim, and a supplied property whose link target resolves into the caller's own scope while the note lives in the shared region SHALL be refused with the cross-scope leak-guard error, leaving the file unchanged. Non-string values SHALL NOT be transformed. The note body SHALL remain byte-identical. A note without a frontmatter block SHALL gain one; a merge whose result is an empty object SHALL remove the block entirely. When the existing leading block looks like frontmatter but is not valid YAML, the tool SHALL refuse with `invalid_argument` and leave the file unchanged. The frontmatter block is re-serialized in normalized form (stable key order; comments and YAML formatting are not preserved). Write gating SHALL match the generic write tools: agents-folder root-level paths are rejected as wrapper-reserved, policy and visibility guards apply, the target must exist (`not_found` otherwise), and the recall index SHALL be updated synchronously. The result SHALL return the full post-update `{ properties }` in the agent-facing clean form (own suffixes stripped), as a JSON object — the response shape is unaffected by `properties_json` being string-encoded on input.

#### Scenario: Upsert and delete in one call
- **WHEN** a note's frontmatter is `{ status: "draft", priority: 2 }` and the tool is called with `properties_json="{\"status\":\"done\",\"reviewed\":true,\"priority\":null}"`
- **THEN** the persisted frontmatter parses as `{ status: "done", reviewed: true }`, the body is byte-identical, and the result echoes the merged set as a JSON object

#### Scenario: Malformed JSON in properties_json is refused
- **WHEN** the tool is called with `properties_json` set to a string that is not syntactically valid JSON (for example a trailing comma or an unterminated object)
- **THEN** the response is an MCP error with code `invalid_argument` and the file is unchanged

#### Scenario: Non-object JSON in properties_json is refused
- **WHEN** the tool is called with `properties_json` set to syntactically valid JSON that decodes to something other than an object (for example an array, a bare string, or a number)
- **THEN** the response is an MCP error with code `invalid_argument` and the file is unchanged

#### Scenario: Own-scope link value is expanded on disk and returned clean
- **WHEN** scope renders to `jarvis.tony` and the tool sets `related: "[[rust]]"` where `rust` resolves to the caller's own `topics/rust.md`
- **THEN** the persisted frontmatter value is `"[[rust.jarvis.tony]]"`, the result echoes `related: "[[rust]]"`, and a subsequent `read_note_properties` returns `"[[rust]]"`

#### Scenario: Shared link value stays clean
- **WHEN** the tool sets `related: "[[release]]"` where `release` resolves to the shared `Actions/release.md`
- **THEN** the persisted value is `"[[release]]"` with no suffix

#### Scenario: Leak guard applies to property values
- **WHEN** policy permits writing the shared note `Actions/release.md` and the tool sets a property containing `[[rust]]` that resolves only into the caller's own scope
- **THEN** the call is refused with the `write_denied`-class cross-scope error naming the target and `Actions/release.md` is unchanged

#### Scenario: Dangling and non-string values are untouched
- **WHEN** the tool sets `related: "[[not-yet-created]]"` (resolving to nothing) and `priority: 2`
- **THEN** both are persisted verbatim

#### Scenario: Block created when absent
- **WHEN** the tool is called against a note with no frontmatter
- **THEN** a `---` fenced block containing the decoded properties is added above the unchanged body

#### Scenario: Emptied block is removed
- **WHEN** the merge deletes every remaining key
- **THEN** the persisted note has no frontmatter fences and the body is unchanged

#### Scenario: Malformed existing frontmatter is refused
- **WHEN** the note begins with a `---` fence whose contents do not parse as YAML
- **THEN** the response is an MCP error with code `invalid_argument` and the file is unchanged

#### Scenario: Updated properties are immediately recallable
- **WHEN** recall runs the tantivy backend and the tool sets `status: "done"` on a note
- **THEN** a subsequent `recall_memory_notes` call with filter `{ key: "status", op: "eq", value: "done" }` returns the note without waiting for the watcher

#### Scenario: Write gating parity
- **WHEN** the tool targets an agents-folder root-level core file, a policy-denied region, a visibility-excluded path, or a missing file
- **THEN** the response carries the same error code the generic write tools would return (`path_not_permitted` naming the wrapper, the policy error, or `not_found`)
