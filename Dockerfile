# syntax=docker/dockerfile:1
#
# The `sparkles` server with its web UI, in a Debian slim image. `docker compose up --build`
# builds and runs it (compose.yaml); docs/USAGE.md#docker covers volumes, configuration,
# authentication, upgrades and backups.
#
# Stages, as the Nix packages build them (nix/fmt-wasm.nix, nix/ui.nix, nix/package.nix):
#
#   wasm-bindgen  the wasm-bindgen CLI of the version Cargo.lock pins for the crate
#   fmt-wasm      the formatter's WebAssembly module (scripts/build-fmt-wasm.sh)
#   ui            the SvelteKit UI with that module, into ui/build
#   server        `cargo build --release -p sparkles-server --locked`, which embeds ui/build
#   runtime       the binary, CA certificates and a non-root user
#
# BuildKit cache mounts keep the cargo registry, the pnpm store and the two cargo target
# directories between builds, so a rebuild compiles only the crates that changed. Pass
# `--build-arg BUILD_JOBS=N` to cap the parallel compile jobs on a busy machine.

# Rust is "stable" in rust-toolchain.toml; the image pins the release that was stable when
# this file was last updated. Node and pnpm are the versions mise.toml pins.
ARG RUST_VERSION=1.98.1
ARG NODE_VERSION=24.21.0
ARG PNPM_VERSION=10.34.5
# must equal the wasm-bindgen crate's version in Cargo.lock (scripts/build-fmt-wasm.sh checks)
ARG WASM_BINDGEN_VERSION=0.2.129
# the builders and the runtime use the same Debian release, so the binary finds the glibc
# it was linked against
ARG DEBIAN_RELEASE=bookworm

# ------------------------------------------------------------------------------ Rust ----

FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS rust
# rust-toolchain.toml is not copied into the image, so cargo uses the image's toolchain
# instead of downloading the latest stable one. Its browser target is added here, before
# the sources, so that a source change does not download it again.
RUN rustup target add wasm32-unknown-unknown
WORKDIR /src

FROM rust AS wasm-bindgen
ARG WASM_BINDGEN_VERSION
ARG BUILD_JOBS
RUN --mount=type=cache,id=sparkles-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    cargo install --locked --jobs "${BUILD_JOBS:-$(nproc)}" --root /opt/wasm-bindgen \
    wasm-bindgen-cli --version "${WASM_BINDGEN_VERSION}"

# The workspace's manifests and sources, shared by the formatter and server builds.
FROM rust AS sources
COPY Cargo.toml Cargo.lock ./
COPY vendor vendor
COPY crates crates

FROM sources AS fmt-wasm
ARG BUILD_JOBS
COPY --from=wasm-bindgen /opt/wasm-bindgen/bin/wasm-bindgen /usr/local/bin/wasm-bindgen
COPY scripts/build-fmt-wasm.sh scripts/build-fmt-wasm.sh
RUN --mount=type=cache,id=sparkles-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sparkles-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sparkles-target-wasm,target=/src/target,sharing=locked \
    CARGO_BUILD_JOBS="${BUILD_JOBS:-$(nproc)}" scripts/build-fmt-wasm.sh /out/wasm

# -------------------------------------------------------------------------------- UI ----

FROM node:${NODE_VERSION}-${DEBIAN_RELEASE}-slim AS ui
ARG PNPM_VERSION
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
RUN corepack enable && corepack prepare "pnpm@${PNPM_VERSION}" --activate
WORKDIR /src/ui
COPY ui/package.json ui/pnpm-lock.yaml ./
RUN --mount=type=cache,id=sparkles-pnpm-store,target=/pnpm/store \
    pnpm install --frozen-lockfile --store-dir /pnpm/store
COPY ui ./
# the formatter in the page; without it the UI would format through POST /$/format
COPY --from=fmt-wasm /out/wasm src/lib/wasm
RUN pnpm build

# ---------------------------------------------------------------------------- server ----

FROM sources AS server
ARG BUILD_JOBS
# The workspace's release profile keeps line tables (`debug = 1`), which make up most of
# the binary's 450 MB. The image drops them and keeps the symbol table, so backtraces
# still name functions; `--build-arg KEEP_DEBUGINFO=1` keeps the line tables too.
ARG KEEP_DEBUGINFO
# the server embeds ui/build at compile time (crates/sparkles-server/src/ui.rs)
COPY --from=ui /src/ui/build ui/build
RUN --mount=type=cache,id=sparkles-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sparkles-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sparkles-target-server,target=/src/target,sharing=locked \
    CARGO_BUILD_JOBS="${BUILD_JOBS:-$(nproc)}" cargo build --release --locked -p sparkles-server \
    && install -D target/release/sparkles /out/sparkles \
    && if [ -z "${KEEP_DEBUGINFO}" ]; then objcopy --strip-debug /out/sparkles; fi

# --------------------------------------------------------------------------- runtime ----

FROM debian:${DEBIAN_RELEASE}-slim AS runtime
# CA certificates for outbound HTTPS: SERVICE, LOAD, OIDC, S3 backup repositories,
# embedding providers and OTLP
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 sparkles \
    && useradd --system --uid 10001 --gid sparkles --create-home --home-dir /home/sparkles \
       --shell /usr/sbin/nologin sparkles \
    && install -d -o sparkles -g sparkles /data
COPY --from=server /out/sparkles /usr/local/bin/sparkles
COPY scripts/docker-healthcheck.sh /usr/local/bin/sparkles-healthcheck
COPY LICENSE THIRD_PARTY_LICENSES.md /usr/share/doc/sparkles/
COPY --from=ui /src/ui/build/licenses.txt /usr/share/doc/sparkles/THIRD_PARTY_LICENSES-UI.md

LABEL org.opencontainers.image.title="Sparkles" \
      org.opencontainers.image.description="RDF database with a Jena/Fuseki-compatible SPARQL server and web UI" \
      org.opencontainers.image.source="https://github.com/kclejeune/sparkles" \
      org.opencontainers.image.licenses="Apache-2.0"

USER sparkles:sparkles
WORKDIR /data
VOLUME /data
EXPOSE 3030
# the server finishes requests in flight for --shutdown-grace (20 s) after SIGTERM; give
# `docker stop` more than that (`--stop-timeout 30`, compose.yaml's stop_grace_period)
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=5s --start-period=5m --start-interval=2s --retries=3 \
    CMD ["sparkles-healthcheck"]
ENTRYPOINT ["sparkles"]
# 0.0.0.0 inside the container, so that a published port reaches the server. Without
# --auth-config the server refuses to start on it unless SPARKLES_ALLOW_OPEN_NETWORK=1 says
# that the operator keeps the port private, as compose.yaml does by publishing it on the
# host's loopback address only (docs/USAGE.md#docker).
CMD ["serve", "--data", "/data", "--host", "0.0.0.0", "--port", "3030"]
