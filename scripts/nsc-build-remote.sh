#!/usr/bin/env bash
# Builds one benchmark revision on a build host for scripts/nsc-build-host.sh, which copies
# this file, scripts/nsc-build.sh and scripts/cargo-sparkles-task into DIR/bin and the
# revision's `git archive` into DIR/src before it calls it over ssh.
#
#   nsc-build-remote.sh DIR UNIT [feature and dependency-resolution flags...]
#
# The build runs scripts/nsc-build.sh in the transient systemd user scope UNIT, so that it can
# be listed and stopped by name and an out-of-memory kill ends the build alone. The scope has
# the gate's memory cap, the smaller of 24G and the host's RAM less 2G, with 2G of swap
# (SPARKLES_NSC_BUILD_MEMORY_MAX overrides it). Inside it the build holds
# ~/lanes/.heavy.lock, which gates and other heavy work on the host share, and waits for it
# when another process holds it. CARGO_BUILD_JOBS defaults to the gate's value for the cap.
# The host's own Boxington cache serves the build.
#
# The binary goes to DIR/out/sparkles with its SHA-256 in DIR/out/sha256. DIR/out/info.txt
# gets the host's cc, c++ and rustc versions in the form of build.txt, DIR/out/rustc.txt and
# cargo.txt the verbose versions, and DIR/out/env.txt the build variables that were set.
# The caller removes DIR.
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo 'usage: nsc-build-remote.sh DIR UNIT [cargo flags...]' >&2
  exit 2
fi
dir=$1
unit=$2
shift 2
fail() {
  echo "nsc-build-remote ($(uname -n)): $*" >&2
  exit 3
}
[ -d "$dir/src" ] && [ -d "$dir/bin" ] || fail "$dir has no src and bin directories"
cd "$dir/src"
# The copied scripts come first, so the build uses the caller's nsc-build.sh and finds the
# cargo subcommand behind `mbx sparkles-task`.
export PATH="$dir/bin:$PATH"
for t in mbx rustc cargo flock systemd-run sha256sum; do command -v "$t" > /dev/null || fail "$t is not on PATH"; done
have=$(mbx --version | awk '{print $2}')
systemd-run --user --scope -q --collect true 2> /dev/null || fail "systemd-run --user --scope does not work"

if [ -n "${SPARKLES_NSC_BUILD_MEMORY_MAX:-}" ]; then
  mem=$SPARKLES_NSC_BUILD_MEMORY_MAX
  mem_g=${mem%G}
  case $mem_g in '' | *[!0-9]*) fail "SPARKLES_NSC_BUILD_MEMORY_MAX is $mem, not a whole number of G" ;; esac
else
  mem_g=$(awk '$1 == "MemTotal:" { m = int($2 / 1048576) - 2; if (m > 24) m = 24; print m }' /proc/meminfo)
  mem=${mem_g}G
fi
jobs=${CARGO_BUILD_JOBS:-$(((mem_g - 1) / 2))}
[ "$jobs" -ge 2 ] || jobs=2
[ "$jobs" -le 8 ] || jobs=8

lock=$HOME/lanes/.heavy.lock
mkdir -p "$HOME/lanes" "$dir/out"
if ! flock -n "$lock" true; then
  echo "waiting for $lock on $(uname -n), which another process holds"
fi
echo "building in scope $unit on $(uname -n): MemoryMax=$mem, CARGO_BUILD_JOBS=$jobs, mbx $have"
systemd-run --user --scope -q --collect --unit "$unit" \
  -p MemoryMax="$mem" -p MemorySwapMax=2G -p OOMPolicy=continue -- \
  env MBX_REMOTE_TOKEN_FILE="${MBX_REMOTE_TOKEN_FILE:-}" CARGO_BUILD_JOBS="$jobs" flock "$lock" bash "$dir/bin/nsc-build.sh" "$dir/out/sparkles" "$@"

{
  echo "cc: $(cc --version 2> /dev/null | head -1)"
  echo "c++: $(c++ --version 2> /dev/null | head -1)"
  rustc -V
} > "$dir/out/info.txt"
rustc -vV > "$dir/out/rustc.txt"
cargo -V > "$dir/out/cargo.txt"
for n in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTUP_TOOLCHAIN CARGO_BUILD_TARGET RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER; do
  [ -z "${!n+x}" ] || printf '%s=%s\n' "$n" "${!n}"
done > "$dir/out/env.txt"
sha256sum "$dir/out/sparkles" | cut -d' ' -f1 > "$dir/out/sha256"
