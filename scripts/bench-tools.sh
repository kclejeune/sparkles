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
    apache-jena | apache-jena-fuseki | qlever | oxigraph | fluree | hyperfine)
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

# Auxiliary tools come from PATH when it has them. Engines and the hyperfine timer always
# come from this flake.
nixbin() {
  local package=$1 binary=$2
  case $package in
    apache-jena | apache-jena-fuseki | qlever | oxigraph | fluree | hyperfine) ;;
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

# Put the flake's hyperfine first on PATH, so the runners' bare `hyperfine` calls, including
# those behind a CPU-pinning client prefix, time with the pinned version. HYPERFINE names
# another binary instead.
use_hyperfine() {
  local binary=${HYPERFINE:-}
  if [ -z "$binary" ]; then binary=$(nixbin hyperfine hyperfine) || return; fi
  PATH="$(dirname "$binary"):$PATH"
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
  use_hyperfine
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
