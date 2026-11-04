#!/usr/bin/env bash
# Build the server for the release archive in the pinned manylinux_2_28 image and check
# the binary against the archive's glibc baseline, as the "Server artifact candidates"
# workflow does, without the UI and the archive assembly (the nightly gate runs this).
#
# The build image is "builder" in scripts/server-artifact-images.json, the same pin that
# the workflow and scripts/server-artifact-build.sh use. The check is the `audit` command
# of scripts/server-artifact.py, which runs parse_elf, so a symbol that needs a glibc
# newer than 2.28, such as gettid (2.30), fails here as it does on GitHub. A call to a
# newer symbol can fail earlier, at the link, with an undefined symbol.
#
# The committed tree (HEAD) is built from a copy made with git archive, so the checkout
# stays clean. The container is rootless podman, holds the memory limit below, and
# downloads the Rust toolchain, Boxington and the crates on every run. It takes about 12
# minutes on a gate host with the image already pulled.
#
# Usage: scripts/server-artifact-link-check.sh
#   SPARKLES_LINK_CHECK_DIR     scratch directory, default .artifacts/server-link-check
#                               under the repository, removed afterwards
#   SPARKLES_LINK_CHECK_KEEP=1  keep the scratch directory (build tree and target)
#   SPARKLES_LINK_CHECK_MEMORY  memory limit of the container, default the cgroup limit of
#                               this process when it has one and otherwise 12g
#   CARGO_BUILD_JOBS            compile jobs inside the container, default the CPU count
set -euo pipefail

cd "$(dirname "$0")/.."
root=$PWD

fail() {
  echo "server-artifact-link-check: $*" >&2
  exit 1
}

command -v podman > /dev/null || fail "podman is not installed on $(hostname); run this on a gate host that has it"
command -v python3 > /dev/null || fail "python3 is not on PATH"
git rev-parse --verify -q HEAD > /dev/null || fail "run it in a git checkout"

builder=$(python3 -c 'import json; print(json.load(open("scripts/server-artifact-images.json"))["builder"])')
[ -n "$builder" ] || fail "no builder image in scripts/server-artifact-images.json"

# The memory limit: the cgroup cap of this process when it is finite, so that the
# container holds the same budget as the gate unit that runs it.
memory=${SPARKLES_LINK_CHECK_MEMORY:-}
if [ -z "$memory" ]; then
  cg=/sys/fs/cgroup$(cut -d: -f3 < /proc/self/cgroup 2> /dev/null || true)
  cap=$(cat "$cg/memory.max" 2> /dev/null || echo max)
  case $cap in
    '' | max | *[!0-9]*) memory=12g ;;
    *) memory=$((cap / 1048576))m ;;
  esac
fi

work=${SPARKLES_LINK_CHECK_DIR:-$root/.artifacts/server-link-check}
rm -rf "$work"
mkdir -p "$work/src"
cleanup() {
  [ "${SPARKLES_LINK_CHECK_KEEP:-0}" = 1 ] || rm -rf "$work"
}
trap cleanup EXIT
git archive HEAD | tar -x -C "$work/src"

echo "server-artifact-link-check: building $(git rev-parse --short HEAD) in $builder with memory $memory"
podman pull -q "$builder" > /dev/null || fail "could not pull $builder"

# The container script runs in the builder's own Python 3.13, as the workflow does. The
# toolchain comes from rust-toolchain.toml and Boxington from the pin in mise.toml, which
# scripts/install-mbx-container.sh installs with a checked digest.
code=0
podman run --rm -i --security-opt label=disable \
  --memory "$memory" --memory-swap "$memory" --pids-limit 4096 \
  -v "$work/src:/work/src:rw" -w /work/src \
  -e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(nproc)}" -e CARGO_INCREMENTAL=0 \
  -e "SPARKLES_ARTIFACT_BUILDER_IMAGE=$builder" \
  "$builder" bash -s << 'CONTAINER' || code=$?
set -euo pipefail
export PATH=/opt/python/cp313-cp313/bin:$PATH
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path --default-toolchain none > /dev/null
export PATH=$HOME/.cargo/bin:$PATH
# shellcheck source=scripts/install-mbx-container.sh
source scripts/install-mbx-container.sh
export PATH="$PWD/scripts:$PATH" MBX_TARGET_VIEWS=1
echo "link-check: build"
mbx sparkles-task -- mbx build --locked --release -p sparkles-server --bin sparkles --target x86_64-unknown-linux-gnu
echo "link-check: audit"
python3 scripts/server-artifact.py audit --binary target/x86_64-unknown-linux-gnu/release/sparkles
CONTAINER
if [ "$code" != 0 ]; then
  fail "the release build or its glibc 2.28 baseline check failed (exit $code); the release archive would not link or load on glibc 2.28 hosts. See the output above for the undefined symbol or the GLIBC versions"
fi
echo "server-artifact-link-check: ok"
