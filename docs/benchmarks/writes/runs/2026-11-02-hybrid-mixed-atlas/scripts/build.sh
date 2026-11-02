#!/usr/bin/env bash
# build.sh <bundle> <outname>: fetch the lane branch from a bundle, release-build the server under the heavy lock
set -uo pipefail
L=~/lanes/hybrid-583
cd $L/wt
git fetch -q "$L/$1" lane/hybrid-mixed-583 && git checkout -q --detach FETCH_HEAD || exit 1
exec 9> ~/lanes/.heavy.lock; flock 9
echo "lock held $(date -u +%FT%TZ) at $(git rev-parse --short HEAD)"
systemd-run --user --scope -q -p MemoryMax=13G -p MemorySwapMax=2G -p OOMPolicy=continue -- \
  nix develop -c bash -c 'mise trust -q . ; mbx build --release -p sparkles-server --bin sparkles' > $L/build-$2.log 2>&1 < /dev/null
rc=$?
echo "build-exit=$rc"
[ $rc = 0 ] && cp target/release/sparkles $L/bin/$2 && echo "copied $2 $(git rev-parse --short HEAD) $(sha256sum $L/bin/$2 | cut -c1-16)"
echo BUILDDONE
