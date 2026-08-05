# configuration Delta

## MODIFIED Requirements

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
