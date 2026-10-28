#!/usr/bin/env bash
# Run only inside the pinned manylinux job container. Never mounts host targets.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ ${AUDITWHEEL_POLICY:-} != manylinux_2_28 || $(uname -m) != x86_64 ]]; then
  echo 'server-artifact: use the pinned x86_64 manylinux 2.28 job container' >&2
  exit 2
fi
if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
  echo 'server-artifact: CARGO_TARGET_DIR opts out of managed targets' >&2
  exit 2
fi
expected_builder=$(python3 -c 'import json; print(json.load(open("scripts/server-artifact-images.json"))["builder"])')
if [[ ${SPARKLES_ARTIFACT_BUILDER_IMAGE:-} != "$expected_builder" ]]; then
  echo 'server-artifact: missing or incorrect builder digest' >&2
  exit 2
fi
if [[ $(mbx --version) != 'mbx 1.28.0' ]]; then
  echo 'server-artifact: install the pinned Boxington 1.28.0' >&2
  exit 2
fi
# Set up the Cargo drop-in as well as direct mbx calls, for child build tools.
mbx setup --yes
mbx_data_dir=${XDG_DATA_HOME:-${HOME}/.local/share}
export PATH="$mbx_data_dir/mbx/bin:$PWD/scripts:$PATH"
if [[ $(command -v cargo) != "$mbx_data_dir/mbx/bin/cargo" ]]; then
  echo 'server-artifact: child Cargo calls do not resolve to the Boxington shim' >&2
  exit 2
fi
cargo --version
export MBX_TARGET_VIEWS=1

out=${1:-.artifacts/server-artifacts/$(git rev-parse HEAD)/x86_64-unknown-linux-gnu}
if [[ -e $out ]]; then
  echo 'server-artifact: output already exists; use a fresh evidence directory' >&2
  exit 2
fi
# Assignment preserves Git's exit status under errexit. A command substitution
# inside the condition would turn a Git failure into an apparently clean tree.
source_status=$(git status --porcelain --untracked-files=normal)
if [[ -n $source_status ]]; then
  echo 'server-artifact: source must be committed and clean' >&2
  exit 2
fi

# All compiler consumers stay under one managed-target lease. The UI and native
# builds run sequentially in the same container/workspace; there is no host target.
export SPARKLES_ARTIFACT_OUT="$out"
mbx sparkles-task -- bash -euo pipefail << 'BUILD'
mise run ui:wasm
mise run ui:build
mbx build --locked --release -p sparkles-server --bin sparkles --target x86_64-unknown-linux-gnu
python3 scripts/server-artifact.py pack \
  --binary target/x86_64-unknown-linux-gnu/release/sparkles \
  --builder "$SPARKLES_ARTIFACT_BUILDER_IMAGE" --out "$SPARKLES_ARTIFACT_OUT"
BUILD
