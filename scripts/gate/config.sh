#!/usr/bin/env bash
# Shared host-role settings for gate.sh and setup-host.sh, sourced by both.
#
# Settings come from the environment first and then from KEY=VALUE lines in
# ${SPARKLES_GATE_CONFIG:-~/.config/sparkles/gate.env}. SPARKLES_BENCH_HOST (default forge)
# names the benchmark host, which never runs gate work. GATE_HOSTS (default "wally")
# lists the hosts that may run gates, less the benchmark host.

gate_config=${SPARKLES_GATE_CONFIG:-$HOME/.config/sparkles/gate.env}
if [ -f "$gate_config" ]; then
  while IFS='=' read -r k v; do
    case $k in
      SPARKLES_BENCH_HOST | GATE_HOSTS | GATE_PRIMARY | GATE_HELPER | GATE_MIN_FREE_GB | GATE_MIN_FREE_HALF_GB | GATE_NEED_CI_GB | GATE_NEED_CONF_GB | GATE_BASE | GATE_CARGO_JOBS | GATE_CARGO_JOBS_HOSTS | GATE_NIGHTLY_AT | GATE_LINK_HOST)
        [ -n "${!k+x}" ] || export "$k=$v"
        ;;
    esac
  done < <(grep -E '^[A-Z_]+=' "$gate_config")
fi
bench=${SPARKLES_BENCH_HOST:-forge}

# 0 when the host name, with or without a domain or user, is the benchmark host.
is_bench() {
  local h=${1#*@}
  [ "${h%%.*}" = "${bench%%.*}" ]
}

gate_hosts=()
for _h in ${GATE_HOSTS:-wally}; do is_bench "$_h" || gate_hosts+=("$_h"); done
unset _h
