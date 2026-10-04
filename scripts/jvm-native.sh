#!/usr/bin/env bash
# Build the JVM bindings' native library (crates/sparkles-ffi) with cargo and generate its
# Kotlin bindings with the crate's own uniffi-bindgen (`mise run jvm:native`). The Gradle
# build in jvm/ reads both: the library from target/<profile>/, the bindings from
# target/jvm/uniffi. It prints the Gradle properties that name them.
#
# The build shares the root target directory, so it reuses the workspace's compiled
# dependencies. JVM_PROFILE=dev builds the library without optimizations.
#
# Usage: scripts/jvm-native.sh [--print-properties]
set -euo pipefail

cd "$(dirname "$0")/.."

crate=crates/sparkles-ffi
profile="${JVM_PROFILE:-release}"
dir=debug
[ "$profile" = dev ] || dir="$profile"

# the generator runs once per build and needs no optimizations; it is built in the dev
# profile so that the library's own build keeps its features
cargo build --locked --manifest-path "$crate/Cargo.toml" --target-dir target \
  --features bindgen --bin uniffi-bindgen
cargo build --locked --manifest-path "$crate/Cargo.toml" --target-dir target \
  --profile "$profile" --lib

lib="target/$dir/libsparkles_ffi.so"
[ -e "$lib" ] || lib="target/$dir/libsparkles_ffi.dylib"
[ -e "$lib" ] || lib="target/$dir/sparkles_ffi.dll"

out=target/jvm/uniffi
rm -rf "$out"
target/debug/uniffi-bindgen generate --library "$lib" --language kotlin --no-format \
  --config "$crate/bindgen.toml" --out-dir "$out" > /dev/null

if [ "${1:-}" = --print-properties ]; then
  echo "-Psparkles.nativeLib=$PWD/$lib -Psparkles.bindings=$PWD/$out"
fi
