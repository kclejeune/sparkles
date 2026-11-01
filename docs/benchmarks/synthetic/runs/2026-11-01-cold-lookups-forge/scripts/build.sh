#!/usr/bin/env bash
# usage: build.sh <worktree-name>   builds release sparkles in ~/lanes/cold-lookups-4/<name> under the heavy lock
set -euo pipefail
name=$1
cd ~/lanes/cold-lookups-4/$name
exec 9> "$HOME/lanes/.heavy.lock"
flock 9
nix develop -c mbx build --release -p sparkles-server --bin sparkles > ../build-$name.log 2>&1
cp target/release/sparkles ../sparkles-$name
readelf -p .comment ../sparkles-$name > ../comment-$name.txt
git rev-parse HEAD > ../rev-$name.txt
echo done >> ../build-$name.log
