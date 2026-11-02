#!/usr/bin/env bash
# Classifies an ontology with ELK, the OWL 2 EL reasoner, for the comparison with the
# classifier of sparkles-owl in docs/benchmarks/el-classification.
#
#   scripts/elk-compare/elk.sh fetch
#   scripts/elk-compare/elk.sh run [--threads N] [--repeat N] [--taxonomy FILE] INPUT
#
# `fetch` downloads the jars that scripts/elk-compare/pins.tsv lists from Maven Central into
# ELK_WORK/lib, checks each against its pinned SHA-256 and Maven Central's SHA-1, and
# compiles ElkClassify.java into ELK_WORK/classes. A jar already present with the pinned
# hash is not downloaded again. `run` fetches when needed and runs ElkClassify, which prints
# one JSON line and writes the taxonomy that scripts/el-oracle.py reads (see the comment at
# the top of ElkClassify.java).
#
# The JVM gets an explicit heap of ELK_HEAP (default 4g) for both -Xms and -Xmx, with the
# G1 collector, and ELK_JAVA_OPTS adds options. Where systemd-run exists, the JVM runs in
# its own user scope with MemoryMax=ELK_MEM_MAX (default 12G) and no swap, so that an
# out-of-memory kill ends only the JVM. ELK_SCOPE=0 runs it directly. ELK's warnings go to
# standard error, with the INFO messages that name the axioms it does not support.
#
# Env: JAVA (default java on PATH, version 11 or later), ELK_WORK (default
# .artifacts/elk-compare), ELK_HEAP, ELK_JAVA_OPTS, ELK_MEM_MAX, ELK_SCOPE.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
WORK=${ELK_WORK:-$ROOT/.artifacts/elk-compare}
JAVA=${JAVA:-java}
JAVAC=${JAVAC:-$(dirname "$(command -v "$JAVA")")/javac}
CENTRAL=https://repo1.maven.org/maven2

# The pinned jars only, so that a stray jar in ELK_WORK/lib is never read.
jars() {
  local path out=
  while IFS=$'\t' read -r path _; do
    case $path in '#'* | '') continue ;; esac
    out=$out${out:+:}$WORK/lib/$(basename "$path")
  done < "$HERE/pins.tsv"
  echo "$out"
}

fetch() {
  mkdir -p "$WORK/lib" "$WORK/classes"
  local path sha jar central
  while IFS=$'\t' read -r path sha; do
    case $path in '#'* | '') continue ;; esac
    jar=$WORK/lib/$(basename "$path")
    if [ -f "$jar" ] && echo "$sha  $jar" | sha256sum -c --quiet 2> /dev/null; then
      continue
    fi
    curl -fsSL --retry 3 -o "$jar.part" "$CENTRAL/$path"
    echo "$sha  $jar.part" | sha256sum -c --quiet
    central=$(curl -fsSL --retry 3 "$CENTRAL/$path.sha1" | cut -c1-40)
    if [ "$central" != "$(sha1sum "$jar.part" | cut -d' ' -f1)" ]; then
      echo "$path: SHA-1 differs from Maven Central's $central" >&2
      exit 1
    fi
    mv "$jar.part" "$jar"
  done < "$HERE/pins.tsv"
  if [ ! -f "$WORK/classes/ElkClassify.class" ] ||
    [ "$HERE/ElkClassify.java" -nt "$WORK/classes/ElkClassify.class" ]; then
    "$JAVAC" -proc:none -nowarn -cp "$(jars)" -d "$WORK/classes" "$HERE/ElkClassify.java"
  fi
}

case ${1:-} in
  fetch)
    fetch
    ;;
  run)
    shift
    fetch
    HEAP=${ELK_HEAP:-4g}
    # shellcheck disable=SC2206
    cmd=("$JAVA" "-Xms$HEAP" "-Xmx$HEAP" -XX:+UseG1GC ${ELK_JAVA_OPTS:-}
      -Dorg.slf4j.simpleLogger.defaultLogLevel=warn
      -Dorg.slf4j.simpleLogger.log.org.semanticweb.elk.reasoner.completeness=info
      -cp "$WORK/classes:$(jars)" ElkClassify "$@")
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
    sed -n '2,22p' "$0" >&2
    exit 2
    ;;
esac
