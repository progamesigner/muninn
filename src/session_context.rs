//! The session-context renderer: the single source of the rendered bootstrap
//! shared by the `load_session_context` tool, the `session-context`/
//! `session-bootstrap` resources, the `session-context` prompt, and the
//! `GET /v1/context` and `GET /v1/bootstrap` HTTP endpoints. A companion layout
//! renderer backs the `session-layout` resource and `GET /v1/layout`.
//!
//! [`render_session_context`] resolves a **template** through a layered lookup
//! (per-scope file → global file → compiled-in default), keyed by the
//! [`RenderKind`] (full `Context` vs lean `Bootstrap`), builds a context map
//! from the five foundational files (substituting a sentinel for any that are
//! absent), the scope keys, a scope directive, and an onboarding directive that
//! is non-empty only when foundational files are missing, then renders the
//! template via [`crate::template::Template`]. It never errors on absence — a
//! fresh vault renders instructions-only.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::MuninnError;
use crate::path::VirtualPath;
use crate::storage::Storage;
use crate::template::Template;

/// Which render to produce: the full session context or the lean bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderKind {
    /// The full session context (foundational files + onboarding + layout pointer).
    Context,
    /// The lean session bootstrap (scope + pointers + onboarding + rules).
    Bootstrap,
}

/// The five foundational files, paired as (placeholder leaf, filename). The
/// context key is `files.<leaf>`.
pub const FOUNDATIONAL: &[(&str, &str)] = &[
    ("persona", "PERSONA.md"),
    ("prompt", "PROMPT.md"),
    ("rules", "RULES.md"),
    ("user", "USER.md"),
    ("memory", "MEMORY.md"),
];

/// The per-scope full-context template filename, resolved through the scope
/// suffix mechanism inside the agents folder.
const PER_SCOPE_CONTEXT_FILE: &str = "AGENT_SESSION_CONTEXT.md";
/// The per-scope lean-bootstrap template filename.
const PER_SCOPE_BOOTSTRAP_FILE: &str = "AGENT_SESSION_BOOTSTRAP.md";
/// The per-scope layout template filename.
const PER_SCOPE_LAYOUT_FILE: &str = "AGENT_MEMORY_LAYOUT.md";

/// Substituted for a `{{files.*}}` placeholder whose file does not exist.
const MISSING_SENTINEL: &str = "(not yet recorded — set via evolve_core_persona)";

/// The compiled-in default full-context template. Self-contained: the scope
/// banner, a slot for each foundational file, the onboarding directive (empty in
/// steady state), and a pointer to the layout surface. The tools guide and the
/// layout prose are no longer embedded here.
const DEFAULT_CONTEXT: &str = "\
# Session Context

{{scope_directive}}

<PERSONA>
{{files.persona}}
</PERSONA>

<RULES>
{{files.rules}}
</RULES>

<MEMORY>
{{files.memory}}
</MEMORY>

<USER>
{{files.user}}
</USER>

<PROMPT>
{{files.prompt}}
</PROMPT>

{{onboarding_directive}}
> **Vault layout & conventions.** Read the `session-layout` resource (`muninn://session-layout/…`) or `GET /v1/layout` before organizing memory. Writes that violate the line caps or the wrapper-only rules will error.
";

/// The compiled-in default lean-bootstrap template, ordered server-owned-content
/// first: the `# Session Bootstrap` heading, the scope banner, a pointer to the
/// full context and the layout surface, the onboarding directive, and finally the
/// untagged `RULES.md` contents as the last (truncatable) content. It deliberately
/// omits persona/memory/user/prompt, the wrapper tags, the tools guide, the layout
/// prose, and any server-defined memory loop, to stay within a SessionStart byte
/// budget; memory discipline is left to the user's own `RULES.md`/`PROMPT.md`.
const DEFAULT_BOOTSTRAP: &str = "\
# Session Bootstrap

{{scope_directive}}

> **Lean bootstrap.** Call the `load_session_context` tool for the full context (persona, working memory, user profile, workflow prompt). Read the `session-layout` resource (`muninn://session-layout/…`) or `GET /v1/layout` for vault structure and conventions; writes that violate the line caps or the wrapper-only rules will error.

{{onboarding_directive}}
{{files.rules}}
";

const DEFAULT_LAYOUT: &str = "\
# Memory Layout

The following layout is a suggestion, not a rule. The server enforces only two
things: core files are wrapper-only (see below) and the line caps on `USER.md`
and `MEMORY.md`. Everything else here is guidance you may adapt.

A small set of core files are special: they are changed only through their
dedicated wrapper tools and are bounded by the line caps. Every other path
behaves like an ordinary filesystem — read, write, and organize it however you
like with the generic note tools.

Core files (changed only through the dedicated wrapper tools):
- `PERSONA.md` — your identity, soul, and style.
- `RULES.md` — safety boundaries and hard constraints.
- `MEMORY.md` — your working-memory index (≤ 200 lines). Its internal structure
  is up to you; keep it a concise index, not a dumping ground.
- `USER.md` — the user profile (≤ 100 lines).
- `PROMPT.md` — workflow rules, plus facts about external tools you operate.
- `HEARTBEAT.md` — current task heartbeat.

Addressing: every path below is shown relative to the agents folder. The wrapper
tools (`append_diary_entry`, `evolve_core_persona`, `update_task_heartbeat`) add
your agents-folder name for you. The generic note tools (`write_memory_note`,
`edit_memory_note`, `delete_memory_note`, `read_memory_note`) take the full path
from the vault root, so you must prepend your agents-folder name yourself — e.g.
when the agents folder is `Agents`, write a topic note as
`Agents/topics/<topic>/<fact>.md`, not `topics/<topic>/<fact>.md`. A path without
that leading segment lands outside the agents folder, where most policies are
read-only.

Subfolders (free-form notes via `write_memory_note`/`edit_memory_note`):
- `diary/<YYYY-MM-DD>.md` — daily diary.
- `workspaces/INDEX.md` and `workspaces/<project>/<item>.md` — per-project work.
- `topics/INDEX.md`, `topics/LOG.md`, and `topics/<topic>/<fact>.md` — durable facts.
- `skills/<skill>/SKILL.md` and `skills/<skill>/references/<name>.md` — skills.
- `agents/<subagent>/PROMPT.md` and `agents/<subagent>/<context>.md` — subagents.

How the managed files are written:
- Diary entries are appended with `append_diary_entry` and read back with
  `read_memory_note`; do not hand-write them.
- The task heartbeat is updated through `update_task_heartbeat`, which targets
  `HEARTBEAT.md`.
- The core root files (`PERSONA.md`, `PROMPT.md`, `RULES.md`, `USER.md`,
  `MEMORY.md`) are changed through `evolve_core_persona` — one file via
  `which`/`content`, or several at once via its `updates` batch form. Generic
  `write_memory_note`/`edit_memory_note`/`delete_memory_note` may only target
  paths under a subfolder; root-level core files are reserved for the wrappers.

Line caps (enforced on tool writes): `USER.md` ≤ 100 lines, `MEMORY.md` ≤ 200 lines.
";

/// The rendered session-context plus the foundational files that were absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContext {
    pub rendered: String,
    pub missing: Vec<String>,
}

/// Render the session-context for a validated scope map.
///
/// `scope` must contain exactly the scheme's placeholder keys (the caller
/// validates this). `global_template_file` is the configured global template
/// path for the requested `kind` (may not exist). `kind` selects the full
/// `Context` render or the lean `Bootstrap` render.
pub fn render_session_context(
    storage: &Storage,
    global_template_file: &Path,
    scope: &BTreeMap<String, String>,
    kind: RenderKind,
) -> Result<SessionContext, MuninnError> {
    let resolver = storage.resolver();
    let rendered_scope =
        resolver
            .scheme()
            .render(scope)
            .map_err(|e| MuninnError::InvalidArgument {
                message: e.to_string(),
            })?;

    // --- build the context map + missing list from foundational files ---
    let mut context: BTreeMap<String, String> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    for (leaf, filename) in FOUNDATIONAL {
        let vpath = agents_vpath(storage, filename)?;
        let physical = resolver.resolve(&rendered_scope, &vpath)?;
        let key = format!("files.{leaf}");
        match storage.read(&physical) {
            Ok(content) => {
                // Strip the caller's own suffix from any `[[wikilinks]]` so the
                // rendered core files (e.g. a MEMORY.md index) show clean names.
                let content = if resolver.scheme().is_empty() {
                    content
                } else {
                    crate::wikilink::strip_links(&content, &rendered_scope, resolver)
                };
                context.insert(key, content);
            }
            Err(MuninnError::NotFound { .. }) => {
                context.insert(key, MISSING_SENTINEL.to_string());
                missing.push((*filename).to_string());
            }
            Err(e) => return Err(e),
        }
    }

    // Scope values: `scope.<key>`.
    for (k, v) in scope {
        context.insert(format!("scope.{k}"), v.clone());
    }

    // Prominent scope banner for the top of the document, so the active scope
    // keys survive truncation and tag-stripping by a consuming harness.
    context.insert("scope_directive".to_string(), scope_directive(scope));

    // Onboarding directive: empty in steady state, non-empty when a foundational
    // file is absent — so a fresh/partial scope is prompted to record them.
    context.insert(
        "onboarding_directive".to_string(),
        onboarding_directive(&missing),
    );

    // --- resolve the template source (layered, by kind) and render ---
    let (per_scope_file, default_template) = match kind {
        RenderKind::Context => (PER_SCOPE_CONTEXT_FILE, DEFAULT_CONTEXT),
        RenderKind::Bootstrap => (PER_SCOPE_BOOTSTRAP_FILE, DEFAULT_BOOTSTRAP),
    };
    let source = resolve_template_source(
        storage,
        &rendered_scope,
        per_scope_file,
        global_template_file,
        default_template,
    )?;
    let rendered = Template::parse(&source).render(&context);
    if !rendered.unknown.is_empty() {
        tracing::warn!(
            unknown = ?rendered.unknown,
            "session-context template referenced unrecognised placeholder(s); left literal"
        );
    }

    Ok(SessionContext {
        rendered: rendered.text,
        missing,
    })
}

/// Render the layout document for a validated scope map. Resolves the layout
/// template (per-scope → global → compiled-in default) and substitutes any
/// `{{scope.<key>}}` placeholders an operator override may use; the compiled-in
/// default contains none. Never errors on absence.
pub fn render_layout(
    storage: &Storage,
    global_layout_file: &Path,
    scope: &BTreeMap<String, String>,
) -> Result<String, MuninnError> {
    let resolver = storage.resolver();
    let rendered_scope =
        resolver
            .scheme()
            .render(scope)
            .map_err(|e| MuninnError::InvalidArgument {
                message: e.to_string(),
            })?;

    let mut context: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in scope {
        context.insert(format!("scope.{k}"), v.clone());
    }

    let source = resolve_template_source(
        storage,
        &rendered_scope,
        PER_SCOPE_LAYOUT_FILE,
        global_layout_file,
        DEFAULT_LAYOUT,
    )?;
    let rendered = Template::parse(&source).render(&context);
    if !rendered.unknown.is_empty() {
        tracing::warn!(
            unknown = ?rendered.unknown,
            "layout template referenced unrecognised placeholder(s); left literal"
        );
    }
    Ok(rendered.text)
}

/// Resolve a template source: per-scope file → global file → compiled default.
/// Absence at any layer is non-fatal; genuine IO errors propagate.
fn resolve_template_source(
    storage: &Storage,
    rendered_scope: &str,
    per_scope_file: &str,
    global_template_file: &Path,
    default_template: &str,
) -> Result<String, MuninnError> {
    // (1) per-scope file, via the scope suffix mechanism inside the agents folder.
    let vpath = agents_vpath(storage, per_scope_file)?;
    let physical = storage.resolver().resolve(rendered_scope, &vpath)?;
    match storage.read(&physical) {
        Ok(content) => return Ok(content),
        Err(MuninnError::NotFound { .. }) => {}
        Err(e) => return Err(e),
    }

    // (2) global template file (an absolute/arbitrary path; read directly).
    match std::fs::read_to_string(global_template_file) {
        Ok(content) => return Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(MuninnError::io("reading session-context template", &e)),
    }

    // (3) compiled-in default.
    Ok(default_template.to_string())
}

/// The clean virtual path of a conventional file relative to the agents folder
/// (e.g. `Agents/PERSONA.md`, or `PERSONA.md` when the agents folder is the
/// vault root).
fn agents_vpath(storage: &Storage, relative: &str) -> Result<VirtualPath, MuninnError> {
    let agents = storage.resolver().agents_dir();
    let full = if agents.as_str().is_empty() {
        relative.to_string()
    } else {
        format!("{agents}/{relative}")
    };
    VirtualPath::new(&full)
}

/// Join the active scope into a deterministic `key=value, …` string, or `None`
/// for an empty scope. The single source of truth for how `{{scope_directive}}`
/// names the scope (the `BTreeMap` already yields sorted key order).
fn scope_keys_csv(scope: &BTreeMap<String, String>) -> Option<String> {
    if scope.is_empty() {
        return None;
    }
    Some(
        scope
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// Build the prominent scope banner placed at the very top of the default
/// template. Names the concrete active scope for a non-empty scope; falls back
/// to generic phrasing (naming no specific key) for an empty scope.
fn scope_directive(scope: &BTreeMap<String, String>) -> String {
    match scope_keys_csv(scope) {
        Some(keys) => format!(
            "> **Active memory scope — `{keys}`.** Every Muninn memory tool call MUST \
             carry exactly these scope arguments on every turn — otherwise it errors or \
             reads/writes the wrong vault."
        ),
        None => String::from(
            "> **Active memory scope.** Every Muninn memory tool call MUST carry the \
             scope keys defined by the server's VFS scheme on every turn — otherwise it \
             errors or reads/writes the wrong vault.",
        ),
    }
}

/// Build the onboarding directive for the `{{onboarding_directive}}` slot. The
/// empty string when every foundational file exists; otherwise a directive
/// naming the absent files and instructing the agent to interview the user and
/// commit them via `evolve_core_persona`.
fn onboarding_directive(missing: &[String]) -> String {
    if missing.is_empty() {
        return String::new();
    }
    format!(
        "> **Onboarding needed — these foundational files are not yet recorded: {}.** \
         Interview the user about identity, role, working style, and boundaries, then commit \
         them in a single `evolve_core_persona` call (the `updates` batch form). Distill the \
         answers into concise wording for fast comprehension by future sessions, not a verbatim \
         transcript. Do this before substantive work.",
        missing.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::PathResolver;
    use crate::scheme::Scheme;
    use assert_fs::TempDir;
    use camino::Utf8PathBuf;

    fn storage_for(tmp: &TempDir, scheme: &str) -> Storage {
        let resolver = PathResolver::new(
            tmp.path().canonicalize().unwrap(),
            Utf8PathBuf::from("Agents"),
            Scheme::parse(scheme).unwrap(),
        );
        Storage::new(resolver, true, false, &[])
    }

    fn scope(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn write(tmp: &TempDir, rel: &str, content: &str) {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// No files, no template → compiled-in default context with all sentinels;
    /// all five foundational files reported missing.
    #[test]
    fn empty_vault_renders_default_with_sentinels() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("AGENT_SESSION_CONTEXT.md");
        let sc = render_session_context(
            &storage,
            &global,
            &scope(&[("agent", "c"), ("user", "a")]),
            RenderKind::Context,
        )
        .unwrap();
        assert!(sc.rendered.contains(MISSING_SENTINEL));
        assert_eq!(sc.missing.len(), 5);
        assert!(sc.rendered.contains("# Session Context"));
    }

    /// Foundational files present are substituted; absent ones get the sentinel
    /// and are listed in `missing`.
    #[test]
    fn substitutes_present_files_and_sentinels_absent() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        write(&tmp, "Agents/c.a/PERSONA.c.a.md", "PERSONA-BODY");
        write(&tmp, "Agents/c.a/RULES.c.a.md", "RULES-BODY");
        let global = tmp.path().join("AGENT_SESSION_CONTEXT.md");
        let sc = render_session_context(
            &storage,
            &global,
            &scope(&[("agent", "c"), ("user", "a")]),
            RenderKind::Context,
        )
        .unwrap();
        assert!(sc.rendered.contains("PERSONA-BODY"));
        assert!(sc.rendered.contains("RULES-BODY"));
        assert!(sc.rendered.contains(MISSING_SENTINEL));
        assert_eq!(
            sc.missing,
            vec![
                "PROMPT.md".to_string(),
                "USER.md".to_string(),
                "MEMORY.md".to_string()
            ]
        );
    }

    /// The default context render no longer embeds the tools guide or the layout
    /// prose; it delimits the five foundational slots in order and points to the
    /// layout surface.
    #[test]
    fn context_default_drops_tools_guide_and_layout() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("AGENT_SESSION_CONTEXT.md");
        let sc = render_session_context(
            &storage,
            &global,
            &scope(&[("agent", "c"), ("user", "a")]),
            RenderKind::Context,
        )
        .unwrap();
        // Foundational slots in order.
        let persona = sc.rendered.find("<PERSONA>").unwrap();
        let rules = sc.rendered.find("<RULES>").unwrap();
        let memory = sc.rendered.find("<MEMORY>").unwrap();
        let user = sc.rendered.find("<USER>").unwrap();
        let prompt = sc.rendered.find("<PROMPT>").unwrap();
        assert!(persona < rules && rules < memory && memory < user && user < prompt);
        // No tools guide, no embedded layout prose.
        assert!(!sc.rendered.contains("<MUNINN:TOOLS>"));
        assert!(!sc.rendered.contains("ordinary filesystem"));
        assert!(!sc.rendered.contains("Line caps (enforced"));
        // A pointer to the layout surface is present.
        assert!(sc.rendered.contains("session-layout"));
    }

    /// `{{onboarding_directive}}` is empty when every foundational file exists,
    /// and present (naming an absent file) otherwise — in both kinds.
    #[test]
    fn onboarding_directive_gated_on_missing() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("missing.md");
        let s = scope(&[("agent", "c"), ("user", "a")]);

        // All five present → no onboarding directive, empty `missing`.
        for (_, filename) in FOUNDATIONAL {
            let stem = filename.strip_suffix(".md").unwrap();
            write(&tmp, &format!("Agents/c.a/{stem}.c.a.md"), "BODY");
        }
        for kind in [RenderKind::Context, RenderKind::Bootstrap] {
            let sc = render_session_context(&storage, &global, &s, kind).unwrap();
            assert!(sc.missing.is_empty());
            assert!(!sc.rendered.contains("Onboarding needed"));
        }

        // Remove PERSONA → onboarding directive names it, in both kinds.
        std::fs::remove_file(tmp.path().join("Agents/c.a/PERSONA.c.a.md")).unwrap();
        for kind in [RenderKind::Context, RenderKind::Bootstrap] {
            let sc = render_session_context(&storage, &global, &s, kind).unwrap();
            assert!(sc.rendered.contains("Onboarding needed"));
            assert!(sc.rendered.contains("PERSONA.md"));
        }
    }

    /// The lean bootstrap render carries scope + persona + rules + pointers, and
    /// omits persona, the heavier foundational slots, the wrapper tags, and any
    /// server-defined memory loop; rules render last and untagged.
    #[test]
    fn bootstrap_render_is_lean() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        write(
            &tmp,
            "Agents/jarvis.tony/PERSONA.jarvis.tony.md",
            "PERSONA-BODY",
        );
        write(
            &tmp,
            "Agents/jarvis.tony/RULES.jarvis.tony.md",
            "RULES-BODY",
        );
        let global = tmp.path().join("AGENT_SESSION_BOOTSTRAP.md");
        let sc = render_session_context(
            &storage,
            &global,
            &scope(&[("agent", "jarvis"), ("user", "tony")]),
            RenderKind::Bootstrap,
        )
        .unwrap();
        // Distinct heading and the server-owned lean core.
        assert!(sc.rendered.contains("# Session Bootstrap"));
        assert!(!sc.rendered.contains("# Session Context"));
        assert!(sc.rendered.contains("`agent=jarvis, user=tony`"));
        assert!(sc.rendered.contains("load_session_context"));
        assert!(sc.rendered.contains("session-layout"));
        // Persona is dropped; rules are inlined untagged.
        assert!(!sc.rendered.contains("PERSONA-BODY"));
        assert!(sc.rendered.contains("RULES-BODY"));
        // No wrapper tags, heavier sections, or tools guide.
        assert!(!sc.rendered.contains("<PERSONA>"));
        assert!(!sc.rendered.contains("<RULES>"));
        assert!(!sc.rendered.contains("<MEMORY>"));
        assert!(!sc.rendered.contains("<USER>"));
        assert!(!sc.rendered.contains("<PROMPT>"));
        assert!(!sc.rendered.contains("<MUNINN:TOOLS>"));
        // Rules are the final content: nothing but trailing whitespace follows.
        let rules_at = sc.rendered.find("RULES-BODY").unwrap();
        assert!(sc.rendered[rules_at..].trim_end() == "RULES-BODY");
        // Same absent-files accounting as the full render.
        assert_eq!(
            sc.missing,
            vec![
                "PROMPT.md".to_string(),
                "USER.md".to_string(),
                "MEMORY.md".to_string()
            ]
        );
    }

    /// The scope directive names the concrete active scope as `key=value` pairs
    /// in deterministic key order with an imperative to carry them.
    #[test]
    fn scope_directive_names_concrete_scope() {
        let directive = scope_directive(&scope(&[("agent", "jarvis"), ("user", "tony")]));
        assert!(directive.contains("agent=jarvis, user=tony"));
        assert!(directive.contains("MUST"));
    }

    /// With no scope keys, the directive keeps generic phrasing and names no key.
    #[test]
    fn scope_directive_falls_back_to_generic_phrasing_for_empty_scope() {
        let directive = scope_directive(&BTreeMap::new());
        assert!(directive.contains("defined by the server's VFS scheme"));
        assert!(directive.contains("MUST"));
        assert!(!directive.contains('='));
    }

    /// Both default templates lead with the bare scope banner directly under the
    /// H1 (no enclosing tag). The context render then opens `<PERSONA>` under
    /// `# Session Context`; the bootstrap render uses its distinct
    /// `# Session Bootstrap` heading and carries no wrapper tags at all.
    #[test]
    fn default_templates_lead_with_bare_scope_banner() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("missing.md");
        for kind in [RenderKind::Context, RenderKind::Bootstrap] {
            let sc = render_session_context(
                &storage,
                &global,
                &scope(&[("agent", "jarvis"), ("user", "tony")]),
                kind,
            )
            .unwrap();
            let banner = sc.rendered.find("Active memory scope").unwrap();
            assert!(sc.rendered.contains("`agent=jarvis, user=tony`"));
            match kind {
                RenderKind::Context => {
                    let h1 = sc.rendered.find("# Session Context").unwrap();
                    let persona = sc.rendered.find("<PERSONA>").unwrap();
                    assert!(h1 < banner && banner < persona);
                    // Bare banner: no tag between the H1 and the first slot.
                    assert!(!sc.rendered[h1..persona].contains('<'));
                }
                RenderKind::Bootstrap => {
                    let h1 = sc.rendered.find("# Session Bootstrap").unwrap();
                    assert!(h1 < banner);
                    // No persona slot or wrapper tags anywhere in the bootstrap,
                    // and the banner sits bare directly under the H1.
                    assert!(!sc.rendered.contains("<PERSONA>"));
                    assert!(!sc.rendered[h1..banner].contains('<'));
                }
            }
        }
    }

    /// `{{files.user}}` (file contents) and `{{scope.user}}` (scope value) are
    /// distinct in a context template.
    #[test]
    fn file_and_scope_namespaces_are_distinct() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        write(&tmp, "Agents/c.tony/USER.c.tony.md", "USER-FILE-BODY");
        let global = tmp.path().join("missing.md");
        write(
            &tmp,
            "Agents/c.tony/AGENT_SESSION_CONTEXT.c.tony.md",
            "file={{files.user}} scope={{scope.user}}",
        );
        let sc = render_session_context(
            &storage,
            &global,
            &scope(&[("agent", "c"), ("user", "tony")]),
            RenderKind::Context,
        )
        .unwrap();
        assert_eq!(sc.rendered, "file=USER-FILE-BODY scope=tony");
    }

    /// Context resolution: per-scope `AGENT_SESSION_CONTEXT.md` wins over the
    /// global file, which wins over the compiled default.
    #[test]
    fn context_layered_resolution() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("GLOBAL.md");
        let s = scope(&[("agent", "c"), ("user", "a")]);

        let sc = render_session_context(&storage, &global, &s, RenderKind::Context).unwrap();
        assert!(sc.rendered.contains("# Session Context"));

        std::fs::write(&global, "GLOBAL-CONTEXT").unwrap();
        let sc = render_session_context(&storage, &global, &s, RenderKind::Context).unwrap();
        assert_eq!(sc.rendered, "GLOBAL-CONTEXT");

        write(
            &tmp,
            "Agents/c.a/AGENT_SESSION_CONTEXT.c.a.md",
            "PER-SCOPE-CONTEXT",
        );
        let sc = render_session_context(&storage, &global, &s, RenderKind::Context).unwrap();
        assert_eq!(sc.rendered, "PER-SCOPE-CONTEXT");
    }

    /// Bootstrap resolution uses its own per-scope file `AGENT_SESSION_BOOTSTRAP.md`
    /// and global path, independent of the context template.
    #[test]
    fn bootstrap_layered_resolution() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("GLOBAL_BOOTSTRAP.md");
        let s = scope(&[("agent", "c"), ("user", "a")]);

        std::fs::write(&global, "GLOBAL-BOOTSTRAP").unwrap();
        let sc = render_session_context(&storage, &global, &s, RenderKind::Bootstrap).unwrap();
        assert_eq!(sc.rendered, "GLOBAL-BOOTSTRAP");

        write(
            &tmp,
            "Agents/c.a/AGENT_SESSION_BOOTSTRAP.c.a.md",
            "PER-SCOPE-BOOTSTRAP",
        );
        let sc = render_session_context(&storage, &global, &s, RenderKind::Bootstrap).unwrap();
        assert_eq!(sc.rendered, "PER-SCOPE-BOOTSTRAP");
    }

    /// The default layout carries the vault-mechanics guidance and the caps, and
    /// omits the missing-files onboarding paragraph.
    #[test]
    fn layout_default_content() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("missing.md");
        let rendered =
            render_layout(&storage, &global, &scope(&[("agent", "c"), ("user", "a")])).unwrap();
        assert!(rendered.contains("# Memory Layout"));
        assert!(rendered.contains("ordinary filesystem"));
        assert!(rendered.contains("relative to the agents folder"));
        assert!(rendered.contains("prepend your agents-folder name"));
        assert!(rendered.contains("USER.md` ≤ 100 lines"));
        assert!(rendered.contains("MEMORY.md` ≤ 200 lines"));
        // The internal suffix mechanism is not exposed.
        assert!(!rendered.contains("suffix"));
        // Onboarding guidance lives in the renderer, not the layout.
        assert!(!rendered.contains("Onboarding needed"));
        assert!(!rendered.contains("interview the user"));
    }

    /// Layout resolution: per-scope `AGENT_MEMORY_LAYOUT.md` wins over global,
    /// which wins over the compiled default, and `{{scope.*}}` substitutes.
    #[test]
    fn layout_layered_resolution_and_scope_substitution() {
        let tmp = TempDir::new().unwrap();
        let storage = storage_for(&tmp, "<agent>.<user>");
        let global = tmp.path().join("GLOBAL_LAYOUT.md");
        let s = scope(&[("agent", "c"), ("user", "a")]);

        let rendered = render_layout(&storage, &global, &s).unwrap();
        assert!(rendered.contains("# Memory Layout"));

        std::fs::write(&global, "GLOBAL-LAYOUT").unwrap();
        let rendered = render_layout(&storage, &global, &s).unwrap();
        assert_eq!(rendered, "GLOBAL-LAYOUT");

        write(
            &tmp,
            "Agents/c.a/AGENT_MEMORY_LAYOUT.c.a.md",
            "layout for {{scope.agent}}",
        );
        let rendered = render_layout(&storage, &global, &s).unwrap();
        assert_eq!(rendered, "layout for c");
    }
}
