# shellcheck shell=bash
# Package resolution shared by the benchmark runners. The caller sets ROOT and WORK.

# Keep every resolved package alive for the whole run, including during store GC.
# Engine packages always come from this flake, even when another version is on PATH.
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

nixbin() {
  local package=$1 binary=$2 prefix
  case $package in
    apache-jena | apache-jena-fuseki | qlever | oxigraph | fluree) ;;
    *)
      if command -v "$binary" > /dev/null; then
        command -v "$binary"
        return
      fi
      ;;
  esac
  prefix=$(nixpkg "$package") || return
  [ -x "$prefix/bin/$binary" ] || {
    echo "$package does not provide bin/$binary" >&2
    return 1
  }
  echo "$prefix/bin/$binary"
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

# GNU time, rather than the shell keyword.
gnu_time() {
  type -P gtime && return
  [ -x /usr/bin/time ] && echo /usr/bin/time && return
  local prefix
  prefix=$(nixpkg time) || return
  echo "$prefix/bin/time"
}
