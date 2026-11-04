#!/usr/bin/env bash
# Source this inside the Linux wheel/addon build container. Keep the compiler
# shims private to the container and the action store mounted from its host job.
mbx_container_install() {
  local mbx_root mbx_version mbx_arch mbx_digest mbx_tmp mbx_install
  mbx_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
  mbx_version=$(sed -n 's/^mr-boxington = "\([^"]*\)"$/\1/p' "$mbx_root/mise.toml")
  if [ "$mbx_version" != 1.29.1 ]; then
    echo 'Update the verified container checksums with the Boxington version pin.' >&2
    return 1
  fi
  case $(uname -m) in
    x86_64)
      mbx_arch=x86_64
      mbx_digest=3d2a6c9c01c182d135290d7cef261990c5ab1650bdd69a8da9643ef651283af2
      ;;
    aarch64 | arm64)
      mbx_arch=aarch64
      mbx_digest=79fb378e26ee06ae3d23b18406d0eff1db196552f65de9af98bc64cf8902f4a8
      ;;
    *)
      echo 'Unsupported Boxington container architecture.' >&2
      return 1
      ;;
  esac
  mbx_install=${MBX_INSTALL_DIR:-/tmp/sparkles-mbx}
  mkdir -p "$mbx_install/bin" || return 1
  mbx_tmp=$(mktemp -d) || return 1
  curl --fail --location --retry 3 --silent --show-error \
    "https://github.com/jdx/mr-boxington/releases/download/v$mbx_version/mbx-$mbx_arch-unknown-linux-musl.tar.gz" \
    --output "$mbx_tmp/mbx.tar.gz" || return 1
  printf '%s  %s\n' "$mbx_digest" "$mbx_tmp/mbx.tar.gz" | sha256sum -c || return 1
  tar -xzf "$mbx_tmp/mbx.tar.gz" -C "$mbx_tmp" || return 1
  install -m 755 "$(find "$mbx_tmp" -type f -name mbx -print -quit)" "$mbx_install/bin/mbx" || return 1
  rm -rf "$mbx_tmp"
  export XDG_DATA_HOME="$mbx_install/data"
  export PATH="$mbx_install/bin:$PATH"
  mbx setup --yes || return 1
  export PATH="$XDG_DATA_HOME/mbx/bin:$PATH"
  mbx --version || return 1
  cargo --version || return 1
}
if mbx_container_install; then
  unset -f mbx_container_install
else
  unset -f mbx_container_install
  if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    exit 1
  else
    return 1
  fi
fi
