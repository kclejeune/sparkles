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
#   server-ocr    the same build with the `pdf-ocr` feature added
#   ocr-libs      PDFium and ONNX Runtime at pinned versions, checked against their sha256
#   base          Debian slim with CA certificates and a non-root user
#   ocr           base with the server-ocr binary and the OCR libraries (`--target ocr`)
#   runtime       base with the server binary (the default target)
#
# BuildKit cache mounts keep the cargo registry, the pnpm store and the cargo target
# directories between builds, so a rebuild compiles only the crates that changed. Pass
# `--build-arg BUILD_JOBS=N` to cap the parallel compile jobs on a busy machine.
#
# `--build-arg FEATURES="a b"` sets the cargo features of sparkles-server that the server
# builds add to its defaults. It defaults to `embed-local`, the local embedding runtime,
# which adds about 5 MB and runs nothing until a local provider is configured, and
# `--build-arg FEATURES=` leaves it out. `--build-arg NO_DEFAULT_FEATURES=1` starts from
# no default features at all.
# `docker build --target ocr -t sparkles:ocr .` builds the OCR variant. Its server finds
# PDFium and ONNX Runtime through PDFIUM_LIB_PATH and ORT_DYLIB_PATH, and OCR is on when
# `--pdf-ocr-models DIR` names a directory of PP-OCR models, usually a mounted volume.

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
# The OCR variant's libraries. PDFium is the bblanchon/pdfium-binaries release that the
# firecrawl-pdfium crate pins for pdf-inspector, whose pdfium.lock.json lists the same
# checksums. ONNX Runtime is Microsoft's CPU build, and ort 2.0.0-rc.13 needs 1.17 or
# later. Neither project publishes checksums, so these were recorded when the versions
# were pinned, and a download that differs fails the build.
ARG PDFIUM_RELEASE=7988
ARG PDFIUM_SHA256_AMD64=7358c15e26a746cd67854887ea11b3b807c436056788eee9294fb972b8f8e0be
ARG PDFIUM_SHA256_ARM64=a2926203456881efa8feca7e0e409de78a4471b9b72e62a74c820faebb3e4551
ARG ONNXRUNTIME_VERSION=1.30.0
ARG ONNXRUNTIME_SHA256_AMD64=a5ed5a3cac51fbb2e90da632ae43d19212faaa20e76484e62bcb7c23ddb3b3fd
ARG ONNXRUNTIME_SHA256_ARM64=e16a27a8ed330bbc698df7330b0cf56e722f354e3bcc92118682c74ef3c3e3da

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
ARG FEATURES=embed-local
ARG NO_DEFAULT_FEATURES
# the server embeds ui/build at compile time (crates/sparkles-server/src/ui.rs)
COPY --from=ui /src/ui/build ui/build
RUN --mount=type=cache,id=sparkles-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sparkles-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sparkles-target-server,target=/src/target,sharing=locked \
    CARGO_BUILD_JOBS="${BUILD_JOBS:-$(nproc)}" cargo build --release --locked -p sparkles-server \
      ${NO_DEFAULT_FEATURES:+--no-default-features} ${FEATURES:+--features "${FEATURES}"} \
    && install -D target/release/sparkles /out/sparkles \
    && if [ -z "${KEEP_DEBUGINFO}" ]; then objcopy --strip-debug /out/sparkles; fi

# The OCR variant's binary, the same build with `pdf-ocr`. Its target directory is a cache
# of its own, so that the two builds do not throw away each other's compiled crates.
FROM sources AS server-ocr
ARG BUILD_JOBS
ARG KEEP_DEBUGINFO
ARG FEATURES=embed-local
ARG NO_DEFAULT_FEATURES
COPY --from=ui /src/ui/build ui/build
RUN --mount=type=cache,id=sparkles-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sparkles-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sparkles-target-server-ocr,target=/src/target,sharing=locked \
    CARGO_BUILD_JOBS="${BUILD_JOBS:-$(nproc)}" cargo build --release --locked -p sparkles-server \
      ${NO_DEFAULT_FEATURES:+--no-default-features} --features "pdf-ocr ${FEATURES}" \
    && install -D target/release/sparkles /out/sparkles \
    && if [ -z "${KEEP_DEBUGINFO}" ]; then objcopy --strip-debug /out/sparkles; fi

# --------------------------------------------------------------------- OCR libraries ----

FROM debian:${DEBIAN_RELEASE}-slim AS ocr-libs
ARG TARGETARCH
ARG PDFIUM_RELEASE
ARG PDFIUM_SHA256_AMD64
ARG PDFIUM_SHA256_ARM64
ARG ONNXRUNTIME_VERSION
ARG ONNXRUNTIME_SHA256_AMD64
ARG ONNXRUNTIME_SHA256_ARM64
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /tmp/ocr
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) pdfium=linux-x64; ort=x64; \
             pdfium_sum="${PDFIUM_SHA256_AMD64}"; ort_sum="${ONNXRUNTIME_SHA256_AMD64}" ;; \
      arm64) pdfium=linux-arm64; ort=aarch64; \
             pdfium_sum="${PDFIUM_SHA256_ARM64}"; ort_sum="${ONNXRUNTIME_SHA256_ARM64}" ;; \
      *) echo "no OCR libraries for ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o pdfium.tgz \
      "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/${PDFIUM_RELEASE}/pdfium-${pdfium}.tgz"; \
    curl -fsSL -o ort.tgz \
      "https://github.com/microsoft/onnxruntime/releases/download/v${ONNXRUNTIME_VERSION}/onnxruntime-linux-${ort}-${ONNXRUNTIME_VERSION}.tgz"; \
    echo "${pdfium_sum}  pdfium.tgz" | sha256sum -c -; \
    echo "${ort_sum}  ort.tgz" | sha256sum -c -; \
    mkdir -p pdfium ort /out/lib /out/doc/pdfium /out/doc/onnxruntime; \
    tar -xzf pdfium.tgz -C pdfium; \
    tar -xzf ort.tgz -C ort --strip-components=1; \
    install -m 0644 pdfium/lib/libpdfium.so /out/lib/; \
    cp -a ort/lib/libonnxruntime.so* /out/lib/; \
    cp -a pdfium/LICENSE pdfium/VERSION pdfium/licenses /out/doc/pdfium/; \
    cp -a ort/LICENSE ort/ThirdPartyNotices.txt ort/VERSION_NUMBER /out/doc/onnxruntime/

# ---------------------------------------------------------------------------- images ----

FROM debian:${DEBIAN_RELEASE}-slim AS base
# CA certificates for outbound HTTPS: SERVICE, LOAD, OIDC, S3 backup repositories,
# embedding providers and OTLP
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 sparkles \
    && useradd --system --uid 10001 --gid sparkles --create-home --home-dir /home/sparkles \
       --shell /usr/sbin/nologin sparkles \
    && install -d -o sparkles -g sparkles /data
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

# The OCR variant. The libraries' license notices are in /usr/share/doc/pdfium and
# /usr/share/doc/onnxruntime.
FROM base AS ocr
COPY --from=ocr-libs /out/lib/ /usr/local/lib/sparkles/
COPY --from=ocr-libs /out/doc/ /usr/share/doc/
ENV PDFIUM_LIB_PATH=/usr/local/lib/sparkles/libpdfium.so \
    ORT_DYLIB_PATH=/usr/local/lib/sparkles/libonnxruntime.so
COPY --from=server-ocr /out/sparkles /usr/local/bin/sparkles
LABEL org.opencontainers.image.title="Sparkles (OCR)"

# The default image. It is the last stage, so `docker build .` builds it.
FROM base AS runtime
COPY --from=server /out/sparkles /usr/local/bin/sparkles
