# shellcheck shell=bash
# Package resolution shared by the benchmark runners. The caller sets ROOT and WORK.

# Keep every resolved package alive for the whole run, including during store GC.
# Engine packages always come from this flake, even when another version is on PATH.
# The build prints one store path per installed output, so a package such as zstd prints
# its bin and man outputs on two lines. Nix roots the out output as gcroots/<package> and
# every other output as gcroots/<package>-<output>. Use nixexe to find an executable.
nixpkg() {
  local package=$1 installable
  case $package in
    apache-jena | apache-jena-fuseki | qlever | oxigraph | fluree)
      installable="$ROOT#$package"
      ;;
    *) installable="nixpkgs#$package" ;;
  esac
  mkdir -p "$WORK/gcroots" || return
  # Store paths are safe to embed in the runners' serialized hyperfine commands,
  # including when the work directory and its GC roots contain spaces.
  nix build --no-update-lock-file --inputs-from "$ROOT" "$installable" \
    --out-link "$WORK/gcroots/$package" --print-out-paths || return
}

# Print bin/<binary> from whichever installed output of the package holds it. The
# executable output need not be the first one that the build prints.
nixexe() {
  local package=$1 binary=$2 paths path
  paths=$(nixpkg "$package") || return
  while IFS= read -r path; do
    if [ -f "$path/bin/$binary" ] && [ -x "$path/bin/$binary" ]; then
      echo "$path/bin/$binary"
      return
    fi
  done <<< "$paths"
  echo "$package does not provide bin/$binary" >&2
  return 1
}

# Auxiliary tools come from PATH when it has them, and engines always come from this flake.
nixbin() {
  local package=$1 binary=$2
  case $package in
    apache-jena | apache-jena-fuseki | qlever | oxigraph | fluree) ;;
    *)
      if command -v "$binary" > /dev/null; then
        command -v "$binary"
        return
      fi
      ;;
  esac
  nixexe "$package" "$binary"
}

fluree_bin() {
  if [ -n "${FLUREE_VERSION:-}" ]; then
    local version
    version=$(nix eval --no-update-lock-file --raw "$ROOT#fluree.version") || return
    [ "$FLUREE_VERSION" = "$version" ] || {
      echo "FLUREE_VERSION=$FLUREE_VERSION differs from the flake's $version; set FLUREE to override the binary" >&2
      return 1
    }
  fi
  nixbin fluree fluree
}

# Fix the hyperfine timer for the whole run and put it first on PATH, so the runners' bare
# `hyperfine` calls, including those behind a CPU-pinning client prefix, all use it. It is
# the hyperfine on PATH, which the mise pin supplies. The locked nixpkgs packages an older
# hyperfine, so there is no fallback to it. HYPERFINE names the absolute path of another
# executable, which runs whatever its file name. The runners read the JSON of hyperfine 1
# and 2 (scripts/bench_hyperfine.py). Runners that time requests call this after
# resolve_tools.
use_hyperfine() {
  local binary=${HYPERFINE:-}
  if [ -z "$binary" ]; then
    binary=$(type -P hyperfine) || {
      echo "hyperfine is not on PATH; run mise install, or set HYPERFINE" >&2
      return 1
    }
    # A second call finds the first call's link on PATH and must not link it to itself.
    binary=$(readlink -f "$binary") || return
  elif [[ $binary != /* ]] || [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
    echo "HYPERFINE=$binary must be the absolute path of an executable file" >&2
    return 1
  fi
  mkdir -p "$WORK/timer" || return
  ln -sfn "$binary" "$WORK/timer/hyperfine" || return
  PATH="$WORK/timer:$PATH"
}

resolve_tools() {
  if has jena; then
    TDBLOADER=${TDBLOADER:-$(nixbin apache-jena tdb2.tdbloader)} || return
    FUSEKI=${FUSEKI:-$(nixbin apache-jena-fuseki fuseki-server)} || return
  fi
  if has qlever; then
    QINDEX=${QINDEX:-$(nixbin qlever qlever-index)} || return
    QSERVER=${QSERVER:-$(nixbin qlever qlever-server)} || return
  fi
  if has oxigraph; then OXIGRAPH=${OXIGRAPH:-$(nixbin oxigraph oxigraph)} || return; fi
  if has fluree; then FLUREE=${FLUREE:-$(fluree_bin)} || return; fi
}

# GNU time, rather than the shell keyword. It cannot go through nixbin, because command -v
# finds the keyword. The optional argument replaces the system location /usr/bin/time, which
# lets the tests reach the nixpkgs fallback.
gnu_time() {
  local system=${1:-/usr/bin/time}
  type -P gtime && return
  [ -x "$system" ] && echo "$system" && return
  nixexe time time
}
