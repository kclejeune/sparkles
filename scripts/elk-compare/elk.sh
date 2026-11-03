#!/usr/bin/env bash
# Classifies an ontology with a reference reasoner for the comparison with the classifier
# of sparkles-owl in docs/benchmarks/el-classification: ELK or Whelk, two OWL 2 EL
# reasoners, or HermiT or Konclude, two complete OWL 2 DL reasoners.
#
#   scripts/elk-compare/elk.sh run [--reasoner elk|hermit|whelk|konclude] [--threads N]
#     [--repeat N] [--taxonomy FILE] INPUT
#   scripts/elk-compare/elk.sh path elk|hermit|whelk|konclude|robot
#
# The reasoners are the flake's packages of nix/reasoners, built from checksum-pinned
# releases. `path` builds one and prints its executable, and `run` runs it. ELK_CLASSIFY,
# HERMIT_CLASSIFY, WHELK_CLASSIFY, KONCLUDE and ROBOT name an executable built elsewhere in
# place of the flake's, for a host or a systemd unit without Nix on its PATH. A package
# that the flake builds is kept alive by a GC root under ELK_WORK/gcroots.
#
# The Java reasoners run scripts/elk-compare/ElkClassify.java, which prints one JSON line
# and writes the taxonomy that scripts/el-oracle.py reads (see the comment at the top of
# that file). Konclude runs through scripts/elk-compare/konclude.py, which prints a JSON
# line of the same kind and converts Konclude's class hierarchy into that form.
#
# A JVM gets an explicit heap of ELK_HEAP (default 4g) for both -Xms and -Xmx, with the
# G1 collector, and ELK_JAVA_OPTS adds options. Where systemd-run exists, the reasoner runs
# in its own user scope with MemoryMax=ELK_MEM_MAX (default 12G) and no swap, so that an
# out-of-memory kill ends only the reasoner. ELK_SCOPE=0 runs it directly. ELK's warnings
# go to standard error, with the INFO messages that name the axioms it does not support.
#
# Env: ELK_WORK (default .artifacts/elk-compare), ELK_HEAP, ELK_JAVA_OPTS, ELK_MEM_MAX,
# ELK_SCOPE, PYTHON (default python3) and the executables above.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
WORK=${ELK_WORK:-$ROOT/.artifacts/elk-compare}
# shellcheck source=scripts/bench-tools.sh
. "$ROOT/scripts/bench-tools.sh"

# The executable of one reasoner, from its variable or from the flake.
reasoner_bin() {
  case $1 in
    elk) if [ -n "${ELK_CLASSIFY:-}" ]; then echo "$ELK_CLASSIFY"; else nixexe elk elk-classify; fi ;;
    hermit) if [ -n "${HERMIT_CLASSIFY:-}" ]; then echo "$HERMIT_CLASSIFY"; else nixexe hermit hermit-classify; fi ;;
    whelk) if [ -n "${WHELK_CLASSIFY:-}" ]; then echo "$WHELK_CLASSIFY"; else nixexe whelk whelk-classify; fi ;;
    konclude) if [ -n "${KONCLUDE:-}" ]; then echo "$KONCLUDE"; else nixexe konclude Konclude; fi ;;
    robot) if [ -n "${ROBOT:-}" ]; then echo "$ROBOT"; else nixexe robot robot; fi ;;
    *)
      echo "unknown reasoner $1: elk, hermit, whelk, konclude or robot" >&2
      return 2
      ;;
  esac
}

case ${1:-} in
  path)
    if [ $# != 2 ]; then
      sed -n '6,8p' "$0" >&2
      exit 2
    fi
    reasoner_bin "$2"
    ;;
  run)
    shift
    which=elk
    args=()
    while [ $# -gt 0 ]; do
      case $1 in
        --reasoner)
          which=${2:-}
          shift 2 || shift
          ;;
        *)
          args+=("$1")
          shift
          ;;
      esac
    done
    case $which in
      elk | hermit | whelk | konclude) ;;
      *)
        echo "unknown reasoner '$which': elk, hermit, whelk or konclude" >&2
        exit 2
        ;;
    esac
    bin=$(reasoner_bin "$which")
    if [ "$which" = konclude ]; then
      # the version from a store path such as /nix/store/<hash>-konclude-0.7.0-1138/bin
      package=$(basename "$(dirname "$(dirname "$(readlink -f "$bin")")")")
      case $package in
        *-konclude-*) label="Konclude ${package#*-konclude-}" ;;
        *) label=Konclude ;;
      esac
      cmd=("${PYTHON:-python3}" "$HERE/konclude.py" run --konclude "$bin" --label "$label"
        ${args[@]+"${args[@]}"})
    else
      HEAP=${ELK_HEAP:-4g}
      export JAVA_OPTS="-Xms$HEAP -Xmx$HEAP -XX:+UseG1GC ${ELK_JAVA_OPTS:-} \
-Dorg.slf4j.simpleLogger.defaultLogLevel=warn \
-Dorg.slf4j.simpleLogger.log.org.semanticweb.elk.reasoner.completeness=info"
      cmd=("$bin" ${args[@]+"${args[@]}"})
    fi
    if [ "${ELK_SCOPE:-1}" != 0 ] && command -v systemd-run > /dev/null; then
      if ! systemd-run --user --scope --quiet true 2> /dev/null; then
        echo "systemd-run cannot create a user scope here; set ELK_SCOPE=0 to run without one" >&2
        exit 1
      fi
      cmd=(systemd-run --user --scope --quiet -p "MemoryMax=${ELK_MEM_MAX:-12G}"
        -p MemorySwapMax=0 -p OOMPolicy=continue "${cmd[@]}")
    fi
    exec "${cmd[@]}"
    ;;
  *)
    sed -n '2,28p' "$0" >&2
    exit 2
    ;;
esac
