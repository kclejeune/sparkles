#!/usr/bin/env bash
# Run inside the Alpine Node build container after its Bash bootstrap.
set -euo pipefail
apk add --no-cache curl build-base cmake perl git
export CARGO_HOME=/work/.cache/musl/cargo RUSTUP_HOME=/work/.cache/musl/rustup
export PATH="$CARGO_HOME/bin:$PATH"
if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
fi
# shellcheck source=scripts/install-mbx-container.sh
source scripts/install-mbx-container.sh
npm install -g pnpm@10.34.5
pnpm config set store-dir /work/.cache/musl/pnpm
pnpm --dir js install --frozen-lockfile
bash scripts/node-native.sh
pnpm --dir js build
node --test js/common/test/*.test.mjs js/client/test/*.test.mjs js/engine/test/*.test.mjs
node js/scripts/platforms.mjs
node js/scripts/pack-runtime.mjs packed
mkdir installed
cd installed
npm init -y
npm install --offline --ignore-scripts --omit=optional ../packed/*.tgz
cp ../js/scripts/packed-smoke.mjs ../js/scripts/packed-types.mts .
node packed-smoke.mjs
node node_modules/typescript/bin/tsc --noEmit --strict --target es2023 --module nodenext packed-types.mts
