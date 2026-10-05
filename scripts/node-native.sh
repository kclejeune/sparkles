#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
profile=${NODE_PROFILE:-release}
args=(--locked --manifest-path crates/sparkles-node/Cargo.toml --target-dir target)
if [[ $profile == release ]]; then
  args+=(--release)
  artifact_dir=release
else artifact_dir=debug; fi
cargo build "${args[@]}"
case $(uname -s) in
  Linux)
    platform=linux
    library=libsparkles_node.so
    suffix=-gnu
    if [[ $(node -p "process.report.getReport().header.glibcVersionRuntime ? 'gnu' : 'musl'") == musl ]]; then suffix=-musl; fi
    ;;
  Darwin)
    platform=darwin
    library=libsparkles_node.dylib
    suffix=
    ;;
  *)
    echo 'Use napi build on this platform' >&2
    exit 1
    ;;
esac
case $(uname -m) in
  x86_64) arch=x64 ;;
  aarch64 | arm64) arch=arm64 ;;
  *)
    echo 'Unsupported architecture' >&2
    exit 1
    ;;
esac
mkdir -p js/engine/native
cp "target/$artifact_dir/$library" "js/engine/native/sparkles.$platform-$arch$suffix.node"
