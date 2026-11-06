# syntax=docker/dockerfile:1

# ---- builder ----------------------------------------------------------------
# Alpine's Rust image is musl-native, so a plain `cargo build` emits a static
# binary with no cross linker. The builder runs on the target platform itself:
# CI builds each arch on a native runner (amd64 and arm64), never under QEMU.
FROM rust:1.99.1-alpine AS builder

# Docker sets TARGETARCH to the architecture currently being assembled.
ARG TARGETARCH

WORKDIR /src

# Resolve the musl target triple for this architecture and stash it so the
# build step and the runtime COPY agree on the output path.
RUN case "$TARGETARCH" in \
        amd64) echo "x86_64-unknown-linux-musl"  > /tmp/triple ;; \
        arm64) echo "aarch64-unknown-linux-musl" > /tmp/triple ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac

# Make sure the toolchain selected by rust-toolchain.toml has the musl target.
# On Alpine this is the host target, so it is normally already installed.
COPY rust-toolchain.toml ./
RUN rustup target add "$(cat /tmp/triple)"

# Compile the static binary. The allowlisted .dockerignore keeps the context to
# Cargo.*, rust-toolchain.toml, src/, and benches/. benches/ is copied only so
# cargo can resolve the [[bench]] target declared in Cargo.toml — manifest
# parsing fails without it. Copy the result out of the cache mount (and
# pre-create the vault mountpoint) in the same layer so both persist.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches

# Optional cargo features for the build, e.g. CARGO_FEATURES=recall-tantivy.
# Empty (the default) builds the lightweight `simple` backend; declared here so
# toggling it does not invalidate the toolchain layers above.
ARG CARGO_FEATURES=""
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --target "$(cat /tmp/triple)" \
        ${CARGO_FEATURES:+--features "$CARGO_FEATURES"} \
 && cp "target/$(cat /tmp/triple)/release/muninn" /muninn \
 && mkdir -p /vault

# ---- runtime ----------------------------------------------------------------
# Empty base: the static binary needs no libc, CA bundle, shell, or /tmp.
FROM scratch AS runtime

COPY --from=builder /muninn /muninn
# Vault mountpoint owned by the nonroot uid so atomic writes (NamedTempFile in
# the vault dir) and unmounted / anonymous-volume runs succeed.
COPY --from=builder --chown=65532:65532 /vault /vault

# Run unprivileged. The static binary resolves no usernames, so a bare numeric
# uid:gid works without /etc/passwd.
USER 65532:65532

# Recall backend default baked into the image. Defaults to `simple` so a plain
# `docker build .` matches the lightweight binary; the published image overrides
# it to `tantivy` (built with CARGO_FEATURES=recall-tantivy). Setting `tantivy`
# without that feature is harmless — the engine falls back to `simple`.
ARG MUNINN_RECALL_BACKEND=simple

# MUNINN_ROOT_DIR is required and canonicalised at startup — point it at the
# volume. Bind HTTP to all interfaces; the built-in default (127.0.0.1) is
# unreachable from outside the container.
ENV MUNINN_ROOT_DIR=/vault \
    MUNINN_TRANSPORT=http \
    MUNINN_HTTP_BIND=0.0.0.0:8000 \
    MUNINN_RECALL_BACKEND=${MUNINN_RECALL_BACKEND}

VOLUME ["/vault"]
EXPOSE 8000

ENTRYPOINT ["/muninn"]
