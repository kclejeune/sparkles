#!/usr/bin/env bash
# Driver on forge for item 440: build four revisions, then run an interleaved A/B.
set -euo pipefail
W=$HOME/lanes/forge-checks-440-432-20261031
exec 8> "$HOME/lanes/.bench.lock"; flock 8
exec 9> "$HOME/lanes/.heavy.lock"; flock 9
echo "locks held $(date -u +%FT%TZ)"
cd "$W"
mkdir -p bin logs
for c in ${BUILD_REVS:-34b69139 1acdc38a 65e369db 926eb569}; do
  [ -x bin/$c ] && continue
  echo "build $c $(date -u +%T)"
  (cd src/$c && env -u CARGO_TARGET_DIR mbx build --release --locked -p sparkles-server --bin sparkles) > logs/build-$c.log 2>&1
  cp src/$c/target/release/sparkles bin/$c
  sha256sum bin/$c >> bin/SHA256SUMS
  readelf -p .comment bin/$c | sed -n 's/^ *\[ *[0-9a-f]*\] *//p' | sort -u > bin/$c.comment
done
echo "builds done $(date -u +%T)"
