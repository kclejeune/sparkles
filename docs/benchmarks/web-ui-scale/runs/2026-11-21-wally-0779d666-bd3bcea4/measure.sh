#!/usr/bin/env bash
# Item 735: the first call of the ontology summary and of its head, before and after.
# Usage: measure.sh BASE_BIN TIP_BIN OUT_DIR [REPS]
# Runs on atlas under ~/lanes/.heavy.lock. Loads each dataset once (with the tip binary),
# then for each repetition and arm starts a server on one dataset, times the first call and
# a second call, and reads the server's VmRSS and VmHWM after each.
set -euo pipefail
base=$1 tip=$2 out=$3 reps=${4:-3}
src=$HOME/lanes/onto-summary-735
data=$src/data
mkdir -p "$out" "$data"
declare -A files=(
  [chebi]=$src/chebi_lite.owl
  [taxon300k]=$HOME/.cache/sparkles-ui-seed/taxon-synthetic-300000.nt
  [taxon1m]=$src/taxon-synthetic-1000000.nt
)
port=39735
for ds in chebi taxon300k taxon1m; do
  if [ ! -e "$data/$ds/.loaded" ]; then
    rm -rf "${data:?}/$ds"
    systemd-run --user --scope -q -p MemoryMax=12G "$tip" load --loc "$data/$ds" "${files[$ds]}" > "$out/load-$ds.log" 2>&1
    touch "$data/$ds/.loaded"
  fi
done
sha256sum "${files[@]}" > "$out/inputs.sha256"
"$base" --version > "$out/base.version" 2>&1 || true
"$tip" --version > "$out/tip.version" 2>&1 || true

rss() { awk '/^VmRSS|^VmHWM/ {printf "%s=%s ", $1, $2}' "/proc/$1/status"; }
call() { # url -> seconds
  curl -s -o "$2" -w '%{time_total} %{http_code}' "$1"
}
serve() { # bin ds
  rm -rf "$out/srv-data"
  systemd-run --user --scope -q -p MemoryMax=12G "$1" serve --data "$out/srv-data" --host 127.0.0.1 --port $port --loc "$2=$data/$2" > "$out/serve.log" 2>&1 &
  pid=$!
  for _ in $(seq 1 1200); do curl -sf "http://127.0.0.1:$port/\$/ping" > /dev/null && break; sleep 0.1; done
  for _ in $(seq 1 6000); do curl -sf "http://127.0.0.1:$port/\$/ready/$2" > /dev/null && break; sleep 0.1; done
}
stop() { kill "$pid"; wait "$pid" 2> /dev/null || true; }

u="http://127.0.0.1:$port/\$/ontology"
for r in $(seq 1 "$reps"); do
  for ds in chebi taxon300k taxon1m; do
    for arm in base tip; do
      bin=$base
      [ $arm = tip ] && bin=$tip
      serve "$bin" "$ds"
      line="rep=$r ds=$ds arm=$arm start: $(rss $pid)"
      if [ $arm = base ]; then
        line+=" | full1 $(call "$u/$ds" "$out/$ds-base-full.json") $(rss $pid)"
        line+=" | full2 $(call "$u/$ds" /dev/null) $(rss $pid)"
      else
        line+=" | head1 $(call "$u/$ds?view=head" "$out/$ds-tip-head.json") $(rss $pid)"
        line+=" | head2 $(call "$u/$ds?view=head" /dev/null) $(rss $pid)"
        line+=" | full1 $(call "$u/$ds" "$out/$ds-tip-full.json") $(rss $pid)"
      fi
      echo "$line" | tee -a "$out/runs.txt"
      stop
    done
  done
done
# the head answers what the full summary does, on each dataset
for ds in chebi taxon300k taxon1m; do
  keys='{dataset, commit, mode, graphs, imports, warnings, header, entities, closure}'
  if diff <(jq -S "$keys" "$out/$ds-tip-head.json") <(jq -S "$keys" "$out/$ds-tip-full.json") > "$out/$ds-diff.txt" &&
    diff <(jq -S 'del(.cached)' "$out/$ds-base-full.json") <(jq -S 'del(.cached)' "$out/$ds-tip-full.json") >> "$out/$ds-diff.txt"; then
    echo "$ds: head equals full, and the full summary is unchanged" | tee -a "$out/runs.txt"
  else
    echo "$ds: DIFFERS, see $ds-diff.txt" | tee -a "$out/runs.txt"
  fi
done
echo done > "$out/DONE"
