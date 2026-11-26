#!/usr/bin/env bash
# Helpers for scripts that launch a Sparkles server on IPv4 loopback.

sparkles_listener() {
  local line
  local record='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z +INFO sparkles: Sparkles [^[:space:]]+ listening on http://127\.0\.0\.1:([1-9][0-9]{0,4})/ \(UI at /ui/\), opening [0-9]+ datasets$'
  while IFS= read -r line; do
    if [[ $line =~ $record ]] && ((BASH_REMATCH[1] <= 65535)); then
      printf 'http://127.0.0.1:%s\n' "${BASH_REMATCH[1]}"
      return
    fi
  done < "$1"
}

sparkles_wait_ready() {
  local pid=$1 log=$2 attempts=$3 url='' i
  for ((i = 0; i < attempts; i++)); do
    if ! kill -0 "$pid" 2> /dev/null; then
      echo "sparkles serve exited; see $log" >&2
      return 1
    fi
    if [[ -z "$url" ]]; then
      url=$(sparkles_listener "$log")
    fi
    if [[ -n "$url" ]] && curl --noproxy '*' -fsS --max-time 1 "$url/\$/ready" > /dev/null 2>&1 &&
      kill -0 "$pid" 2> /dev/null; then
      printf '%s\n' "$url"
      return
    fi
    sleep 0.1
  done
  echo "sparkles serve did not become ready; see $log" >&2
  return 1
}
