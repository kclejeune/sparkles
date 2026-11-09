#!/usr/bin/env bash
# Check, in minutes, the generated and copied files that merged lanes most often leave out
# of step, before a batch gate spends an hour finding them.
#
# It runs these checks, each with its log in .artifacts/premerge/<check>.log, and prints a
# line for each and the fix for each failure:
#   client-ts     the TypeScript client's OpenAPI types match docs/openapi.json and the
#                 explanation contract type-checks (node:client:check)
#   client-rust   the Rust client's OpenAPI models match docs/openapi.json (client:types:check)
#   lock-py, lock-node, lock-ffi
#                 each binding workspace's Cargo.lock resolves as it is (`mbx metadata
#                 --locked`), which fails when the root Cargo.lock changed without it
#   py-parity     every `planned:` Python entry of crates/sparkles/bindings.toml is in the
#                 DEFERRED set of crates/sparkles-py/tests/test_parity.py, every `skip:` entry
#                 is one the test allows, and no DEFERRED key is stale
#   nix-pins      the Nix packages evaluate against Cargo.lock, which fails when a lock
#                 refresh moves a crate that a Nix file pins, such as wasm-bindgen in
#                 nix/fmt-wasm.nix
#   doc-paths     cited source paths and documentation entry-page links (lint:doc-paths)
#   licenses      the third-party licence files match the Cargo locks (licenses:check)
#   bench-scripts the script unit tests, which include the CI task-profile test, and the
#                 crash harness's tests (test:bench-scripts)
#
# Before the checks, a `target` link that Boxington's collection left dangling is repaired
# by placing a fresh managed target with `mbx sparkles-task`.
#
# Usage: scripts/premerge.sh [--fix] [check...]
#   --fix   regenerate what can be regenerated (client:gen, client:types, py:lock, node:lock,
#           jvm:lock and licenses) before checking, and list the files that changed, to commit
#   check   run only these checks
set -euo pipefail

main() {
  cd "$(dirname "$0")/.."
  all=(client-ts client-rust lock-py lock-node lock-ffi nix-pins py-parity doc-paths licenses bench-scripts)
  fix=
  checks=()
  while [ $# -gt 0 ]; do
    case $1 in
      --fix) fix=1 ;;
      -h | --help)
        sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
        exit 0
        ;;
      -*)
        echo "premerge.sh: unknown option $1" >&2
        exit 2
        ;;
      *) checks+=("$1") ;;
    esac
    shift
  done
  [ ${#checks[@]} -gt 0 ] || checks=("${all[@]}")
  for c in "${checks[@]}"; do
    [[ " ${all[*]} " == *" $c "* ]] || {
      echo "premerge.sh: unknown check $c (one of: ${all[*]})" >&2
      exit 2
    }
  done

  out=.artifacts/premerge
  rm -rf "$out"
  mkdir -p "$out"
  start=$SECONDS
  repair_target || return 1

  if [ -n "$fix" ]; then
    before=$(git status --porcelain)
    for t in node:client:gen client:types py:lock node:lock jvm:lock licenses; do
      echo "fix: mise run $t"
      mise run "$t" > "$out/fix-${t//:/_}.log" 2>&1 || echo "fix: $t failed, see $out/fix-${t//:/_}.log"
    done
    after=$(git status --porcelain)
    if [ "$before" != "$after" ]; then
      echo "fix: these files changed and need a commit:"
      # diff exits 1 when the lists differ, which is the expected case here
      { diff <(echo "$before") <(echo "$after") || [ $? -eq 1 ]; } | sed -n 's/^> /  /p'
    else
      echo "fix: nothing changed"
    fi
  fi

  failed=()
  for c in "${checks[@]}"; do
    t0=$SECONDS
    if "check_${c//-/_}" > "$out/$c.log" 2>&1; then
      printf 'PASS %-14s %4ds\n' "$c" "$((SECONDS - t0))"
    else
      printf 'FAIL %-14s %4ds  log: %s\n' "$c" "$((SECONDS - t0))" "$out/$c.log"
      failed+=("$c")
    fi
  done
  if [ ${#failed[@]} -eq 0 ]; then
    echo "premerge passed in $((SECONDS - start))s"
    return 0
  fi
  for c in "${failed[@]}"; do
    printf '\n==== %s: %s\n' "$c" "$(hint "$c")"
    tail -n 20 "$out/$c.log"
  done
  printf '\npremerge failed: %s\n' "${failed[*]}"
  return 1
}

# Boxington collects a whole managed target whose lease no command holds, and it can leave
# `target` as a link to a directory that no longer exists. A step that writes under it then
# fails with "File exists" or "Not a directory". `mbx sparkles-task` places a fresh target
# before it runs its command, so an empty command under it repairs the link.
repair_target() {
  [ -L target ] && [ ! -e target ] || return 0
  echo "premerge: target links to a collected directory, placing a fresh one with mbx sparkles-task"
  if PATH="$PWD/scripts:$PATH" mbx sparkles-task -- true > "$out/repair-target.log" 2>&1 && [ -e target ]; then
    return 0
  fi
  echo "premerge: could not repair the dangling target link, see $out/repair-target.log" >&2
  return 1
}

hint() {
  case $1 in
    client-ts) echo "run 'mise run node:client:gen' (or premerge.sh --fix) and commit js/client/src/openapi.d.ts, or fix js/scripts/explanation-contract.ts" ;;
    client-rust) echo "run 'mise run client:types' (or premerge.sh --fix) and commit crates/sparkles-client/src/types/generated.rs" ;;
    lock-py) echo "run 'mise run py:lock' (or premerge.sh --fix) and commit crates/sparkles-py/Cargo.lock" ;;
    lock-node) echo "run 'mise run node:lock' (or premerge.sh --fix) and commit crates/sparkles-node/Cargo.lock" ;;
    lock-ffi) echo "run 'mise run jvm:lock' (or premerge.sh --fix) and commit crates/sparkles-ffi/Cargo.lock" ;;
    py-parity) echo "add the Python binding, or the key to DEFERRED in crates/sparkles-py/tests/test_parity.py with the reason in bindings.toml, or drop a stale DEFERRED key" ;;
    nix-pins) echo "update the version and hashes in the Nix file that the log names to match Cargo.lock" ;;
    doc-paths) echo "fix the cited paths or links that the log names" ;;
    licenses) echo "run 'mise run licenses' (or premerge.sh --fix) and commit the THIRD_PARTY_LICENSES.md files it changes" ;;
    bench-scripts) echo "fix the failing script test that the log names (scripts/bench-tests)" ;;
  esac
}

# Evaluating a package checks the pins its Nix file asserts against Cargo.lock without
# building anything. A host without Nix skips the check.
check_nix_pins() {
  if ! command -v nix > /dev/null; then
    echo "nix not found, skipping"
    return 0
  fi
  local sys
  sys=$(nix eval --raw --impure --expr builtins.currentSystem) || return 1
  nix eval --raw ".#packages.$sys.sparkles-fmt-wasm.drvPath"
}

check_client_ts() { mise run node:client:check; }
check_client_rust() { mise run client:types:check; }

# The lock of a binding workspace, resolved offline first and online when a crate it names
# is not downloaded yet.
binding_lock() {
  local m=crates/$1/Cargo.toml
  if mbx metadata --locked --offline --format-version 1 --manifest-path "$m" > /dev/null; then return 0; fi
  echo "retrying online"
  mbx metadata --locked --format-version 1 --manifest-path "$m" > /dev/null
}
check_lock_py() { binding_lock sparkles-py; }
check_lock_node() { binding_lock sparkles-node; }
check_lock_ffi() { binding_lock sparkles-ffi; }

check_py_parity() {
  python3 - << 'PY'
import ast, sys, tomllib
from pathlib import Path
test = Path("crates/sparkles-py/tests/test_parity.py")
tree = ast.parse(test.read_text())
deferred = allowed_skips = None
for node in ast.walk(tree):
    if isinstance(node, ast.Assign) and any(getattr(t, "id", None) == "DEFERRED" for t in node.targets):
        deferred = set(ast.literal_eval(node.value))
    if isinstance(node, ast.Compare) and isinstance(node.left, ast.Name) and node.left.id == "key" \
            and isinstance(node.ops[0], ast.In) and isinstance(node.comparators[0], ast.Set):
        allowed_skips = set(ast.literal_eval(node.comparators[0]))
if deferred is None or allowed_skips is None:
    sys.exit(f"cannot find DEFERRED or the allowed skip: keys in {test}")
bindings = tomllib.loads(Path("crates/sparkles/bindings.toml").read_text())
planned = {k for k, e in bindings.items() if e.get("python", "").startswith("planned:")}
skips = {k for k, e in bindings.items() if e.get("python", "").startswith("skip:")}
bad = False
for k in sorted(planned - deferred):
    print(f"bindings.toml plans a Python binding for {k}, which DEFERRED does not list")
    bad = True
for k in sorted(skips - allowed_skips):
    print(f"bindings.toml skips the Python binding of {k}, which the parity test does not allow")
    bad = True
for k in sorted(deferred - planned):
    print(f"DEFERRED lists {k}, which bindings.toml no longer plans")
    bad = True
print(f"{len(planned)} planned, {len(deferred)} deferred, {len(skips)} skipped")
sys.exit(1 if bad else 0)
PY
}

check_doc_paths() { mise run lint:doc-paths; }
check_licenses() { python3 scripts/third-party-licenses.py --check; }
check_bench_scripts() { mise run test:bench-scripts; }

main "$@"
