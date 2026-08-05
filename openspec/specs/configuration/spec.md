# configuration Specification

## Purpose
TBD - created by archiving change build-agentmem-mcp-server. Update Purpose after archive.
## Requirements
### Requirement: Configuration source
The system SHALL be configured exclusively via environment variables. CLI flags MAY be accepted as overrides, but the canonical configuration surface is the environment.

#### Scenario: Env vars are read at startup
- **WHEN** the server is launched
- **THEN** it reads `MUNINN_ROOT_DIR`, `MUNINN_AGENTS_DIR`, `MUNINN_VFS_SCHEME`, `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE`, `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE`, `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE`, `MUNINN_POLICY`, `MUNINN_TRANSPORT`, `MUNINN_HTTP_BIND`, `MUNINN_HTTP_BEARER`, `MUNINN_HTTP_ALLOWED_HOSTS`, `MUNINN_TIMEZONE`, `MUNINN_HONOR_IGNORE_FILES`, `MUNINN_INCLUDE_HIDDEN`, and `MUNINN_LOG` from the process environment

#### Scenario: CLI flag overrides env var
- **WHEN** the server is launched with `--http-bind 0.0.0.0:9000` and `MUNINN_HTTP_BIND` is also set
- **THEN** the CLI flag wins and the bind address is `0.0.0.0:9000`

### Requirement: Required configuration variables
The system SHALL require `MUNINN_ROOT_DIR` to be present and valid at startup, and SHALL refuse to start otherwise. All other variables have defaults.

#### Scenario: Missing root dir
- **WHEN** `MUNINN_ROOT_DIR` is unset
- **THEN** the process exits non-zero with a stderr message naming the variable

#### Scenario: Root dir is not a directory
- **WHEN** `MUNINN_ROOT_DIR` points to a path that does not exist or is not a directory
- **THEN** the process exits non-zero with a stderr message naming the variable and the offending value

#### Scenario: All other variables have defaults
- **WHEN** only `MUNINN_ROOT_DIR` is set and every other variable is unset
- **THEN** the server starts successfully with: agents folder `Agents`, scheme `<agent>.<user>`, global session-context template path `<root>/AGENT_SESSION_CONTEXT.md`, global session-bootstrap template path `<root>/AGENT_SESSION_BOOTSTRAP.md`, global memory-layout template path `<root>/AGENT_MEMORY_LAYOUT.md`, policy `namespaced`, transport `http`, bind `127.0.0.1:8000`, timezone `UTC`, ignore files honoured, hidden entries excluded

### Requirement: Session-context template file configuration
The system SHALL honour `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE` as the filesystem path to the global session-context template document. The default value SHALL be `<root>/AGENT_SESSION_CONTEXT.md`. A relative value SHALL be interpreted relative to the vault root. The configured file need not exist; when it is absent, the system SHALL fall back to the compiled-in default template (subject to the layered resolution defined in the memory-tools capability).

#### Scenario: Default global template path
- **WHEN** `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE` is unset
- **THEN** the global session-context template path resolves to `<root>/AGENT_SESSION_CONTEXT.md`

#### Scenario: Custom global template path
- **WHEN** `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE=/etc/muninn/bootstrap.md`
- **THEN** the server reads the global session-context template from that path

#### Scenario: Configured file absent is not an error
- **WHEN** `MUNINN_SESSION_CONTEXT_TEMPLATE_FILE` points to a path that does not exist
- **THEN** the server starts successfully and the renderer falls back to the compiled-in default template

### Requirement: Session-bootstrap template file configuration
The system SHALL honour `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE` as the filesystem path to the global session-bootstrap (lean) template document. The default value SHALL be `<root>/AGENT_SESSION_BOOTSTRAP.md`. A relative value SHALL be interpreted relative to the vault root. The configured file need not exist; when it is absent, the system SHALL fall back to the compiled-in default bootstrap template (subject to the layered resolution defined in the memory-tools capability).

#### Scenario: Default global bootstrap template path
- **WHEN** `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE` is unset
- **THEN** the global session-bootstrap template path resolves to `<root>/AGENT_SESSION_BOOTSTRAP.md`

#### Scenario: Custom global bootstrap template path
- **WHEN** `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE=/etc/muninn/bootstrap.md`
- **THEN** the server reads the global session-bootstrap template from that path

#### Scenario: Configured bootstrap file absent is not an error
- **WHEN** `MUNINN_SESSION_BOOTSTRAP_TEMPLATE_FILE` points to a path that does not exist
- **THEN** the server starts successfully and the renderer falls back to the compiled-in default bootstrap template

### Requirement: Memory-layout template file configuration
The system SHALL honour `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE` as the filesystem path to the global memory-layout template document. The default value SHALL be `<root>/AGENT_MEMORY_LAYOUT.md`. A relative value SHALL be interpreted relative to the vault root. The configured file need not exist; when it is absent, the system SHALL fall back to the compiled-in default layout content (subject to the layered resolution defined in the memory-tools capability).

#### Scenario: Default global layout template path
- **WHEN** `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE` is unset
- **THEN** the global memory-layout template path resolves to `<root>/AGENT_MEMORY_LAYOUT.md`

#### Scenario: Custom global layout template path
- **WHEN** `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE=/etc/muninn/layout.md`
- **THEN** the server reads the global memory-layout template from that path

#### Scenario: Configured layout file absent is not an error
- **WHEN** `MUNINN_MEMORY_LAYOUT_TEMPLATE_FILE` points to a path that does not exist
- **THEN** the server starts successfully and the renderer falls back to the compiled-in default layout content

### Requirement: Agents folder configuration
The system SHALL honour `MUNINN_AGENTS_DIR` as the relative folder name under the vault root that delimits the scoped/suffixed region. The default value SHALL be `Agents`. A value of `.` or the empty string SHALL be interpreted as "the agents folder IS the vault root".

#### Scenario: Default agents folder
- **WHEN** `MUNINN_AGENTS_DIR` is unset
- **THEN** the agents folder resolves to `<root>/Agents/`

#### Scenario: Custom subdirectory
- **WHEN** `MUNINN_AGENTS_DIR=memory`
- **THEN** the agents folder resolves to `<root>/memory/` and any virtual path under `memory/` is treated as inside the agents region

#### Scenario: Vault root is the agents folder
- **WHEN** `MUNINN_AGENTS_DIR=.` (or empty)
- **THEN** the agents folder resolves to the vault root itself; every virtual path inside the vault is inside the agents region and the "outside the agents folder" region is empty

#### Scenario: Path traversal in agents dir is rejected
- **WHEN** `MUNINN_AGENTS_DIR=../escape`
- **THEN** the process exits non-zero with a stderr message naming the variable and the offending value

### Requirement: VFS scheme
The system SHALL honour `MUNINN_VFS_SCHEME` as a dotted scheme string composed of literal segments and `<ident>` placeholders. The default value SHALL be `<agent>.<user>`. The scheme's placeholders define the required scope parameters on every tool call.

#### Scenario: Default scheme requires agent and user
- **WHEN** `MUNINN_VFS_SCHEME` is unset
- **THEN** every tool's input schema includes required string fields `agent` and `user`

#### Scenario: Single-key scheme
- **WHEN** `MUNINN_VFS_SCHEME=<agent>`
- **THEN** every tool's input schema includes a required string field `agent` and no `user` field

#### Scenario: Empty scheme disables suffixing
- **WHEN** `MUNINN_VFS_SCHEME=` (empty string)
- **THEN** tool input schemas include no scope fields, no VFS suffix is applied, and no own-scope filtering is performed inside the agents folder

#### Scenario: Custom multi-key scheme
- **WHEN** `MUNINN_VFS_SCHEME=<team>.<agent>.<env>.<user>`
- **THEN** every tool's input schema includes four required string fields `team`, `agent`, `env`, `user`; the rendered suffix for `{team:"platform", agent:"jarvis", env:"prod", user:"tony"}` is `platform.jarvis.prod.tony`

#### Scenario: Literal segments in scheme
- **WHEN** `MUNINN_VFS_SCHEME=v1.<agent>.<user>`
- **THEN** the rendered suffix for `{agent:"jarvis", user:"tony"}` is `v1.jarvis.tony` and tool schemas require only `agent` and `user`

#### Scenario: Malformed scheme
- **WHEN** `MUNINN_VFS_SCHEME=<agent` (unclosed bracket) or contains characters outside the grammar
- **THEN** the process exits non-zero with a stderr message naming the variable and pointing at the offending character

#### Scenario: Invalid placeholder name
- **WHEN** a placeholder ident does not match `[A-Za-z_][A-Za-z0-9_]*` (for example `<1bad>` or `<a-b>`)
- **THEN** the process exits non-zero with a stderr message naming the variable and the offending placeholder

### Requirement: Server-wide policy
The system SHALL honour `MUNINN_POLICY` as exactly one of `scoped`, `namespaced`, `readonly`, `readwrite`. The default value SHALL be `namespaced`. The policy governs read/write permissions across the whole vault, in concert with the agents-folder boundary.

#### Scenario: Default policy
- **WHEN** `MUNINN_POLICY` is unset
- **THEN** the effective policy is `namespaced`: inside the agents folder, own-scope read/write with suffix; outside the agents folder but inside the vault root, read-only with no suffix; outside the vault root, denied

#### Scenario: scoped policy denies outside agents folder
- **WHEN** `MUNINN_POLICY=scoped` and an agent attempts to read a path outside the agents folder but inside the vault root
- **THEN** the operation is refused with code `path_not_permitted`

#### Scenario: readonly forbids writes everywhere
- **WHEN** `MUNINN_POLICY=readonly` and any tool that performs a write is invoked
- **THEN** the operation is refused with code `write_denied`, regardless of whether the target is inside or outside the agents folder

#### Scenario: readwrite permits writes outside agents folder
- **WHEN** `MUNINN_POLICY=readwrite` and an agent writes to a path outside the agents folder but inside the vault root
- **THEN** the write succeeds, no VFS suffix is applied, and every other agent sees the resulting file at the same virtual path

#### Scenario: Invalid policy
- **WHEN** `MUNINN_POLICY` is set to any value other than the four accepted strings
- **THEN** the process exits non-zero with a stderr message listing the accepted values

### Requirement: HTTP transport variables
The system SHALL, when the active transport is `http`, accept an optional `MUNINN_HTTP_BIND` socket address, an optional `MUNINN_HTTP_BEARER` static token, an optional `MUNINN_HTTP_TOKENS_FILE` path, and an optional `MUNINN_HTTP_ALLOWED_HOSTS` allow-list. `MUNINN_HTTP_BIND` SHALL default to `127.0.0.1:8000` when the variable is unset, so local development needs no CORS or auth configuration.

`MUNINN_HTTP_ALLOWED_HOSTS` SHALL be a comma-separated list of `Host` authorities — each a hostname or `host:port` — that the Streamable HTTP transport accepts in the inbound `Host` header. When the variable is unset (or empty after trimming), the system SHALL leave the transport's built-in loopback-only default in effect (`localhost`, `127.0.0.1`, `::1`). The single value `*` SHALL disable `Host` validation entirely. Surrounding whitespace around each entry SHALL be trimmed and empty entries SHALL be ignored. The variable SHALL be overridable by a mirroring `--http-allowed-hosts` CLI flag, with the CLI flag taking precedence over the environment variable.

`MUNINN_HTTP_TOKENS_FILE` SHALL name a JSON file of the form `{ "tokens": [ { "token": <string>, "scopes": { <placeholder>: <exact-or-*> , … } }, … ] }`, read once at startup and mirrored by a `--http-tokens-file` CLI flag. Validation SHALL fail startup (with a message that does not echo token values) when the file is missing or unreadable, when JSON parsing fails, when any entry's `token` is empty, when a `scopes` object names a key that is not an active scheme placeholder or omits one of the placeholders, or when a scope value is neither an exact string nor the single character `*`. Token values SHALL NOT appear in logs or in `--print-config` output. The variable SHALL be ignored under the `stdio` transport.

#### Scenario: Default bind address is loopback
- **WHEN** transport is `http` and `MUNINN_HTTP_BIND` is unset
- **THEN** the server binds `127.0.0.1:8000` and the chosen address is logged at startup

#### Scenario: Non-loopback bind without bearer logs a warning
- **WHEN** `MUNINN_HTTP_BIND=0.0.0.0:8000` is set and both `MUNINN_HTTP_BEARER` and `MUNINN_HTTP_TOKENS_FILE` are unset
- **THEN** the server starts and emits a single `WARN`-level log line indicating the endpoint is reachable from outside the host and is unauthenticated

#### Scenario: Allowed hosts default to loopback only
- **WHEN** transport is `http` and `MUNINN_HTTP_ALLOWED_HOSTS` is unset
- **THEN** the transport accepts the `Host` values `localhost`, `127.0.0.1`, and `::1` and rejects all others, matching the prior default

#### Scenario: Configured allowed hosts are accepted
- **WHEN** `MUNINN_HTTP_ALLOWED_HOSTS=muninn.svc.cluster.local,muninn.example.com:8000` is set
- **THEN** the parsed list is applied so that the trimmed authorities `muninn.svc.cluster.local` and `muninn.example.com:8000` are accepted in the inbound `Host` header

#### Scenario: Wildcard disables Host validation
- **WHEN** `MUNINN_HTTP_ALLOWED_HOSTS=*` is set
- **THEN** the transport accepts any `Host` header value and the server emits a single `WARN`-level log line noting that `Host` validation is disabled

#### Scenario: Tokens file is validated at startup
- **WHEN** `MUNINN_HTTP_TOKENS_FILE` points to a file whose entry grants a key that is not a scheme placeholder (e.g. `tenant` under the scheme `<agent>.<user>`), or uses a partial pattern like `"t*"`
- **THEN** the server refuses to start with an error naming the offending key or value, without echoing any token

#### Scenario: Tokens never appear in output
- **WHEN** the server starts with a valid tokens file and `--print-config` is requested or startup logs are inspected
- **THEN** no token value appears in any output

#### Scenario: Stdio ignores HTTP variables
- **WHEN** `MUNINN_TRANSPORT=stdio` and `MUNINN_HTTP_ALLOWED_HOSTS` or `MUNINN_HTTP_TOKENS_FILE` is set
- **THEN** no TCP listener is opened and the values of the HTTP-only variables are ignored

### Requirement: Visibility filter variables
The system SHALL honour `MUNINN_HONOR_IGNORE_FILES` and `MUNINN_INCLUDE_HIDDEN` as strict booleans (`true`/`false`) that control which files are visible to and addressable by agents. The defaults SHALL be `MUNINN_HONOR_IGNORE_FILES=true` and `MUNINN_INCLUDE_HIDDEN=false`. When `MUNINN_HONOR_IGNORE_FILES=true`, the system SHALL consult a generic `.ignore` file in addition to `.gitignore` and `.obsidianignore`.

The system SHALL additionally accept `MUNINN_INCLUDE_HIDDEN_GLOBS`, a comma-separated list of gitignore-style glob patterns evaluated relative to the vault root. Each pattern exempts matching dot-paths — and their entire subtree — from hidden-segment exclusion, so that a specific dotfile or dot-directory (e.g. `.obsidian/**`) can be exposed while other dotfiles stay excluded. The default SHALL be empty (no exemptions). Each of the boolean variables and the glob list SHALL be overridable by a mirroring CLI flag (`--honor-ignore-files`, `--include-hidden`, `--include-hidden-globs`), with the CLI flag taking precedence over the environment variable.

#### Scenario: Defaults exclude hidden files and honour ignore files
- **WHEN** neither variable is set
- **THEN** any path whose any segment begins with `.` is excluded from all tools, and any path matched by a `.ignore`, `.gitignore`, or `.obsidianignore` rule inside the vault is also excluded

#### Scenario: Including hidden files
- **WHEN** `MUNINN_INCLUDE_HIDDEN=true`
- **THEN** dotfiles and dotdirectories (excluding ignored ones, unless ignore is also disabled) are visible to and addressable by agents

#### Scenario: Include-hidden glob list exposes selected dot-paths
- **WHEN** `MUNINN_INCLUDE_HIDDEN=false` and `MUNINN_INCLUDE_HIDDEN_GLOBS=.obsidian/**,**/.config`
- **THEN** dot-paths matching either glob (and everything beneath them) are visible to and addressable by agents, while all other dot-paths remain excluded

#### Scenario: Empty include-hidden glob list is the default
- **WHEN** `MUNINN_INCLUDE_HIDDEN_GLOBS` is unset or empty
- **THEN** no dot-path exemption applies and hidden filtering behaves exactly as when only `MUNINN_INCLUDE_HIDDEN` is considered

#### Scenario: CLI flag overrides environment for the glob list
- **WHEN** `MUNINN_INCLUDE_HIDDEN_GLOBS=.cache/**` is set in the environment and the process is started with `--include-hidden-globs .obsidian/**`
- **THEN** the effective include-hidden glob list is `.obsidian/**` and `.cache/**` is not applied

#### Scenario: Disabling ignore-file enforcement
- **WHEN** `MUNINN_HONOR_IGNORE_FILES=false`
- **THEN** `.ignore`, `.gitignore`, and `.obsidianignore` patterns are not consulted; hidden filtering still applies according to `MUNINN_INCLUDE_HIDDEN` and `MUNINN_INCLUDE_HIDDEN_GLOBS`

#### Scenario: Invalid boolean
- **WHEN** either boolean variable is set to a value other than `true` or `false`
- **THEN** the process exits non-zero with a stderr message naming the variable and the offending value

#### Scenario: Invalid glob pattern fails fast
- **WHEN** `MUNINN_INCLUDE_HIDDEN_GLOBS` contains an entry that is not a valid gitignore-style glob
- **THEN** the process exits non-zero with a stderr message naming the variable and the offending pattern

### Requirement: Timezone for date-derived tools
The system SHALL honour `MUNINN_TIMEZONE` as an IANA timezone identifier (e.g. `Asia/Taipei`, `UTC`). The default value SHALL be `UTC`. The timezone SHALL be used by any tool that derives a date or time from "now" (notably `append_daily_entry`).

#### Scenario: Default timezone is UTC
- **WHEN** `MUNINN_TIMEZONE` is unset and `append_daily_entry` is called at `2026-05-25T23:30:00Z`
- **THEN** the resolved virtual path is `<agents_dir>/diary/2026-05-25.md`

#### Scenario: Custom timezone shifts the date boundary
- **WHEN** `MUNINN_TIMEZONE=Asia/Taipei` and `append_daily_entry` is called at `2026-05-25T23:30:00Z` (07:30 next day in Taipei)
- **THEN** the resolved virtual path is `<agents_dir>/diary/2026-05-26.md`

#### Scenario: Invalid timezone fails fast
- **WHEN** `MUNINN_TIMEZONE` is set to a string that is not a valid IANA timezone
- **THEN** the process exits non-zero with a stderr message naming the variable and the offending value

### Requirement: Logging configuration
The system SHALL honour `MUNINN_LOG` as a `tracing_subscriber::EnvFilter` directive string. The default level SHALL be `info` for the `muninn` crate and `warn` for everything else.

#### Scenario: Default filter
- **WHEN** `MUNINN_LOG` is unset
- **THEN** the active filter is `warn,muninn=info`

#### Scenario: Custom filter applied
- **WHEN** `MUNINN_LOG=debug,muninn=trace`
- **THEN** the active filter is exactly that string and is logged once at startup

### Requirement: Recall configuration variables
The system SHALL read recall configuration from the environment, with CLI flag
overrides consistent with the other configuration variables. `MUNINN_RECALL_BACKEND`
SHALL select the backend, accepting `simple`, `tantivy`, and `off`, and SHALL default
to `simple`. The system SHALL additionally accept configuration for the filesystem
watcher debounce window, the regex scan guard (a byte and/or time cap), and the
recall memory/eviction bound. The `tantivy` backend SHALL be compiled in only under
its optional cargo feature. The system SHALL accept an optional
`MUNINN_RECALL_INDEX_DIR` variable naming a directory for persistent on-disk
indexes; when unset — the default — indexes are held in memory only and no index
data is written to disk. When set, the value MUST be an absolute path that does
not lie inside the vault root (the recursive vault watcher would observe index
writes); a value inside the vault root SHALL fail startup with a human-readable
error. When set while the effective backend is not `tantivy`, the setting SHALL
be ignored with a startup warning.

#### Scenario: Default backend
- **WHEN** `MUNINN_RECALL_BACKEND` is unset
- **THEN** the configured backend is `simple`

#### Scenario: Invalid backend value fails fast
- **WHEN** `MUNINN_RECALL_BACKEND` is set to a value other than `simple`, `tantivy`,
  or `off`
- **THEN** the process writes a human-readable line to stderr naming the variable and
  exits with a non-zero status, consistent with other misconfiguration handling

#### Scenario: No on-disk index by default
- **WHEN** recall is enabled under any backend and `MUNINN_RECALL_INDEX_DIR` is unset
- **THEN** the server requires no index directory and writes no index data to disk

#### Scenario: Index dir inside the vault root is rejected
- **WHEN** `MUNINN_RECALL_INDEX_DIR` resolves to a path equal to or underneath
  `MUNINN_ROOT_DIR`
- **THEN** the process writes a human-readable line to stderr naming both variables
  and explaining the conflict, exits with a non-zero status, and does NOT begin
  serving requests

#### Scenario: Index dir with a non-tantivy backend warns and is ignored
- **WHEN** `MUNINN_RECALL_INDEX_DIR` is set and the effective backend is `simple`
  (including a tantivy request falling back without the cargo feature)
- **THEN** the server starts normally, holds indexes in memory only, and logs a
  warning that the index directory is ignored
