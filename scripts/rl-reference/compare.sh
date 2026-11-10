#!/usr/bin/env bash
# Compares the OWL 2 RL materialization of `sparkles infer --profile owl-rl` with the
# reference materialization of Nemo (scripts/rl-reference/owl2rl.rls), triple for triple.
#
#   scripts/rl-reference/compare.sh INPUT WORKDIR
#
# INPUT is any RDF file that `sparkles load` reads. Sparkles loads it, materializes it and
# dumps the default graph and the derived graph in one N-Quads dump, so that both share
# its blank node labels. The default graph, as Sparkles stores it, is the reference's
# input, encoded by rl_terms.py so that Nemo's normalization of literals and its renaming
# of blank nodes cannot change a term. Nemo then runs two programs.
#
# * The aligned program, owl2rl.rls with sparkles-extensions.rls, derives the closure that
#   Sparkles' rule set defines. Its derived graph must equal Sparkles' triple for triple,
#   and every difference must match a row of KNOWN (default known.tsv).
# * The core program, owl2rl.rls alone, derives the closure of the W3C rules. Each of its
#   triples that Sparkles lacks must match a row of KNOWN, and Sparkles' additions are
#   only counted.
#
# WORKDIR gets sparkles-derived.nt, reference-derived.nt, core-derived.nt, diff.json and
# core.json (the counts by side, predicate and explanation) and the logs. The exit status
# is 0 when every difference is explained, 1 when one is not and 2 when a step fails.
#
# Env: SPARKLES (default target/release/sparkles), NMO (default the flake's nemo package),
# KNOWN, PYTHON (default python3), ROOT and WORK for the package resolution of
# scripts/bench-tools.sh.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=${ROOT:-$(cd "$HERE/../.." && pwd)}
PYTHON=${PYTHON:-python3}
KNOWN=${KNOWN:-$HERE/known.tsv}
INFERRED=urn:x-sparkles:inferred

[ $# -eq 2 ] || {
  sed -n '2,20p' "$0" >&2
  exit 2
}
input=$(realpath "$1")
work=$(realpath -m "$2")
mkdir -p "$work"
fail() {
  echo "compare.sh: $*" >&2
  exit 2
}

SPARKLES=${SPARKLES:-$ROOT/target/release/sparkles}
[ -x "$SPARKLES" ] || fail "no Sparkles binary at $SPARKLES; build it or set SPARKLES"
if [ -z "${NMO:-}" ]; then
  WORK=${WORK:-$work}
  # shellcheck source=scripts/bench-tools.sh
  . "$ROOT/scripts/bench-tools.sh"
  NMO=$(nixexe nemo nmo) || fail "cannot build the nemo package"
fi

cd "$work"
rm -rf db enc nemo-aligned nemo-core
"$SPARKLES" load --loc db "$input" > load.log 2>&1 || fail "sparkles load failed, see $work/load.log"
"$SPARKLES" infer --loc db --profile owl-rl > infer.log 2>&1 || fail "sparkles infer failed, see $work/infer.log"
"$SPARKLES" dump --loc db --format nq --out all.nq > dump.log 2>&1 || fail "sparkles dump failed, see $work/dump.log"
rm -rf db
"$PYTHON" -I "$HERE/rl_terms.py" split all.nq base.nt sparkles-derived.nt "$INFERRED" 2> split.log ||
  fail "splitting the dump failed, see $work/split.log"
rm -f all.nq
mkdir -p enc
"$PYTHON" -I "$HERE/rl_terms.py" encode base.nt enc 2> encode.log || fail "encoding failed, see $work/encode.log"
# the aligned program leaves out the reflexive cases of scm-eqc2 and scm-eqp2 (owl2rl.rls)
grep -v '% reflexive-scm-eq$' "$HERE/owl2rl.rls" | cat - "$HERE/sparkles-extensions.rls" > aligned.rls
nemo() {
  "$NMO" --overwrite-results --import-dir enc --export-dir "nemo-$1" --report short "$2" > "nemo-$1.log" 2>&1 ||
    fail "nemo failed on the $1 program, see $work/nemo-$1.log"
  "$PYTHON" -I "$HERE/rl_terms.py" decode enc "nemo-$1/derived.nt" "$3" 2> "decode-$1.log" ||
    fail "decoding failed, see $work/decode-$1.log"
  rm -rf "nemo-$1"
}
nemo aligned aligned.rls reference-derived.nt
nemo core "$HERE/owl2rl.rls" core-derived.nt
rc=0
"$PYTHON" -I "$HERE/rl_terms.py" diff sparkles-derived.nt reference-derived.nt diff.json --known "$KNOWN" || rc=1
"$PYTHON" -I "$HERE/rl_terms.py" diff sparkles-derived.nt core-derived.nt core.json --known "$KNOWN" --subset || rc=1
exit "$rc"
