## MODIFIED Requirements

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

### Requirement: GitHub Container Registry publication on tag

The image SHALL be pushed to the GitHub Container Registry at `ghcr.io/progamesigner/muninn`, and publication SHALL occur only for tag-push builds that have passed the `check` job.

#### Scenario: Tag push publishes the image

- **WHEN** a Git tag is pushed and the `check` job succeeds
- **THEN** the image is built and pushed to `ghcr.io/progamesigner/muninn`

#### Scenario: Non-tag builds do not publish

- **WHEN** a build runs for a branch push or pull request (not a tag)
- **THEN** no image is pushed to the registry
