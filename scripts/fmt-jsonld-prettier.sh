#!/usr/bin/env bash
# Compare the formatter's JSON-LD golden outputs with oxfmt (the UI's pinned formatter,
# which follows Prettier's JSON conventions): each expected output
# (crates/sparkles-fmt/tests/golden/jsonld/*.out.jsonld) goes through oxfmt as JSON at the
# same width and indent, and every difference is shown as a unified diff.
#
# Expected differences, from rules where sparkles fmt deliberately departs from Prettier:
# - an array holding an object or an array always takes one element per line, where
#   Prettier keeps `[{ "@id": "x" }]` or `[[1, 2], [3, 4]]` on one line when it fits;
# - a broken array of numbers takes one number per line, where Prettier fills lines;
# - number lexemes are printed as written, where oxfmt rewrites them (`1.50` to `1.5`,
#   `1E3` to `1e3`);
# - an object of two or more members is always expanded, where Prettier keeps an object
#   on one line when it was written on one line (the golden outputs are already
#   expanded, so this shows only in the inputs);
# - key order: Prettier never reorders keys (oxfmt cannot show this: it formats the
#   outputs, whose keys are already in order).
#
# Usage: scripts/fmt-jsonld-prettier.sh [--strict]
#   --strict  exit 1 when any output differs (default: report only)
# Opt-in (`mise run fmt:jsonld-compare`), not part of `mise run ci`.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
oxfmt="$root/ui/node_modules/.bin/oxfmt"
strict=0
if [ "${1:-}" = "--strict" ]; then
  strict=1
fi
if [ ! -x "$oxfmt" ]; then
  echo "oxfmt is not installed: run mise run ui:install" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

total=0
differ=0
for out in "$root"/crates/sparkles-fmt/tests/golden/jsonld/*.out.jsonld; do
  name="$(basename "$out")"
  width=100
  case "$name" in
    *.w[0-9]*.out.jsonld)
      width="${name##*.w}"
      width="${width%%.out.jsonld}"
      ;;
  esac
  printf '{ "printWidth": %s, "tabWidth": 2, "useTabs": false }\n' "$width" > "$work/oxfmtrc.json"
  total=$((total + 1))
  if ! "$oxfmt" -c "$work/oxfmtrc.json" --stdin-filepath=golden.json < "$out" > "$work/formatted.json"; then
    echo "$name: oxfmt failed" >&2
    exit 2
  fi
  if ! diff -u --label "$name (sparkles fmt)" --label "$name (oxfmt)" "$out" "$work/formatted.json"; then
    differ=$((differ + 1))
  fi
done

echo "$differ of $total JSON-LD golden outputs differ from oxfmt"
if [ "$strict" = 1 ] && [ "$differ" -gt 0 ]; then
  exit 1
fi
