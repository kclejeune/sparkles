#!/usr/bin/env bash
# Build an exact revision on an explicit SSH/Boat host, retaining artifact provenance.
# Usage: scripts/build-remote.sh --host HOST --out DIR REV [native release cargo flags...]
set -euo pipefail
# shellcheck source=scripts/build-host.sh
. "$(cd "$(dirname "$0")" && pwd)/build-host.sh"
die() {
  echo "build-remote: $*" >&2
  exit 2
}
out=
while [ $# -gt 0 ]; do
  case $1 in
    --host)
      BH_ARG=$2
      shift 2
      ;;
    --out)
      out=$2
      shift 2
      ;;
    -h | --help)
      sed -n '2,3s/^# \{0,1\}//p' "$0"
      exit 0
      ;;
    --*) die "unknown option $1" ;;
    *) break ;;
  esac
done
[ $# -gt 0 ] && [ -n "$out" ] && [ -n "$BH_ARG" ] || die 'need --host HOST --out DIR REV [cargo flags...]'
sha=$(git -C "$BH_ROOT" rev-parse --verify "$1^{commit}")
shift
out=$(realpath -m "$out")
[ ! -e "$out" ] || die 'output directory must be new'
bh_check_arg
bh_resolve
[ "$BH_HOST" != local ] || die 'use mbx build for a local build, or name a remote build host'
mkdir -p "$out"
printf '%s\n' "$sha" > "$out/source.commit"
trap bh_cleanup EXIT
trap 'exit 130' INT TERM
bh_build "$BH_HOST" "$sha" "$out" "$@" > "$out/build.log" 2>&1
printf 'build host: %s\nsource: %s\nartifact: %s/sparkles\n' "$BH_HOST" "$sha" "$out"
