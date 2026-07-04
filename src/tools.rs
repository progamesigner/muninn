//! The agent-facing tool surface: schema generation, scope extraction, and the
//! tool handlers (the fourteen memory-note tools plus `recall_memory_notes` when
//! the recall backend is enabled).
//!
//! Each tool's input schema is assembled at startup by merging the
//! scheme-derived scope fields (see [`crate::scheme::Scheme::to_json_schema`])
//! with the tool-specific fields generated from a `schemars`-derived struct. At
//! call time the scope keys are extracted and validated, the virtual path is
//! resolved under the caller's own scope, the policy gate runs before any IO, and
//! visibility filters reject hidden/ignored targets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{LocalResult, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use rmcp::model::{CallToolResult, Content, JsonObject, Tool};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::config::Grant;
use crate::error::MuninnError;
use crate::path::{PhysicalPath, VirtualPath};
use crate::policy::{Policy, PolicyError, Region};
use crate::recall::{FilterOp, PropertyFilter, RecallEngine, RecallQuery};
use crate::scheme::Scheme;
use crate::storage::{Cursor, LinkEntry, Storage};

/// The default page size for `list_memory_notes`.
const DEFAULT_LIMIT: u64 = 200;
/// The maximum permitted page size.
const MAX_LIMIT: u64 = 1000;
/// The maximum number of paths accepted by `read_memory_notes`.
const MAX_BATCH_READ: usize = 20;
/// The maximum number of entries accepted by `write_memory_notes`.
const MAX_BATCH_WRITE: usize = 20;
/// The maximum number of entries accepted by `evolve_core_persona`'s batch form
/// (one per foundational file).
const MAX_BATCH_EVOLVE: usize = 5;

/// The set of tool names this server exposes, in advertised order.
pub const TOOL_NAMES: &[&str] = &[
    "list_memory_notes",
    "read_memory_note",
    "read_memory_notes",
    "write_memory_note",
    "write_memory_notes",
    "edit_memory_note",
    "delete_memory_note",
    "rename_memory_note",
    "read_note_properties",
    "update_note_properties",
    "load_session_context",
    "evolve_core_persona",
    "update_task_heartbeat",
    "append_diary_entry",
];

// --- schemars structs (tool-specific fields only; scope fields are merged in) ---

#[derive(JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ListFields {
    /// Optional virtual-path prefix, relative to the agents folder, to filter by.
    #[serde(default)]
    path_prefix: Option<String>,
    /// Optional glob pattern matched against each entry's clean, vault-root-relative
    /// virtual path (e.g. `Agents/diary/2026-*`, `**/release.md`). Supports `*`, `**`,
    /// `?`, and character classes. Composes with `path_prefix` via AND (both must
    /// match). Filters paths only; reads no note contents.
    #[serde(default)]
    glob: Option<String>,
    /// Optional ordering of results by clean virtual path: `name_asc` (the
    /// default) returns ascending path order; `name_desc` returns descending.
    /// Ordering is applied before pagination, so `limit`/`cursor` page over the
    /// ordered set.
    #[serde(default)]
    order: Option<String>,
    /// Maximum number of entries to return (default 200, maximum 1000).
    #[serde(default)]
    limit: Option<u64>,
    /// Opaque pagination cursor returned by a previous call.
    #[serde(default)]
    cursor: Option<String>,
    /// Optional selector for what the items represent: `files` (the default)
    /// returns individual note virtual paths; `dirs` returns the distinct
    /// directory virtual paths derived from the visible set — every ancestor
    /// directory of a visible note, deduplicated and deterministically ordered.
    /// The `dirs` view honors `path_prefix`/`glob` and pagination and reads no
    /// note contents.
    #[serde(default)]
    view: Option<String>,
}

/// Result ordering for `list_memory_notes`, by clean virtual path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListOrder {
    NameAsc,
    NameDesc,
}

/// What `list_memory_notes` items represent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListView {
    Files,
    Dirs,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct PathFields {
    /// The virtual path of the note, relative to the vault root.
    path: String,
}

/// One `read_memory_notes` entry: a bare virtual path (the whole note) or an
/// object requesting a line range of that note.
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
enum BatchReadEntry {
    /// A virtual path, relative to the vault root; the whole note is returned.
    Path(String),
    /// A path with an optional line range, with the same `offset`/`limit`
    /// semantics as `read_memory_note`; the entry's result additionally carries
    /// `total_lines`.
    Ranged {
        /// The virtual path of the note, relative to the vault root.
        path: String,
        /// 1-based line number of the first returned line.
        #[schemars(range(min = 1))]
        offset: Option<u64>,
        /// Maximum number of lines to return (default: all remaining lines).
        #[schemars(range(min = 1))]
        limit: Option<u64>,
    },
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct BatchReadFields {
    /// 1 to 20 entries, each either a vault-root-relative virtual path string
    /// (the whole note) or an object `{ path, offset?, limit? }` requesting a
    /// line range of that note. The result carries one `notes` entry per
    /// requested path, in request order: `{ path, content }` on success or
    /// `{ path, error: { code, message } }` on failure, plus `total_lines` when
    /// a range was requested. Per-path failures (e.g. `not_found`,
    /// `path_not_permitted`) do not fail the call.
    paths: Vec<BatchReadEntry>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ReadFields {
    /// The virtual path of the note, relative to the vault root.
    path: String,
    /// Optional 1-based line number of the first returned line (default 1).
    /// When `offset` or `limit` is supplied, the structured result additionally
    /// carries `total_lines` — the line count of the full note — and an offset
    /// past the last line returns empty content rather than an error.
    #[schemars(range(min = 1))]
    offset: Option<u64>,
    /// Optional maximum number of lines to return, counted from `offset`
    /// (default: all remaining lines).
    #[schemars(range(min = 1))]
    limit: Option<u64>,
    /// When `true`, the structured result additionally carries a `backlinks`
    /// array: the clean virtual path of every visible note containing at least
    /// one link that resolves to this note, deduplicated and sorted ascending.
    /// Absent or `false` leaves the response unchanged.
    backlinks: Option<bool>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct WriteFields {
    /// The virtual path of the note, relative to the vault root.
    path: String,
    /// The full new contents of the note; with `append: true`, the bytes to
    /// append instead.
    content: String,
    /// When `true`, `content` is appended to the existing note verbatim —
    /// exact bytes, no implicit separator — and a missing note is created with
    /// `content` as its full body. Absent or `false` replaces the whole note.
    append: Option<bool>,
}

/// One `write_memory_notes` entry: a note to write, with the same per-entry
/// semantics as `write_memory_note`.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct BatchWriteEntry {
    /// The virtual path of the note, relative to the vault root.
    path: String,
    /// The full new contents of the note; with `append: true`, the bytes to
    /// append instead.
    content: String,
    /// When `true`, `content` is appended to the existing note verbatim —
    /// exact bytes, no implicit separator — and a missing note is created with
    /// `content` as its full body. Absent or `false` replaces the whole note.
    append: Option<bool>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct BatchWriteFields {
    /// 1 to 20 entries, each `{ path, content, append? }` with the same
    /// per-entry semantics as `write_memory_note`. Every entry is fully
    /// validated (path, policy, visibility, link expansion) before any file is
    /// touched: any failure rejects the whole call with no writes, as do an
    /// empty or oversized array, a malformed entry, and two entries resolving
    /// to the same virtual path. A `[[wikilink]]` may target a note created by
    /// another entry of the same call — resolution is order-independent. The
    /// result carries one `{ path, bytes_written }` entry per note, in request
    /// order. Each applied entry is atomic and indexed for recall
    /// synchronously, but the batch is not transactional across entries: a
    /// crash mid-apply may leave a prefix of the batch applied.
    notes: Vec<BatchWriteEntry>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EditFields {
    /// The virtual path of the note, relative to the vault root.
    path: String,
    /// The exact string to replace. It must occur exactly once in the file.
    search_string: String,
    /// The replacement string.
    replace_string: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct RenameFields {
    /// The current virtual path of the note, relative to the vault root.
    path: String,
    /// The destination virtual path, relative to the vault root. Must not
    /// already name an existing note.
    new_path: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct PropertiesReadFields {
    /// The virtual path of the note, relative to the vault root.
    path: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct PropertiesUpdateFields {
    /// The virtual path of the note, relative to the vault root. The note must
    /// already exist.
    path: String,
    /// The properties to merge into the note's frontmatter. Each key is upserted
    /// with its JSON value (strings, numbers, booleans, arrays, and nested
    /// objects round-trip); a key supplied with an explicit `null` is deleted.
    properties: Map<String, Value>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EmptyFields {}

/// Which foundational session file `evolve_core_persona` targets.
#[derive(JsonSchema, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum Which {
    Persona,
    Prompt,
    Rules,
    User,
    Memory,
}

/// One batch `evolve_core_persona` entry: a foundational file and its full new
/// contents, with the same per-entry semantics as the single form.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct EvolveUpdateEntry {
    /// Which foundational file to replace: one of persona, prompt, rules, user, memory.
    which: Which,
    /// The full new contents of the selected file. `rules` content must be ≤ 40
    /// lines, `user` content ≤ 100 lines, and `memory` content ≤ 200 lines.
    content: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EvolveFields {
    /// Single form: which foundational file to replace — one of persona, prompt,
    /// rules, user, memory. Requires `content`. Exactly one of the single form
    /// (`which` + `content`) or the batch form (`updates`) must be supplied;
    /// neither or both is rejected.
    which: Option<Which>,
    /// Single form: the full new contents of the selected file. `rules` content
    /// must be ≤ 40 lines, `user` content ≤ 100 lines, and `memory` content
    /// ≤ 200 lines.
    content: Option<String>,
    /// Batch form: 1 to 5 `{ which, content }` entries, each with the same
    /// semantics as the single form; duplicate `which` values are rejected.
    /// Every entry is validated — `which` domain, line caps, link expansion —
    /// before any file is written: any failure rejects the whole call leaving
    /// every foundational file unchanged. Entries are applied in request order;
    /// the result carries one `{ which, bytes_written }` entry per update, in
    /// request order. Each applied entry is atomic, but the batch is not
    /// transactional across files: a crash mid-apply may leave a prefix of the
    /// entries applied.
    updates: Option<Vec<EvolveUpdateEntry>>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ContentOnlyFields {
    /// The content to write.
    content: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct DiaryFields {
    /// The diary entry body.
    content: String,
    /// Optional short title for the entry; when present the heading reads
    /// `## <HH:MM:SS> — <title>`, otherwise `## <HH:MM:SS>`.
    title: Option<String>,
}

/// The comparison a frontmatter property filter applies.
#[derive(JsonSchema, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum FilterOpField {
    Exists,
    Eq,
    Contains,
    Gt,
    Lt,
    Ge,
    Le,
}

/// One frontmatter property predicate (tantivy backend only).
#[derive(JsonSchema)]
#[allow(dead_code)]
struct PropertyFilterField {
    /// The frontmatter property key.
    key: String,
    /// The comparison: exists, eq, contains, gt, lt, ge, le.
    op: FilterOpField,
    /// The value to compare against (omitted for `exists`).
    value: Option<String>,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct RecallFields {
    /// Full-text query. On the simple backend this is a case-insensitive
    /// substring match; on the tantivy backend it is BM25-ranked.
    query: Option<String>,
    /// Regular-expression query matched over note content.
    regex: Option<String>,
    /// Frontmatter property filters. Requires the tantivy backend; rejected with
    /// `unsupported` on the simple backend.
    filters: Option<Vec<PropertyFilterField>>,
    /// Include only notes modified at or after this time. Accepts an RFC 3339
    /// timestamp, or a bare `YYYY-MM-DD` date interpreted as start of day in the
    /// configured `MUNINN_TIMEZONE`. Bounds are half-open
    /// (`modified_after ≤ mtime < modified_before`) and compare the filesystem
    /// mtime, which restores and sync tools may have back-dated. Counts as a
    /// sufficient predicate on its own: with no `query`/`regex`/`filters`, hits
    /// are ordered by `modified_at` descending with `score: 1.0` and empty
    /// `snippets`.
    modified_after: Option<String>,
    /// Include only notes modified strictly before this time. Same accepted
    /// formats, half-open semantics, and mtime caveat as `modified_after`.
    modified_before: Option<String>,
    /// Optional virtual-path prefix, relative to the agents folder, to filter by.
    path_prefix: Option<String>,
    /// Maximum number of hits to return (default 200, maximum 1000).
    limit: Option<u64>,
    /// Opaque pagination cursor returned by a previous call.
    cursor: Option<String>,
}

/// The agent-facing toolbox: holds the storage layer, the active policy, and the
/// configured timezone, and answers `tools/list` and `tools/call`.
pub struct Toolbox {
    storage: Storage,
    policy: Policy,
    timezone: Tz,
    tools: Vec<Tool>,
    /// Absolute path to the global session-context (full) template file (may not exist).
    session_context_template_file: PathBuf,
    /// Absolute path to the global session-bootstrap (lean) template file (may not exist).
    session_bootstrap_template_file: PathBuf,
    /// Absolute path to the global memory-layout template file (may not exist).
    memory_layout_template_file: PathBuf,
    /// The recall engine, present unless `MUNINN_RECALL_BACKEND=off`.
    recall: Option<Arc<RecallEngine>>,
}

impl Toolbox {
    /// Build the toolbox and precompute every tool's input schema for the active
    /// scheme. The `recall_memory_notes` tool is advertised only when `recall` is
    /// `Some` (i.e. the backend is not `off`).
    pub fn new(
        storage: Storage,
        policy: Policy,
        timezone: Tz,
        session_context_template_file: PathBuf,
        session_bootstrap_template_file: PathBuf,
        memory_layout_template_file: PathBuf,
        recall: Option<Arc<RecallEngine>>,
    ) -> Toolbox {
        let scheme = storage.resolver().scheme().clone();
        let tools = build_tools(&scheme, recall.is_some());
        Toolbox {
            storage,
            policy,
            timezone,
            tools,
            session_context_template_file,
            session_bootstrap_template_file,
            memory_layout_template_file,
            recall,
        }
    }

    /// The recall engine handle, for the server's warm-up, watcher start, and the
    /// `GET /readyz` probe.
    pub fn recall_engine(&self) -> Option<Arc<RecallEngine>> {
        self.recall.clone()
    }

    /// The advertised tool list for `tools/list`.
    pub fn list_tools(&self) -> Vec<Tool> {
        self.tools.clone()
    }

    /// Dispatch a `tools/call`. Returns `None` when the tool name is unknown so
    /// the caller can map it to a protocol-level "method not found"; `Some(Err)`
    /// carries a domain error to surface as a structured tool result.
    ///
    /// `grant` is the scope grant the transport resolved for this request
    /// ([`Grant::AllScopes`] on stdio or when no authentication is configured);
    /// requested scope keys outside the grant are rejected with `scope_denied`
    /// before the handler runs — before any path resolution or IO.
    pub fn call(
        &self,
        name: &str,
        args: &JsonObject,
        grant: &Grant,
    ) -> Option<Result<CallToolResult, MuninnError>> {
        let handler: fn(&Toolbox, &JsonObject) -> Result<CallToolResult, MuninnError> = match name {
            "list_memory_notes" => Toolbox::list_memory_notes,
            "read_memory_note" => Toolbox::read_memory_note,
            "read_memory_notes" => Toolbox::read_memory_notes,
            "write_memory_note" => Toolbox::write_memory_note,
            "write_memory_notes" => Toolbox::write_memory_notes,
            "edit_memory_note" => Toolbox::edit_memory_note,
            "delete_memory_note" => Toolbox::delete_memory_note,
            "rename_memory_note" => Toolbox::rename_memory_note,
            "read_note_properties" => Toolbox::read_note_properties,
            "update_note_properties" => Toolbox::update_note_properties,
            "load_session_context" => Toolbox::load_session_context,
            "evolve_core_persona" => Toolbox::evolve_core_persona,
            "update_task_heartbeat" => Toolbox::update_task_heartbeat,
            "append_diary_entry" => Toolbox::append_diary_entry,
            "recall_memory_notes" if self.recall.is_some() => Toolbox::recall_memory_notes,
            _ => return None,
        };
        if let Err(err) = self.check_grant(args, grant) {
            return Some(Err(err));
        }
        Some(handler(self, args))
    }

    /// Check the scope keys present in `args` against the per-request grant.
    /// Only well-formed keys are checked here — missing or malformed scope keys
    /// fall through to [`Toolbox::scope_map`]'s own validation and its standard
    /// `missing_scope`/`invalid_argument` errors.
    fn check_grant(&self, args: &JsonObject, grant: &Grant) -> Result<(), MuninnError> {
        if matches!(grant, Grant::AllScopes) {
            return Ok(());
        }
        let placeholders = self.scheme().placeholders();
        let mut scope: BTreeMap<String, String> = BTreeMap::new();
        for ph in &placeholders {
            if let Some(Value::String(s)) = args.get(*ph)
                && !s.is_empty()
            {
                scope.insert((*ph).to_string(), s.clone());
            }
        }
        grant.check(&placeholders, &scope)
    }

    /// Notify the recall engine of the server's own write so its in-memory index
    /// updates synchronously. A no-op when recall is disabled.
    fn recall_on_write(&self, scope: &str, region: Region, physical: &PhysicalPath) {
        if let Some(engine) = &self.recall {
            engine.on_write(scope, region, physical);
        }
    }

    // --- scope + argument helpers ---

    fn scheme(&self) -> &Scheme {
        self.storage.resolver().scheme()
    }

    /// Extract and validate the scope keys from the arguments, rejecting any
    /// argument key that is neither a scope placeholder nor a known tool field.
    /// Returns the validated scope map (placeholder ident → value).
    fn scope_map(
        &self,
        args: &JsonObject,
        tool_fields: &[&str],
    ) -> Result<BTreeMap<String, String>, MuninnError> {
        let placeholders = self.scheme().placeholders();

        let mut scope: BTreeMap<String, String> = BTreeMap::new();
        for ph in &placeholders {
            match args.get(*ph) {
                Some(Value::String(s)) if !s.is_empty() => {
                    scope.insert((*ph).to_string(), s.clone());
                }
                Some(Value::String(_)) => {
                    return Err(MuninnError::InvalidArgument {
                        message: format!("scope key '{ph}' must not be empty"),
                    });
                }
                Some(_) => {
                    return Err(MuninnError::InvalidArgument {
                        message: format!("scope key '{ph}' must be a string"),
                    });
                }
                None => {
                    return Err(MuninnError::MissingScope {
                        key: (*ph).to_string(),
                    });
                }
            }
        }

        for key in args.keys() {
            let is_scope = placeholders.contains(&key.as_str());
            let is_field = tool_fields.contains(&key.as_str());
            if !is_scope && !is_field {
                return Err(MuninnError::InvalidArgument {
                    message: format!("unexpected parameter '{key}'"),
                });
            }
        }

        Ok(scope)
    }

    /// Extract, validate, and render the scope suffix from the arguments.
    fn resolve_scope(
        &self,
        args: &JsonObject,
        tool_fields: &[&str],
    ) -> Result<String, MuninnError> {
        let scope = self.scope_map(args, tool_fields)?;
        self.scheme()
            .render(&scope)
            .map_err(|e| MuninnError::InvalidArgument {
                message: e.to_string(),
            })
    }

    /// The scheme's placeholder idents, in order — the scope keys every surface
    /// (tool, resource, prompt) requires. Used by the MCP server to derive the
    /// resource URI parameters and the prompt arguments.
    pub fn scheme_placeholders(&self) -> Vec<String> {
        self.scheme()
            .placeholders()
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    /// Render the session-context for a pre-built scope map (used by the
    /// `session-context`/`session-bootstrap` resources, the `session-context`
    /// prompt, and `GET /v1/context`/`GET /v1/bootstrap`). `kind` selects the
    /// full `Context` or lean `Bootstrap` render. Validates that `scope` contains
    /// exactly the scheme's placeholder keys, then checks it against the
    /// per-request `grant`, before rendering — an unauthorized scope is rejected
    /// with `scope_denied` before any file IO.
    pub fn render_session_context(
        &self,
        scope: &BTreeMap<String, String>,
        grant: &Grant,
        kind: crate::session_context::RenderKind,
    ) -> Result<crate::session_context::SessionContext, MuninnError> {
        self.check_render_scope(scope, grant)?;
        let template_file = match kind {
            crate::session_context::RenderKind::Context => &self.session_context_template_file,
            crate::session_context::RenderKind::Bootstrap => &self.session_bootstrap_template_file,
        };
        crate::session_context::render_session_context(&self.storage, template_file, scope, kind)
    }

    /// Render the layout document for a pre-built scope map (used by the
    /// `session-layout` resource and `GET /v1/layout`). Validates and grant-checks
    /// the scope as `render_session_context` does before rendering.
    pub fn render_layout(
        &self,
        scope: &BTreeMap<String, String>,
        grant: &Grant,
    ) -> Result<String, MuninnError> {
        self.check_render_scope(scope, grant)?;
        crate::session_context::render_layout(
            &self.storage,
            &self.memory_layout_template_file,
            scope,
        )
    }

    /// Validate that `scope` carries exactly the scheme's placeholder keys
    /// (non-empty) and is within the per-request `grant`. Shared by the render
    /// surfaces, which reject an unauthorized scope before any file IO.
    fn check_render_scope(
        &self,
        scope: &BTreeMap<String, String>,
        grant: &Grant,
    ) -> Result<(), MuninnError> {
        let placeholders = self.scheme().placeholders();
        for ph in &placeholders {
            match scope.get(*ph) {
                Some(v) if !v.is_empty() => {}
                Some(_) => {
                    return Err(MuninnError::InvalidArgument {
                        message: format!("scope key '{ph}' must not be empty"),
                    });
                }
                None => {
                    return Err(MuninnError::MissingScope {
                        key: (*ph).to_string(),
                    });
                }
            }
        }
        for key in scope.keys() {
            if !placeholders.contains(&key.as_str()) {
                return Err(MuninnError::InvalidArgument {
                    message: format!("unexpected scope key '{key}'"),
                });
            }
        }
        grant.check(&placeholders, scope)?;
        Ok(())
    }

    /// The clean virtual path of a conventional file relative to the agents
    /// folder (e.g. `Agents/PERSONA.md`, or `PERSONA.md` when the agents folder is
    /// the vault root).
    fn agents_vpath(&self, relative: &str) -> Result<VirtualPath, MuninnError> {
        let agents = self.storage.resolver().agents_dir();
        let full = if agents.as_str().is_empty() {
            relative.to_string()
        } else {
            format!("{agents}/{relative}")
        };
        VirtualPath::new(&full)
    }

    /// Reject a generic write/edit/delete that targets an agents-folder
    /// root-level core file. Such files are reserved for the dedicated wrappers
    /// (`evolve_core_persona`, `update_task_heartbeat`); the returned error
    /// carries `path_not_permitted` and names the wrapper to use. A no-op for
    /// subfolder paths and for paths outside the agents folder.
    fn reject_if_root_reserved(&self, vpath: &VirtualPath) -> Result<(), MuninnError> {
        if !self.storage.resolver().is_agents_root_level(vpath) {
            return Ok(());
        }
        let is_heartbeat = vpath
            .as_path()
            .file_name()
            .is_some_and(|name| name == "HEARTBEAT.md");
        let wrapper = if is_heartbeat {
            "update_task_heartbeat"
        } else {
            "evolve_core_persona"
        };
        Err(MuninnError::RootPathReserved {
            virtual_path: vpath.as_str().to_string(),
            wrapper,
        })
    }

    /// Apply the write-side link transform to `content` for a note at `vpath`.
    /// A no-op when the scheme is empty (no scopes, hence no suffix and no
    /// cross-scope leak). May return a leak-guard `WriteDenied` when a shared
    /// note links into the caller's own scope.
    fn expand_links_for(
        &self,
        scope: &str,
        vpath: &VirtualPath,
        content: &str,
    ) -> Result<String, MuninnError> {
        if self.scheme().is_empty() {
            return Ok(content.to_string());
        }
        let resolver = self.storage.resolver();
        let region = resolver.detect_region(vpath);
        let regions = self.policy.list_visible_regions(false);
        let index = self.storage.build_link_index(scope, &regions)?;
        crate::wikilink::expand_links(content, scope, region, resolver, &index)
    }

    /// Strip the caller's own scope suffix from link targets in `content`. A
    /// no-op when the scheme is empty.
    fn strip_links_for(&self, scope: &str, content: &str) -> String {
        if self.scheme().is_empty() {
            return content.to_string();
        }
        crate::wikilink::strip_links(content, scope, self.storage.resolver())
    }

    /// [`Self::expand_links_for`] over every string leaf of a property value
    /// tree destined for the note at `vpath`, recursing into arrays and nested
    /// objects. A no-op when the scheme is empty.
    fn expand_value_links_for(
        &self,
        scope: &str,
        vpath: &VirtualPath,
        value: &Value,
    ) -> Result<Value, MuninnError> {
        if self.scheme().is_empty() {
            return Ok(value.clone());
        }
        let resolver = self.storage.resolver();
        let region = resolver.detect_region(vpath);
        let regions = self.policy.list_visible_regions(false);
        let index = self.storage.build_link_index(scope, &regions)?;
        crate::wikilink::expand_value_links(value, scope, region, resolver, &index)
    }

    /// [`Self::strip_links_for`] over every string leaf of a property value
    /// tree. A no-op when the scheme is empty.
    fn strip_value_links_for(&self, scope: &str, value: &Value) -> Value {
        if self.scheme().is_empty() {
            return value.clone();
        }
        crate::wikilink::strip_value_links(value, scope, self.storage.resolver())
    }

    /// Map a [`PolicyError`] to the appropriate boundary error for `vpath`.
    fn policy_err(err: PolicyError, vpath: &VirtualPath) -> MuninnError {
        match err {
            PolicyError::NotPermitted => MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            },
            PolicyError::WriteDenied => MuninnError::WriteDenied {
                virtual_path: vpath.as_str().to_string(),
            },
        }
    }

    // --- handlers ---

    fn list_memory_notes(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(
            args,
            &["path_prefix", "glob", "order", "limit", "cursor", "view"],
        )?;

        let view = match opt_str(args, "view")?.as_deref() {
            None | Some("files") => ListView::Files,
            Some("dirs") => ListView::Dirs,
            Some(other) => {
                return Err(MuninnError::InvalidArgument {
                    message: format!("view must be \"files\" or \"dirs\", got {other:?}"),
                });
            }
        };

        let order = match opt_str(args, "order")?.as_deref() {
            None | Some("name_asc") => ListOrder::NameAsc,
            Some("name_desc") => ListOrder::NameDesc,
            Some(other) => {
                return Err(MuninnError::InvalidArgument {
                    message: format!("order must be \"name_asc\" or \"name_desc\", got {other:?}"),
                });
            }
        };

        let path_prefix = opt_str(args, "path_prefix")?;
        let glob = match opt_str(args, "glob")? {
            Some(pattern) => Some(
                globset::Glob::new(&pattern)
                    .map_err(|e| MuninnError::InvalidArgument {
                        message: format!("invalid glob: {e}"),
                    })?
                    .compile_matcher(),
            ),
            None => None,
        };
        let limit = match opt_u64(args, "limit")? {
            Some(n) if n > MAX_LIMIT => {
                return Err(MuninnError::InvalidArgument {
                    message: format!("limit must not exceed {MAX_LIMIT}"),
                });
            }
            Some(n) => n.max(1),
            None => DEFAULT_LIMIT,
        };
        let offset = match opt_str(args, "cursor")? {
            Some(c) => Cursor::decode(&c)?,
            None => 0,
        };

        let regions = self.policy.list_visible_regions(self.scheme().is_empty());
        let mut items: Vec<String> = self
            .storage
            .list_visible(&scope, &regions)?
            .into_iter()
            .map(|p| p.as_str().to_string())
            .collect();

        if matches!(order, ListOrder::NameDesc) {
            items.reverse();
        }

        if let Some(prefix) = &path_prefix {
            let agents = self.storage.resolver().agents_dir();
            let effective = if agents.as_str().is_empty() {
                prefix.clone()
            } else {
                format!("{agents}/{prefix}")
            };
            let with_sep = format!("{effective}/");
            items.retain(|p| p == &effective || p.starts_with(&with_sep));
        }

        if let Some(matcher) = &glob {
            items.retain(|p| matcher.is_match(p));
        }

        if matches!(view, ListView::Dirs) {
            // Derive every ancestor directory of each visible file. A BTreeSet
            // deduplicates and yields deterministic ascending order; the order
            // selector then reverses it to match the files view.
            let mut dirs: BTreeSet<String> = BTreeSet::new();
            for path in &items {
                let mut end = 0;
                while let Some(idx) = path[end..].find('/') {
                    let cut = end + idx;
                    dirs.insert(path[..cut].to_string());
                    end = cut + 1;
                }
            }
            items = dirs.into_iter().collect();
            if matches!(order, ListOrder::NameDesc) {
                items.reverse();
            }
        }

        let total = items.len() as u64;
        let start = offset.min(total) as usize;
        let end = (offset + limit).min(total) as usize;
        let page: Vec<String> = items[start..end].to_vec();
        let next_cursor = if (end as u64) < total {
            Some(Cursor::encode(end as u64))
        } else {
            None
        };

        Ok(ok_json(json!({
            "items": page,
            "next_cursor": next_cursor,
        })))
    }

    /// Gate and read one note's stored bytes for `scope`: policy gate by region,
    /// suffix resolution, visibility check, read — no link transform. Shared by
    /// every reader so their gating cannot drift.
    fn read_raw(&self, scope: &str, vpath: &VirtualPath) -> Result<String, MuninnError> {
        let resolver = self.storage.resolver();
        let region = resolver.detect_region(vpath);
        self.policy
            .gate_read(region)
            .map_err(|e| Self::policy_err(e, vpath))?;
        let physical = resolver.resolve(scope, vpath)?;
        if !self.storage.is_visible(&physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            });
        }
        self.storage.read(&physical)
    }

    /// Read one note for `scope` with full single-read semantics: [`Self::read_raw`]
    /// plus the own-suffix link strip. Shared by `read_memory_note` and
    /// `read_memory_notes` so the two cannot drift.
    fn read_one(&self, scope: &str, vpath: &VirtualPath) -> Result<String, MuninnError> {
        Ok(self.strip_links_for(scope, &self.read_raw(scope, vpath)?))
    }

    /// [`Self::read_one`] plus the optional line range: the slice is applied to
    /// the agent-facing content (after the link strip), so line numbers match a
    /// whole-note read and a suffix never leaks through a slice boundary.
    /// Returns the content and, only when a range was requested, the full
    /// note's line count. Shared by `read_memory_note` and `read_memory_notes`
    /// so range semantics cannot drift.
    fn read_one_ranged(
        &self,
        scope: &str,
        vpath: &VirtualPath,
        range: Option<LineRange>,
    ) -> Result<(String, Option<u64>), MuninnError> {
        let content = self.read_one(scope, vpath)?;
        Ok(match range {
            Some(range) => {
                let (sliced, total) = slice_lines(&content, range);
                (sliced, Some(total))
            }
            None => (content, None),
        })
    }

    fn read_memory_note(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["path", "backlinks", "offset", "limit"])?;
        let vpath = VirtualPath::new(&require_str(args, "path")?)?;
        let want_backlinks = opt_bool(args, "backlinks")?.unwrap_or(false);
        let range = opt_line_range(args)?;
        let (content, total_lines) = self.read_one_ranged(&scope, &vpath, range)?;
        let mut structured = json!({ "content": &content });
        if let Some(total) = total_lines {
            structured["total_lines"] = json!(total);
        }
        if want_backlinks {
            structured["backlinks"] = json!(self.collect_backlinks(&scope, &vpath)?);
        }
        let mut result = CallToolResult::success(vec![Content::text(content)]);
        result.structured_content = Some(structured);
        Ok(result)
    }

    fn read_memory_notes(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["paths"])?;
        let entries = match args.get("paths") {
            Some(Value::Array(items)) => items
                .iter()
                .map(parse_batch_entry)
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(MuninnError::InvalidArgument {
                    message: "argument 'paths' must be an array".to_string(),
                });
            }
            None => {
                return Err(MuninnError::InvalidArgument {
                    message: "missing required argument 'paths'".to_string(),
                });
            }
        };
        if entries.is_empty() {
            return Err(MuninnError::InvalidArgument {
                message: "argument 'paths' must contain at least one path".to_string(),
            });
        }
        if entries.len() > MAX_BATCH_READ {
            return Err(MuninnError::InvalidArgument {
                message: format!("argument 'paths' must not exceed {MAX_BATCH_READ} entries"),
            });
        }
        let notes: Vec<Value> = entries
            .iter()
            .map(|(path, range)| {
                match VirtualPath::new(path)
                    .and_then(|vpath| self.read_one_ranged(&scope, &vpath, *range))
                {
                    Ok((content, Some(total))) => {
                        json!({ "path": path, "content": content, "total_lines": total })
                    }
                    Ok((content, None)) => json!({ "path": path, "content": content }),
                    Err(err) => json!({
                        "path": path,
                        "error": { "code": err.code().as_str(), "message": err.to_string() },
                    }),
                }
            })
            .collect();
        Ok(ok_json(json!({ "notes": notes })))
    }

    /// The clean virtual paths of every visible note containing at least one
    /// link that resolves to `vpath`, deduplicated and sorted ascending. Notes
    /// that cannot be read (raced deletion, non-UTF-8) are skipped — they cannot
    /// contain resolvable links.
    fn collect_backlinks(
        &self,
        scope: &str,
        vpath: &VirtualPath,
    ) -> Result<Vec<String>, MuninnError> {
        let target_clean = vpath.as_str().strip_suffix(".md").unwrap_or(vpath.as_str());
        let resolver = self.storage.resolver();
        let regions = self.policy.list_visible_regions(self.scheme().is_empty());
        let index = self.storage.build_link_index(scope, &regions)?;
        let mut backlinks = BTreeSet::new();
        for referrer in self.storage.list_visible(scope, &regions)? {
            let physical = resolver.resolve(scope, &referrer)?;
            let Ok(content) = self.storage.read(&physical) else {
                continue;
            };
            if crate::wikilink::references_to(&content, target_clean, scope, resolver, &index) {
                backlinks.insert(referrer.as_str().to_string());
            }
        }
        Ok(backlinks.into_iter().collect())
    }

    fn write_memory_note(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["path", "content", "append"])?;
        let vpath = VirtualPath::new(&require_str(args, "path")?)?;
        let content = require_str(args, "content")?;
        let append = opt_bool(args, "append")?.unwrap_or(false);
        self.reject_if_root_reserved(&vpath)?;
        // Gate by policy before the link transform so a read-only/denied region
        // reports its policy error rather than a leak-guard `write_denied`.
        let region = self.storage.resolver().detect_region(&vpath);
        self.policy
            .gate_write(region)
            .map_err(|e| Self::policy_err(e, &vpath))?;
        let content = self.expand_links_for(&scope, &vpath, &content)?;
        self.gated_write(&scope, &vpath, |physical, storage| {
            if append {
                storage.read_modify_write(physical, |current| {
                    Ok(match current {
                        Some(existing) => format!("{existing}{content}"),
                        None => content.clone(),
                    })
                })
            } else {
                storage.write_atomic(physical, &content)
            }
        })
    }

    /// Write up to [`MAX_BATCH_WRITE`] notes in one call, all-or-nothing on
    /// validation. Phase 1 validates every entry with the exact per-entry
    /// semantics of `write_memory_note` — reserved roots, policy gating, link
    /// expansion (against the visible set seeded with the batch's own paths, so
    /// intra-batch links resolve order-independently), and visibility — and
    /// computes the final contents with no writes; phase 2 then applies in
    /// request order. Each applied entry is atomic and indexed synchronously,
    /// but the batch is not transactional across entries: a failure or crash
    /// mid-apply leaves a clean prefix of the batch applied.
    fn write_memory_notes(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["notes"])?;
        let raw_entries = match args.get("notes") {
            Some(Value::Array(items)) => items,
            Some(_) => {
                return Err(MuninnError::InvalidArgument {
                    message: "argument 'notes' must be an array".to_string(),
                });
            }
            None => {
                return Err(MuninnError::InvalidArgument {
                    message: "missing required argument 'notes'".to_string(),
                });
            }
        };
        if raw_entries.is_empty() {
            return Err(MuninnError::InvalidArgument {
                message: "argument 'notes' must contain at least one entry".to_string(),
            });
        }
        if raw_entries.len() > MAX_BATCH_WRITE {
            return Err(MuninnError::InvalidArgument {
                message: format!("argument 'notes' must not exceed {MAX_BATCH_WRITE} entries"),
            });
        }

        let resolver = self.storage.resolver();

        // --- Phase 1: validate and compute; no writes. ---
        let mut entries = Vec::with_capacity(raw_entries.len());
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for raw in raw_entries {
            let (path, content, append) = parse_batch_write_entry(raw)?;
            let vpath = VirtualPath::new(&path)?;
            if !seen.insert(vpath.as_str().to_string()) {
                return Err(MuninnError::InvalidArgument {
                    message: format!("duplicate path '{}' in 'notes'", vpath.as_str()),
                });
            }
            let region = resolver.detect_region(&vpath);
            entries.push((path, vpath, region, content, append));
        }

        // The link index is seeded with the batch's own paths before any entry
        // expands, so an entry can link to a note created by another entry of
        // the same call and resolution is independent of entry order.
        let index = if self.scheme().is_empty() {
            None
        } else {
            let regions = self.policy.list_visible_regions(false);
            let base = self.storage.build_link_index(&scope, &regions)?;
            let pending: Vec<LinkEntry> = entries
                .iter()
                .map(|(_, vpath, region, _, _)| LinkEntry {
                    clean_path: vpath
                        .as_str()
                        .strip_suffix(".md")
                        .unwrap_or(vpath.as_str())
                        .to_string(),
                    region: *region,
                })
                .collect();
            Some(crate::wikilink::seed_index(&base, &pending))
        };

        let mut prepared = Vec::with_capacity(entries.len());
        for (path, vpath, region, content, append) in entries {
            self.reject_if_root_reserved(&vpath)?;
            self.policy
                .gate_write(region)
                .map_err(|e| Self::policy_err(e, &vpath))?;
            let content = match &index {
                Some(index) => {
                    crate::wikilink::expand_links(&content, &scope, region, resolver, index)?
                }
                None => content,
            };
            let physical = resolver.resolve(&scope, &vpath)?;
            if !self.storage.is_visible(&physical) {
                return Err(MuninnError::PathNotPermitted {
                    virtual_path: vpath.as_str().to_string(),
                });
            }
            prepared.push((path, region, physical, content, append));
        }

        // --- Phase 2: apply in request order. ---
        let mut results = Vec::with_capacity(prepared.len());
        for (path, region, physical, content, append) in prepared {
            let written = if append {
                // The locked read-modify-write, not precomputed bytes: an
                // external edit between the phases must not be lost.
                self.storage.read_modify_write(&physical, |current| {
                    Ok(match current {
                        Some(existing) => format!("{existing}{content}"),
                        None => content.clone(),
                    })
                })?
            } else {
                self.storage.write_atomic(&physical, &content)?
            };
            self.recall_on_write(&scope, region, &physical);
            results.push(json!({ "path": path, "bytes_written": written }));
        }
        Ok(ok_json(json!({ "results": results })))
    }

    fn edit_memory_note(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["path", "search_string", "replace_string"])?;
        let vpath = VirtualPath::new(&require_str(args, "path")?)?;
        let search = require_str(args, "search_string")?;
        let replace = require_str(args, "replace_string")?;
        self.reject_if_root_reserved(&vpath)?;
        let region = self.storage.resolver().detect_region(&vpath);
        self.policy
            .gate_write(region)
            .map_err(|e| Self::policy_err(e, &vpath))?;
        // Match the on-disk (suffixed) link form: transform the search/replace
        // snippets the same way stored content was transformed on write.
        let search = self.expand_links_for(&scope, &vpath, &search)?;
        let replace = self.expand_links_for(&scope, &vpath, &replace)?;
        let resolver = self.storage.resolver();
        let physical = resolver.resolve(&scope, &vpath)?;
        if !self.storage.is_visible(&physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            });
        }
        let replaced = self
            .storage
            .edit_search_replace(&physical, &search, &replace)?;
        self.recall_on_write(&scope, region, &physical);
        Ok(ok_json(json!({ "chars_replaced": replaced })))
    }

    fn delete_memory_note(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["path"])?;
        let vpath = VirtualPath::new(&require_str(args, "path")?)?;
        self.reject_if_root_reserved(&vpath)?;
        let resolver = self.storage.resolver();
        let region = resolver.detect_region(&vpath);
        self.policy
            .gate_write(region)
            .map_err(|e| Self::policy_err(e, &vpath))?;
        let physical = resolver.resolve(&scope, &vpath)?;
        if !self.storage.is_visible(&physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            });
        }
        self.storage.delete(&physical)?;
        Ok(ok_json(json!({ "deleted": true })))
    }

    /// Move a note from `path` to `new_path`, rewriting every visible incoming
    /// link to resolve to the new location. Phase 1 validates everything and
    /// computes every rewritten content with no writes; phase 2 then mutates in
    /// the order destination → referrers → source, so a crash mid-flight leaves
    /// both copies present and never a dangling reference.
    fn rename_memory_note(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["path", "new_path"])?;
        let vpath = VirtualPath::new(&require_str(args, "path")?)?;
        let new_vpath = VirtualPath::new(&require_str(args, "new_path")?)?;
        let resolver = self.storage.resolver();

        // --- Phase 1: validate and compute; no writes. ---
        // Core files are wrapper-managed on both ends.
        self.reject_if_root_reserved(&vpath)?;
        self.reject_if_root_reserved(&new_vpath)?;

        // Both the source's and the destination's region must be policy-writable.
        let src_region = resolver.detect_region(&vpath);
        self.policy
            .gate_write(src_region)
            .map_err(|e| Self::policy_err(e, &vpath))?;
        let dest_region = resolver.detect_region(&new_vpath);
        self.policy
            .gate_write(dest_region)
            .map_err(|e| Self::policy_err(e, &new_vpath))?;

        let src_physical = resolver.resolve(&scope, &vpath)?;
        if !self.storage.is_visible(&src_physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            });
        }
        let stored = self.storage.read(&src_physical)?;

        let dest_physical = resolver.resolve(&scope, &new_vpath)?;
        if !self.storage.is_visible(&dest_physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: new_vpath.as_str().to_string(),
            });
        }
        if dest_physical.as_path().exists() {
            return Err(MuninnError::DestinationExists {
                virtual_path: new_vpath.as_str().to_string(),
            });
        }

        let source_clean = vpath.as_str().strip_suffix(".md").unwrap_or(vpath.as_str());
        let dest_entry = LinkEntry {
            clean_path: new_vpath
                .as_str()
                .strip_suffix(".md")
                .unwrap_or(new_vpath.as_str())
                .to_string(),
            region: dest_region,
        };
        let regions = self.policy.list_visible_regions(self.scheme().is_empty());
        let index = self.storage.build_link_index(&scope, &regions)?;

        // The moved note's own content: strip to the clean form, re-point
        // self-references at the destination, then re-expand for the
        // destination's region against the post-rename visible set (the
        // cross-scope leak guard surfaces here, before any mutation).
        let clean_content = self.strip_links_for(&scope, &stored);
        let (retargeted, _) = crate::wikilink::retarget_links(
            &clean_content,
            source_clean,
            &dest_entry,
            &scope,
            dest_region,
            resolver,
            &index,
        )?;
        let dest_content = if self.scheme().is_empty() {
            retargeted
        } else {
            let post = crate::wikilink::post_rename_index(&index, source_clean, &dest_entry);
            crate::wikilink::expand_links(&retargeted, &scope, dest_region, resolver, &post)?
        };

        // Referrers: every visible note with a link resolving to the source must
        // live in a writable region; compute each rewritten content now.
        let mut rewrites: Vec<(Region, PhysicalPath, String)> = Vec::new();
        for referrer in self.storage.list_visible(&scope, &regions)? {
            if referrer == vpath {
                continue; // the moved note's own content is handled above
            }
            let r_physical = resolver.resolve(&scope, &referrer)?;
            let Ok(r_content) = self.storage.read(&r_physical) else {
                continue; // unreadable notes cannot contain resolvable links
            };
            if !crate::wikilink::references_to(&r_content, source_clean, &scope, resolver, &index) {
                continue;
            }
            let r_region = resolver.detect_region(&referrer);
            self.policy
                .gate_write(r_region)
                .map_err(|e| Self::policy_err(e, &referrer))?;
            let (rewritten, _) = crate::wikilink::retarget_links(
                &r_content,
                source_clean,
                &dest_entry,
                &scope,
                r_region,
                resolver,
                &index,
            )?;
            rewrites.push((r_region, r_physical, rewritten));
        }

        // --- Phase 2: mutate. Destination first, source last, so a crash
        // mid-flight leaves every link resolvable to at least one copy. ---
        self.storage.write_atomic(&dest_physical, &dest_content)?;
        self.recall_on_write(&scope, dest_region, &dest_physical);
        let notes_rewritten = rewrites.len();
        for (r_region, r_physical, rewritten) in rewrites {
            self.storage.write_atomic(&r_physical, &rewritten)?;
            self.recall_on_write(&scope, r_region, &r_physical);
        }
        self.storage.delete(&src_physical)?;
        self.recall_on_write(&scope, src_region, &src_physical);

        Ok(ok_json(json!({
            "renamed": true,
            "path": vpath.as_str(),
            "new_path": new_vpath.as_str(),
            "notes_rewritten": notes_rewritten,
        })))
    }

    fn read_note_properties(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["path"])?;
        let vpath = VirtualPath::new(&require_str(args, "path")?)?;
        // Parse the stored bytes, then strip the caller's own suffix from the
        // parsed value tree, so the property surface matches the read-path view
        // of the body: the agent never observes its own scope suffix.
        let content = self.read_raw(&scope, &vpath)?;
        let props = crate::frontmatter::parse(&content).props;
        Ok(ok_json(
            json!({ "properties": self.strip_value_links_for(&scope, &props) }),
        ))
    }

    fn update_note_properties(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["path", "properties"])?;
        let vpath = VirtualPath::new(&require_str(args, "path")?)?;
        let updates = require_object(args, "properties")?;
        self.reject_if_root_reserved(&vpath)?;
        let region = self.storage.resolver().detect_region(&vpath);
        self.policy
            .gate_write(region)
            .map_err(|e| Self::policy_err(e, &vpath))?;
        let physical = self.storage.resolver().resolve(&scope, &vpath)?;
        if !self.storage.is_visible(&physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            });
        }
        // The whole merge runs under the per-target lock so concurrent updates
        // to different keys both land. Supplied string values get the
        // write-side link transform (the cross-scope leak guard fires before
        // any mutation); the merge itself stays a pure data operation and the
        // body is untouched.
        let updates = Value::Object(updates);
        let mut merged = Value::Object(Map::new());
        self.storage.read_modify_write(&physical, |current| {
            let existing = current.ok_or_else(|| MuninnError::NotFound {
                virtual_path: vpath.as_str().to_string(),
            })?;
            let Value::Object(expanded) = self.expand_value_links_for(&scope, &vpath, &updates)?
            else {
                unreachable!("expanding an object yields an object");
            };
            let next = crate::frontmatter::merge(&existing, &expanded).map_err(|e| {
                MuninnError::InvalidArgument {
                    message: e.to_string(),
                }
            })?;
            merged = crate::frontmatter::parse(&next).props;
            Ok(next)
        })?;
        self.recall_on_write(&scope, region, &physical);
        // The response is the merged set in the agent-facing clean form, even
        // when untouched existing keys carry suffixed values.
        Ok(ok_json(
            json!({ "properties": self.strip_value_links_for(&scope, &merged) }),
        ))
    }

    fn load_session_context(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        // Accept only scope parameters (no `path`/`which`). Always the full
        // context render; the scope grant is enforced at the call dispatch.
        let scope = self.scope_map(args, &[])?;
        let sc = crate::session_context::render_session_context(
            &self.storage,
            &self.session_context_template_file,
            &scope,
            crate::session_context::RenderKind::Context,
        )?;
        Ok(ok_json(
            json!({ "rendered": sc.rendered, "missing": sc.missing }),
        ))
    }

    /// Replace foundational session files: the single `which`/`content` form
    /// writes one file with the legacy response, the batch `updates` form
    /// validates every entry (which domain, duplicates, line caps, link
    /// expansion, policy, visibility) before writing any file, then applies in
    /// request order. Exactly one of the two forms must be supplied.
    fn evolve_core_persona(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["which", "content", "updates"])?;
        let has_single = args.contains_key("which") || args.contains_key("content");
        let has_batch = args.contains_key("updates");
        if has_single && has_batch {
            return Err(MuninnError::InvalidArgument {
                message: "supply either 'which'/'content' or 'updates', not both".to_string(),
            });
        }
        if !has_single && !has_batch {
            return Err(MuninnError::InvalidArgument {
                message: "supply either 'which' and 'content', or an 'updates' array".to_string(),
            });
        }

        if has_single {
            let which = require_str(args, "which")?;
            let (filename, line_cap) = evolve_target(&which)?;
            let content = require_str(args, "content")?;
            let (vpath, content) =
                self.prepare_evolve_content(&scope, filename, line_cap, &content)?;
            return self.gated_write(&scope, &vpath, |physical, storage| {
                storage.write_atomic(physical, &content)
            });
        }

        let raw_updates = match args.get("updates") {
            Some(Value::Array(items)) => items,
            Some(_) => {
                return Err(MuninnError::InvalidArgument {
                    message: "argument 'updates' must be an array".to_string(),
                });
            }
            None => unreachable!("has_batch checked above"),
        };
        if raw_updates.is_empty() {
            return Err(MuninnError::InvalidArgument {
                message: "argument 'updates' must contain at least one entry".to_string(),
            });
        }
        if raw_updates.len() > MAX_BATCH_EVOLVE {
            return Err(MuninnError::InvalidArgument {
                message: format!("argument 'updates' must not exceed {MAX_BATCH_EVOLVE} entries"),
            });
        }

        let resolver = self.storage.resolver();

        // --- Phase 1: validate every entry and compute final contents; no writes. ---
        let mut seen: BTreeSet<&'static str> = BTreeSet::new();
        let mut prepared = Vec::with_capacity(raw_updates.len());
        for raw in raw_updates {
            let (which, content) = parse_evolve_update_entry(raw)?;
            let (filename, line_cap) = evolve_target(&which)?;
            if !seen.insert(filename) {
                return Err(MuninnError::InvalidArgument {
                    message: format!("duplicate which '{which}' in 'updates'"),
                });
            }
            let (vpath, content) =
                self.prepare_evolve_content(&scope, filename, line_cap, &content)?;
            let region = resolver.detect_region(&vpath);
            self.policy
                .gate_write(region)
                .map_err(|e| Self::policy_err(e, &vpath))?;
            let physical = resolver.resolve(&scope, &vpath)?;
            if !self.storage.is_visible(&physical) {
                return Err(MuninnError::PathNotPermitted {
                    virtual_path: vpath.as_str().to_string(),
                });
            }
            prepared.push((which, region, physical, content));
        }

        // --- Phase 2: apply in request order. ---
        let mut results = Vec::with_capacity(prepared.len());
        for (which, region, physical, content) in prepared {
            let written = self.storage.write_atomic(&physical, &content)?;
            self.recall_on_write(&scope, region, &physical);
            results.push(json!({ "which": which, "bytes_written": written }));
        }
        Ok(ok_json(json!({ "results": results })))
    }

    /// Validate one `evolve_core_persona` target: cap-check the agent-facing
    /// content, resolve the conventional vpath, and expand links. Returns the
    /// vpath and the final on-disk content; writes nothing.
    fn prepare_evolve_content(
        &self,
        scope: &str,
        filename: &'static str,
        line_cap: Option<usize>,
        content: &str,
    ) -> Result<(VirtualPath, String), MuninnError> {
        if let Some(cap) = line_cap {
            let lines = content.lines().count();
            if lines > cap {
                return Err(MuninnError::InvalidArgument {
                    message: format!(
                        "{filename} must not exceed {cap} lines (got {lines}); file left unchanged"
                    ),
                });
            }
        }
        let vpath = self.agents_vpath(filename)?;
        // Expand link targets so core files (e.g. a MEMORY.md index of `[[notes]]`)
        // resolve in Obsidian; the line caps above count the agent-facing content,
        // and expansion never changes the line count.
        let content = self.expand_links_for(scope, &vpath, content)?;
        Ok((vpath, content))
    }

    fn update_task_heartbeat(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["content"])?;
        let content = require_str(args, "content")?;
        let vpath = self.agents_vpath("HEARTBEAT.md")?;
        let content = self.expand_links_for(&scope, &vpath, &content)?;
        self.gated_write(&scope, &vpath, |physical, storage| {
            storage.write_atomic(physical, &content)
        })
    }

    fn append_diary_entry(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let scope = self.resolve_scope(args, &["content", "title"])?;
        let content = require_str(args, "content")?;
        if content.is_empty() {
            return Err(MuninnError::InvalidArgument {
                message: "content must not be empty".to_string(),
            });
        }
        let title = opt_str(args, "title")?;
        let now = Utc::now().with_timezone(&self.timezone);
        let date = now.format("%Y-%m-%d").to_string();
        let time = now.format("%H:%M:%S").to_string();
        let heading = match title.as_deref() {
            Some(t) if !t.is_empty() => format!("## {time} — {t}"),
            _ => format!("## {time}"),
        };
        let vpath = self.agents_vpath(&format!("diary/{date}.md"))?;

        let region = self.storage.resolver().detect_region(&vpath);
        self.policy
            .gate_write(region)
            .map_err(|e| Self::policy_err(e, &vpath))?;
        let content = self.expand_links_for(&scope, &vpath, &content)?;
        let resolver = self.storage.resolver();
        let physical = resolver.resolve(&scope, &vpath)?;
        if !self.storage.is_visible(&physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            });
        }
        let written = self.storage.read_modify_write(&physical, |current| {
            Ok(match current {
                Some(existing) => format!("{existing}\n{heading}\n{content}\n"),
                None => format!("# {date}\n\n{heading}\n{content}\n"),
            })
        })?;
        self.recall_on_write(&scope, region, &physical);
        Ok(ok_json(json!({ "bytes_written": written })))
    }

    /// Shared write path: gate by policy, enforce visibility, then run `op`.
    fn gated_write(
        &self,
        scope: &str,
        vpath: &VirtualPath,
        op: impl FnOnce(&crate::path::PhysicalPath, &Storage) -> Result<usize, MuninnError>,
    ) -> Result<CallToolResult, MuninnError> {
        let resolver = self.storage.resolver();
        let region = resolver.detect_region(vpath);
        self.policy
            .gate_write(region)
            .map_err(|e| Self::policy_err(e, vpath))?;
        let physical = resolver.resolve(scope, vpath)?;
        if !self.storage.is_visible(&physical) {
            return Err(MuninnError::PathNotPermitted {
                virtual_path: vpath.as_str().to_string(),
            });
        }
        let written = op(&physical, &self.storage)?;
        self.recall_on_write(scope, region, &physical);
        Ok(ok_json(json!({ "bytes_written": written })))
    }

    fn recall_memory_notes(&self, args: &JsonObject) -> Result<CallToolResult, MuninnError> {
        let engine = self
            .recall
            .as_ref()
            .expect("recall_memory_notes dispatched only when recall is enabled");
        let scope = self.resolve_scope(
            args,
            &[
                "query",
                "regex",
                "filters",
                "modified_after",
                "modified_before",
                "path_prefix",
                "limit",
                "cursor",
            ],
        )?;

        let text = opt_str(args, "query")?;
        let regex = opt_str(args, "regex")?;
        let filters = parse_filters(args)?;
        let modified_after = opt_time_bound(args, "modified_after", self.timezone)?;
        let modified_before = opt_time_bound(args, "modified_before", self.timezone)?;
        let path_prefix = opt_str(args, "path_prefix")?;
        if text.is_none()
            && regex.is_none()
            && filters.is_empty()
            && modified_after.is_none()
            && modified_before.is_none()
        {
            return Err(MuninnError::InvalidArgument {
                message: "at least one of query, regex, filters, modified_after, or \
                          modified_before is required"
                    .to_string(),
            });
        }
        let limit = match opt_u64(args, "limit")? {
            Some(n) if n > MAX_LIMIT => {
                return Err(MuninnError::InvalidArgument {
                    message: format!("limit must not exceed {MAX_LIMIT}"),
                });
            }
            Some(n) => n.max(1),
            None => DEFAULT_LIMIT,
        };
        let offset = match opt_str(args, "cursor")? {
            Some(c) => Cursor::decode(&c)?,
            None => 0,
        };

        let regions = self.policy.list_visible_regions(self.scheme().is_empty());
        let query = RecallQuery {
            text,
            regex,
            filters,
            path_prefix,
            limit,
            offset,
            modified_after,
            modified_before,
        };
        let results = engine.recall(&scope, &regions, &query)?;

        let hits: Vec<Value> = results
            .hits
            .into_iter()
            .map(|h| {
                let mut hit = json!({ "path": h.path, "score": h.score, "snippets": h.snippets });
                if let Some(modified_at) = h.modified_at {
                    hit["modified_at"] = json!(modified_at);
                }
                hit
            })
            .collect();
        Ok(ok_json(json!({
            "hits": hits,
            "next_cursor": results.next_cursor,
            "truncated": results.truncated,
        })))
    }
}

/// Parse the optional `filters` argument into [`PropertyFilter`]s.
fn parse_filters(args: &JsonObject) -> Result<Vec<PropertyFilter>, MuninnError> {
    let raw = match args.get("filters") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(MuninnError::InvalidArgument {
                message: "argument 'filters' must be an array".to_string(),
            });
        }
    };
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let obj = item
            .as_object()
            .ok_or_else(|| MuninnError::InvalidArgument {
                message: "each filter must be an object with 'key' and 'op'".to_string(),
            })?;
        let key = match obj.get("key") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => {
                return Err(MuninnError::InvalidArgument {
                    message: "each filter must have a non-empty string 'key'".to_string(),
                });
            }
        };
        let op = match obj.get("op").and_then(Value::as_str) {
            Some("exists") => FilterOp::Exists,
            Some("eq") => FilterOp::Eq,
            Some("contains") => FilterOp::Contains,
            Some("gt") => FilterOp::Gt,
            Some("lt") => FilterOp::Lt,
            Some("ge") => FilterOp::Ge,
            Some("le") => FilterOp::Le,
            _ => {
                return Err(MuninnError::InvalidArgument {
                    message: "filter 'op' must be one of exists|eq|contains|gt|lt|ge|le"
                        .to_string(),
                });
            }
        };
        let value = match obj.get("value") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                return Err(MuninnError::InvalidArgument {
                    message: "filter 'value' must be a string".to_string(),
                });
            }
        };
        out.push(PropertyFilter { key, op, value });
    }
    Ok(out)
}

// --- free helpers ---

/// Build a successful tool result whose text is the JSON form and whose
/// structured content is `value`.
fn ok_json(value: Value) -> CallToolResult {
    let text = serde_json::to_string(&value).unwrap_or_default();
    let mut result = CallToolResult::success(vec![Content::text(text)]);
    result.structured_content = Some(value);
    result
}

/// Require a string argument, erroring with `invalid_argument` when absent or of
/// the wrong type.
fn require_str(args: &JsonObject, key: &str) -> Result<String, MuninnError> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(MuninnError::InvalidArgument {
            message: format!("argument '{key}' must be a string"),
        }),
        None => Err(MuninnError::InvalidArgument {
            message: format!("missing required argument '{key}'"),
        }),
    }
}

/// Require a JSON-object argument, erroring with `invalid_argument` when absent
/// or of the wrong type.
fn require_object(args: &JsonObject, key: &str) -> Result<Map<String, Value>, MuninnError> {
    match args.get(key) {
        Some(Value::Object(o)) => Ok(o.clone()),
        Some(_) => Err(MuninnError::InvalidArgument {
            message: format!("argument '{key}' must be an object"),
        }),
        None => Err(MuninnError::InvalidArgument {
            message: format!("missing required argument '{key}'"),
        }),
    }
}

fn opt_str(args: &JsonObject, key: &str) -> Result<Option<String>, MuninnError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(MuninnError::InvalidArgument {
            message: format!("argument '{key}' must be a string"),
        }),
    }
}

fn opt_bool(args: &JsonObject, key: &str) -> Result<Option<bool>, MuninnError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(MuninnError::InvalidArgument {
            message: format!("argument '{key}' must be a boolean"),
        }),
    }
}

/// Parse an optional `modified_after`/`modified_before` argument via
/// [`parse_time_bound`].
fn opt_time_bound(args: &JsonObject, key: &str, tz: Tz) -> Result<Option<SystemTime>, MuninnError> {
    match opt_str(args, key)? {
        Some(raw) => Ok(Some(parse_time_bound(key, &raw, tz)?)),
        None => Ok(None),
    }
}

/// Parse a time bound: an RFC 3339 timestamp, or a bare `YYYY-MM-DD` date
/// resolved to start of day in the configured timezone. Anything else is
/// `invalid_argument`.
fn parse_time_bound(key: &str, raw: &str, tz: Tz) -> Result<SystemTime, MuninnError> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.into());
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let start = date.and_time(NaiveTime::MIN);
        let resolved = match start.and_local_timezone(tz) {
            LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => Some(dt),
            // Midnight falls in a DST gap: the day starts when the clock resumes.
            LocalResult::None => (start + chrono::Duration::hours(1))
                .and_local_timezone(tz)
                .earliest(),
        };
        if let Some(dt) = resolved {
            return Ok(dt.into());
        }
    }
    Err(MuninnError::InvalidArgument {
        message: format!(
            "argument '{key}' must be an RFC 3339 timestamp or a YYYY-MM-DD date, got {raw:?}"
        ),
    })
}

fn opt_u64(args: &JsonObject, key: &str) -> Result<Option<u64>, MuninnError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            n.as_u64()
                .map(Some)
                .ok_or_else(|| MuninnError::InvalidArgument {
                    message: format!("argument '{key}' must be a non-negative integer"),
                })
        }
        Some(_) => Err(MuninnError::InvalidArgument {
            message: format!("argument '{key}' must be an integer"),
        }),
    }
}

/// A requested 1-based line range over a note's agent-facing content.
#[derive(Clone, Copy)]
struct LineRange {
    /// The 1-based line number of the first returned line.
    offset: u64,
    /// The maximum number of lines returned (`None`: all remaining lines).
    limit: Option<u64>,
}

/// Parse an optional positive integer, rejecting `0` with `invalid_argument`
/// (schema-level `minimum` keywords are not enforced server-side for all
/// clients).
fn opt_positive(args: &JsonObject, key: &str) -> Result<Option<u64>, MuninnError> {
    match opt_u64(args, key)? {
        Some(0) => Err(MuninnError::InvalidArgument {
            message: format!("argument '{key}' must be a positive integer"),
        }),
        other => Ok(other),
    }
}

/// Extract the optional `offset`/`limit` line range from `args`. Returns `None`
/// when neither is supplied — the whole-note read, whose response must stay
/// byte-identical to prior behavior.
fn opt_line_range(args: &JsonObject) -> Result<Option<LineRange>, MuninnError> {
    let offset = opt_positive(args, "offset")?;
    let limit = opt_positive(args, "limit")?;
    if offset.is_none() && limit.is_none() {
        return Ok(None);
    }
    Ok(Some(LineRange {
        offset: offset.unwrap_or(1),
        limit,
    }))
}

/// Parse one `read_memory_notes` entry: a bare path string or a
/// `{ path, offset?, limit? }` object. A malformed entry is a call-level
/// `invalid_argument` — per-entry errors are reserved for path resolution and IO.
fn parse_batch_entry(entry: &Value) -> Result<(String, Option<LineRange>), MuninnError> {
    match entry {
        Value::String(path) => Ok((path.clone(), None)),
        Value::Object(fields) => {
            for key in fields.keys() {
                if !matches!(key.as_str(), "path" | "offset" | "limit") {
                    return Err(MuninnError::InvalidArgument {
                        message: format!("unexpected key '{key}' in 'paths' entry"),
                    });
                }
            }
            let Some(Value::String(path)) = fields.get("path") else {
                return Err(MuninnError::InvalidArgument {
                    message: "object entries in 'paths' must carry a string 'path'".to_string(),
                });
            };
            Ok((path.clone(), opt_line_range(fields)?))
        }
        _ => Err(MuninnError::InvalidArgument {
            message:
                "argument 'paths' entries must be strings or { path, offset?, limit? } objects"
                    .to_string(),
        }),
    }
}

/// Parse one `write_memory_notes` entry: a `{ path, content, append? }` object.
/// A malformed entry is a call-level `invalid_argument` — the batch is
/// all-or-nothing, so nothing is validated further once any entry is malformed.
fn parse_batch_write_entry(entry: &Value) -> Result<(String, String, bool), MuninnError> {
    let Value::Object(fields) = entry else {
        return Err(MuninnError::InvalidArgument {
            message: "argument 'notes' entries must be { path, content, append? } objects"
                .to_string(),
        });
    };
    for key in fields.keys() {
        if !matches!(key.as_str(), "path" | "content" | "append") {
            return Err(MuninnError::InvalidArgument {
                message: format!("unexpected key '{key}' in 'notes' entry"),
            });
        }
    }
    let Some(Value::String(path)) = fields.get("path") else {
        return Err(MuninnError::InvalidArgument {
            message: "entries in 'notes' must carry a string 'path'".to_string(),
        });
    };
    let Some(Value::String(content)) = fields.get("content") else {
        return Err(MuninnError::InvalidArgument {
            message: "entries in 'notes' must carry a string 'content'".to_string(),
        });
    };
    let append = match fields.get("append") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(MuninnError::InvalidArgument {
                message: "'append' in a 'notes' entry must be a boolean".to_string(),
            });
        }
    };
    Ok((path.clone(), content.clone(), append))
}

/// Map an `evolve_core_persona` `which` value to its conventional filename and
/// optional line cap.
fn evolve_target(which: &str) -> Result<(&'static str, Option<usize>), MuninnError> {
    match which {
        "persona" => Ok(("PERSONA.md", None)),
        "prompt" => Ok(("PROMPT.md", None)),
        "rules" => Ok(("RULES.md", Some(40))),
        "user" => Ok(("USER.md", Some(100))),
        "memory" => Ok(("MEMORY.md", Some(200))),
        other => Err(MuninnError::InvalidArgument {
            message: format!(
                "which must be one of persona|prompt|rules|user|memory, got '{other}'"
            ),
        }),
    }
}

/// Parse one `evolve_core_persona` batch entry: a `{ which, content }` object.
/// A malformed entry is a call-level `invalid_argument` — the batch is
/// all-or-nothing, so nothing is validated further once any entry is malformed.
fn parse_evolve_update_entry(entry: &Value) -> Result<(String, String), MuninnError> {
    let Value::Object(fields) = entry else {
        return Err(MuninnError::InvalidArgument {
            message: "argument 'updates' entries must be { which, content } objects".to_string(),
        });
    };
    for key in fields.keys() {
        if !matches!(key.as_str(), "which" | "content") {
            return Err(MuninnError::InvalidArgument {
                message: format!("unexpected key '{key}' in 'updates' entry"),
            });
        }
    }
    let Some(Value::String(which)) = fields.get("which") else {
        return Err(MuninnError::InvalidArgument {
            message: "entries in 'updates' must carry a string 'which'".to_string(),
        });
    };
    let Some(Value::String(content)) = fields.get("content") else {
        return Err(MuninnError::InvalidArgument {
            message: "entries in 'updates' must carry a string 'content'".to_string(),
        });
    };
    Ok((which.clone(), content.clone()))
}

/// Slice `content` to a 1-based line range. Lines are delimited by `\n` with
/// delimiters preserved — `\r\n` content keeps the `\r` inside its line, and a
/// missing final newline still counts as a final line — so concatenating
/// consecutive slices reproduces the content byte-for-byte. Returns the sliced
/// content and the total line count of the full content (`0` for an empty
/// note); an offset past the last line yields empty content.
fn slice_lines(content: &str, range: LineRange) -> (String, u64) {
    let total = content.split_inclusive('\n').count() as u64;
    let lines = content
        .split_inclusive('\n')
        .skip(range.offset as usize - 1);
    let sliced = match range.limit {
        Some(limit) => lines.take(limit as usize).collect(),
        None => lines.collect(),
    };
    (sliced, total)
}

/// Generate the tool-specific field schema fragment via `schemars`, with all
/// subschemas inlined so enums appear directly rather than via `$ref`.
fn fields_schema<T: JsonSchema>() -> JsonObject {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<T>();
    match serde_json::to_value(schema) {
        Ok(Value::Object(object)) => object,
        _ => Map::new(),
    }
}

/// Merge the scheme-derived scope fields (first) with a tool's own field schema
/// into a single object input schema with `additionalProperties: false`.
fn merge_schema(scheme: &Scheme, fields: JsonObject) -> JsonObject {
    let scope = scheme.to_json_schema();

    let mut properties = Map::new();
    if let Some(Value::Object(sp)) = scope.get("properties") {
        for (k, v) in sp {
            properties.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(fp)) = fields.get("properties") {
        for (k, v) in fp {
            properties.insert(k.clone(), v.clone());
        }
    }

    let mut required = Vec::new();
    if let Some(Value::Array(sr)) = scope.get("required") {
        required.extend(sr.iter().cloned());
    }
    if let Some(Value::Array(fr)) = fields.get("required") {
        required.extend(fr.iter().cloned());
    }

    let mut out = Map::new();
    out.insert("type".to_string(), json!("object"));
    out.insert("properties".to_string(), Value::Object(properties));
    out.insert("required".to_string(), Value::Array(required));
    out.insert("additionalProperties".to_string(), json!(false));
    out
}

fn tool(name: &'static str, description: &'static str, schema: JsonObject) -> Tool {
    // `Tool` is `#[non_exhaustive]`; use its constructor rather than a struct literal.
    Tool::new(name, description, schema)
}

/// Assemble the tool list for a given scheme. The `recall_memory_notes` tool is
/// appended only when `recall_enabled`.
fn build_tools(scheme: &Scheme, recall_enabled: bool) -> Vec<Tool> {
    let mut tools = vec![
        tool(
            "list_memory_notes",
            "List the virtual paths of memory notes visible to the given scope, with pagination.",
            merge_schema(scheme, fields_schema::<ListFields>()),
        ),
        tool(
            "read_memory_note",
            "Read the UTF-8 contents of a single memory note by its virtual path. Optional `offset`/`limit` select a 1-based line range and add `total_lines` (the full note's line count) to the result. Set `backlinks` to also return the visible notes whose links resolve to it.",
            merge_schema(scheme, fields_schema::<ReadFields>()),
        ),
        tool(
            "read_memory_notes",
            "Read up to 20 memory notes in one call. Each `paths` entry is a virtual path string (whole note) or `{ path, offset?, limit? }` requesting a line range plus `total_lines`. Returns one `notes` entry per requested path, in request order: `{ path, content }` on success or `{ path, error: { code, message } }` on failure. Per-path failures do not fail the call.",
            merge_schema(scheme, fields_schema::<BatchReadFields>()),
        ),
        tool(
            "write_memory_note",
            "Atomically write the full contents of a memory note at the given virtual path.",
            merge_schema(scheme, fields_schema::<WriteFields>()),
        ),
        tool(
            "write_memory_notes",
            "Write up to 20 memory notes in one call. Each `notes` entry is `{ path, content, append? }` with the same semantics as `write_memory_note`. Every entry is validated before any file is touched — any failure rejects the whole call with no writes — and a `[[wikilink]]` may target a note created by another entry of the same call. Entries are applied in request order; returns one `{ path, bytes_written }` result per entry, in request order.",
            merge_schema(scheme, fields_schema::<BatchWriteFields>()),
        ),
        tool(
            "edit_memory_note",
            "Replace the unique occurrence of a search string in a note and persist atomically.",
            merge_schema(scheme, fields_schema::<EditFields>()),
        ),
        tool(
            "delete_memory_note",
            "Delete a single memory note by its virtual path.",
            merge_schema(scheme, fields_schema::<PathFields>()),
        ),
        tool(
            "rename_memory_note",
            "Move a single note from `path` to `new_path`, rewriting every visible incoming link (decorations preserved) to resolve to the new location. The destination must not already exist.",
            merge_schema(scheme, fields_schema::<RenameFields>()),
        ),
        tool(
            "read_note_properties",
            "Read a note's frontmatter properties as a JSON object in `{ properties }`. Absent or malformed frontmatter yields an empty object. `[[wikilink]]` targets in string values (including inside arrays and nested objects) are returned in their clean agent-facing form, matching the body view of `read_memory_note`.",
            merge_schema(scheme, fields_schema::<PropertiesReadFields>()),
        ),
        tool(
            "update_note_properties",
            "Merge a JSON object into a note's frontmatter atomically: each key is upserted, an explicit `null` deletes a key, and the note body is untouched. The block is created when absent, removed when the merge empties it, and re-serialized in normalized form (stable key order; comments and formatting are not preserved). `[[wikilink]]` targets in string values (including inside arrays and nested objects) are persisted in their resolvable on-disk form, exactly as a whole-file write would store them. Returns the full post-update `{ properties }` in the clean agent-facing form — what you write is what you read back.",
            merge_schema(scheme, fields_schema::<PropertiesUpdateFields>()),
        ),
        tool(
            "load_session_context",
            "Render the full session-context for the active scope: the five foundational files (persona, prompt, rules, user, memory) woven into the configured template, plus a pointer to the layout surface. Returns { rendered, missing }.",
            merge_schema(scheme, fields_schema::<EmptyFields>()),
        ),
        tool(
            "evolve_core_persona",
            "Atomically replace foundational session files (persona|prompt|rules|user|memory): a single file via `which` + `content`, or several in one call via an `updates` array of 1–5 { which, content } entries (no duplicate `which`), validated as a unit before any write — supply exactly one form. When bootstrapping missing foundational files, interview the user first (identity, role, working style, boundaries), distill the answers into your own concise wording, then commit all affected files in one batch call. Enforces caps: RULES.md ≤ 40 lines, USER.md ≤ 100 lines, MEMORY.md ≤ 200 lines.",
            merge_schema(scheme, fields_schema::<EvolveFields>()),
        ),
        tool(
            "update_task_heartbeat",
            "Atomically replace the scope's HEARTBEAT.md.",
            merge_schema(scheme, fields_schema::<ContentOnlyFields>()),
        ),
        tool(
            "append_diary_entry",
            "Append a timestamped section to today's diary file for the active scope. Accepts an optional `title` for the entry heading.",
            merge_schema(scheme, fields_schema::<DiaryFields>()),
        ),
    ];
    if recall_enabled {
        tools.push(tool(
            "recall_memory_notes",
            "Search memory notes by content within the caller's visible set. Returns ranked hits as { path, score (0-1), snippets, modified_at }. Supply at least one of `query` (full-text), `regex`, `filters` (frontmatter properties; tantivy backend only), or the `modified_after`/`modified_before` time bounds. With time bounds alone, hits are ordered by recency. Paginated like list_memory_notes.",
            merge_schema(scheme, fields_schema::<RecallFields>()),
        ));
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant named by an RFC 3339 string, for comparison.
    fn st(rfc3339: &str) -> SystemTime {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .into()
    }

    #[test]
    fn time_bound_parses_rfc3339_with_any_offset() {
        let offset = parse_time_bound("modified_after", "2026-06-10T12:30:00+02:00", Tz::UTC);
        assert_eq!(offset.unwrap(), st("2026-06-10T10:30:00Z"));
        let zulu = parse_time_bound("modified_after", "2026-06-10T10:30:00Z", Tz::UTC);
        assert_eq!(zulu.unwrap(), st("2026-06-10T10:30:00Z"));
    }

    #[test]
    fn time_bound_resolves_bare_date_in_configured_timezone() {
        let taipei = parse_time_bound("modified_after", "2026-06-10", Tz::Asia__Taipei);
        assert_eq!(taipei.unwrap(), st("2026-06-09T16:00:00Z"));
        let utc = parse_time_bound("modified_after", "2026-06-10", Tz::UTC);
        assert_eq!(utc.unwrap(), st("2026-06-10T00:00:00Z"));
    }

    #[test]
    fn time_bound_rejects_anything_else() {
        for raw in ["last tuesday", "2026-06", "2026-06-10T12:00:00", ""] {
            let err = parse_time_bound("modified_after", raw, Tz::UTC).unwrap_err();
            assert_eq!(
                err.code(),
                crate::error::ErrorCode::InvalidArgument,
                "input {raw:?}"
            );
        }
    }

    /// Shorthand for a `LineRange` with both bounds.
    fn range(offset: u64, limit: Option<u64>) -> LineRange {
        LineRange { offset, limit }
    }

    #[test]
    fn slice_lines_returns_mid_file_range_with_delimiters() {
        let content = "a\nb\nc\nd\n";
        assert_eq!(
            slice_lines(content, range(2, Some(2))),
            ("b\nc\n".into(), 4)
        );
    }

    #[test]
    fn slice_lines_offset_alone_reads_to_the_end() {
        assert_eq!(
            slice_lines("a\nb\nc\n", range(2, None)),
            ("b\nc\n".into(), 3)
        );
    }

    #[test]
    fn slice_lines_empty_note_has_zero_lines() {
        assert_eq!(slice_lines("", range(1, None)), (String::new(), 0));
    }

    #[test]
    fn slice_lines_missing_final_newline_counts_as_a_line() {
        let content = "a\nb";
        assert_eq!(slice_lines(content, range(2, Some(1))), ("b".into(), 2));
        // Concatenating consecutive slices reproduces the content byte-for-byte.
        let (first, _) = slice_lines(content, range(1, Some(1)));
        let (second, _) = slice_lines(content, range(2, Some(1)));
        assert_eq!(format!("{first}{second}"), content);
    }

    #[test]
    fn slice_lines_keeps_crlf_inside_the_line() {
        let content = "a\r\nb\r\nc";
        assert_eq!(slice_lines(content, range(1, Some(1))), ("a\r\n".into(), 3));
        assert_eq!(slice_lines(content, range(3, None)), ("c".into(), 3));
    }

    #[test]
    fn slice_lines_offset_past_eof_is_empty() {
        assert_eq!(slice_lines("a\nb\n", range(5, None)), (String::new(), 2));
    }
}
