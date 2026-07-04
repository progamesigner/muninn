# container-image Specification

## Purpose
TBD - created by archiving change add-container-image-release. Update Purpose after archive.
## Requirements
### Requirement: Minimal static container image

The project SHALL publish a container image whose runtime layer is built `FROM scratch` and contains a single statically linked (musl) `muninn` binary with no shell, package manager, or CA certificate bundle.

#### Scenario: Image contains only the binary

- **WHEN** the published image's filesystem is inspected
- **THEN** it contains the `muninn` binary and no shell, package manager, or additional OS userland

#### Scenario: Binary is statically linked

- **WHEN** the `muninn` binary inside the image is examined for dynamic dependencies
- **THEN** it has no dynamic linker dependencies (statically linked against musl)

#### Scenario: Server starts without external runtime files

- **WHEN** the container is run with only a vault directory mounted
- **THEN** the server starts and serves the HTTP transport without requiring `/tmp`, a CA bundle, or system timezone data

### Requirement: Multi-stage build with a minimal allowlisted context

The image SHALL be produced by a multi-stage `Dockerfile` (a builder stage that compiles the binary and a final runtime stage that contains only the binary), and the build context SHALL be constrained by a `.dockerignore` that ignores everything (`*`) and re-includes only the inputs required to compile — `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, and the `src` Rust sources.

#### Scenario: Final stage carries no build tooling

- **WHEN** the published runtime image is inspected
- **THEN** it contains none of the builder-stage toolchain (no Rust compiler, cargo, or zig) — only the `muninn` binary

#### Scenario: Build context excludes non-source files

- **WHEN** the image is built
- **THEN** the `.dockerignore` denies all paths by default and re-includes only `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, and `src`, so directories such as `target/`, `.git/`, `docs/`, `tests/`, and `openspec/` are absent from the build context

#### Scenario: Build succeeds from the allowlisted context alone

- **WHEN** the image is built using only the allowlisted files
- **THEN** compilation succeeds without referencing any excluded file

### Requirement: Multi-architecture publication

The image SHALL be published as a multi-architecture manifest covering `linux/amd64` and `linux/arm64`.

#### Scenario: Manifest lists both architectures

- **WHEN** the published image manifest is inspected
- **THEN** it includes entries for both `linux/amd64` and `linux/arm64`

#### Scenario: Pull selects the host architecture

- **WHEN** the image is pulled on an `arm64` host
- **THEN** the `linux/arm64` variant is selected and runs natively

### Requirement: GitHub Container Registry publication on tag

The image SHALL be pushed to the GitHub Container Registry at `ghcr.io/progamesigner/muninn`, and publication SHALL occur only for tag-push builds that have passed the `check` job.

#### Scenario: Tag push publishes the image

- **WHEN** a Git tag is pushed and the `check` job succeeds
- **THEN** the image is built and pushed to `ghcr.io/progamesigner/muninn`

#### Scenario: Non-tag builds do not publish

- **WHEN** a build runs for a branch push or pull request (not a tag)
- **THEN** no image is pushed to the registry
### Requirement: Release tag scheme

Each release publication SHALL produce three tags: the semantic version `:{version}`, the moving `:latest`, and an immutable `:sha-<gitsha>`.

#### Scenario: Version tag matches the release

- **WHEN** the tag `v1.2.3` is pushed
- **THEN** the image is published with tag `:1.2.3`

#### Scenario: Latest and sha tags are published

- **WHEN** a release is published
- **THEN** a `:latest` tag and a `:sha-<gitsha>` tag referencing the same image are also published

### Requirement: Dynamic OCI image labels

The image SHALL carry the following ten OpenContainers labels, each populated from a build-derived or source-of-truth value rather than a hardcoded constant: `org.opencontainers.image.created`, `.revision`, `.version`, `.title`, `.description`, `.source`, `.url`, `.authors`, `.documentation`, and `.vendor`.

#### Scenario: Build-derived labels reflect the build

- **WHEN** the published image labels are inspected
- **THEN** `created` holds the build timestamp, `revision` holds the Git commit SHA, and `version` holds the release version

#### Scenario: Source-of-truth labels derive from project metadata

- **WHEN** the published image labels are inspected
- **THEN** `title`, `description`, `source`, and `url` derive from repository/`Cargo.toml` metadata, and `authors`, `documentation`, and `vendor` derive from `Cargo.toml` (via `cargo metadata`) and the owning organization

#### Scenario: No label is empty

- **WHEN** any of the ten required labels is read from the published image
- **THEN** it has a non-empty value

### Requirement: Nonroot runtime user

The container SHALL run as a non-root numeric user by default.

#### Scenario: Default user is non-root

- **WHEN** the container is started without an overriding `--user` flag
- **THEN** the process runs as a non-root numeric UID

### Requirement: HTTP transport default and health probing

The container's default entrypoint SHALL serve the HTTP transport, and because a `scratch` image cannot run an embedded `HEALTHCHECK`, liveness SHALL be observable via the `GET /health` route for an external probe.

#### Scenario: Default run serves HTTP

- **WHEN** the container is run with no transport argument
- **THEN** the server listens for the Streamable HTTP transport

#### Scenario: Health route answers an external probe

- **WHEN** an external orchestrator probes `GET /health` against the running container
- **THEN** the server responds with a success status indicating liveness
