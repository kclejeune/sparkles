#!/usr/bin/env bash
# Build the formatter for the browser (crates/sparkles-fmt-wasm) and its JavaScript
# bindings into OUT_DIR, by default ui/src/lib/wasm (git-ignored), where the UI build
# picks it up and loads it the first time something is formatted. Without it the UI
# formats through POST /$/format, so the module is optional (`mise run ui:wasm`; the Nix
# UI package builds it too).
#
# Needs the wasm32-unknown-unknown target (rust-toolchain.toml lists it) and the
# wasm-bindgen CLI of the version Cargo.lock pins for the wasm-bindgen crate (mise.toml's
# `ui:wasm` task installs it). The build is for size: opt-level "z", one codegen unit,
# fat LTO, no debug info, symbols stripped (a profile given on the command line, so the
# workspace's release profile stays as it is). wasm-opt -Oz is not run: on this module it
# saves about 10% raw but compresses worse (brotli 261 KB against 248 KB).
#
# Usage: scripts/build-fmt-wasm.sh [OUT_DIR]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/ui/src/lib/wasm}
target_dir=${CARGO_TARGET_DIR:-$root/target}

want=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/"/, "", $3); print $3; exit }' "$root/Cargo.lock")
have=$(wasm-bindgen --version 2> /dev/null | awk '{ print $2 }') || true
if [ "$have" != "$want" ]; then
  echo "build-fmt-wasm: Cargo.lock has wasm-bindgen $want but the wasm-bindgen CLI is '${have:-missing}'" >&2
  echo "  (install the same version: the ui:wasm task in mise.toml pins it)" >&2
  exit 1
fi

cargo build --manifest-path "$root/Cargo.toml" -p sparkles-fmt-wasm \
  --target wasm32-unknown-unknown --profile fmt-wasm \
  --config 'profile.fmt-wasm.inherits="release"' \
  --config 'profile.fmt-wasm.opt-level="z"' \
  --config 'profile.fmt-wasm.lto=true' \
  --config 'profile.fmt-wasm.codegen-units=1' \
  --config 'profile.fmt-wasm.debug=false' \
  --config 'profile.fmt-wasm.strip=true' \
  --config 'profile.fmt-wasm.panic="abort"'

# into a fresh directory first, so a failed run leaves the previous module in place
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
wasm-bindgen --target web --no-typescript --out-dir "$tmp" \
  "$target_dir/wasm32-unknown-unknown/fmt-wasm/sparkles_fmt_wasm.wasm"
mkdir -p "$out"
rm -rf "${out:?}"/sparkles_fmt_wasm*
cp -r "$tmp"/. "$out"/
echo "build-fmt-wasm: $(wc -c < "$out/sparkles_fmt_wasm_bg.wasm") bytes in $out"
