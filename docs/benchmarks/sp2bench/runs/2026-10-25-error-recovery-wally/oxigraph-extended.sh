#!/usr/bin/env bash
set -euo pipefail
exec 8> "$HOME/lanes/.bench.lock"
flock -n 8 || exit 75
exec 9> "$HOME/lanes/.heavy.lock"
flock -n 9 || exit 75
lane="$HOME/lanes/benchmark-error-recovery-20261025"
export ROOT="$lane/src-reviewed" SPARKLES="$HOME/lanes/competitive-births-release-20261025/target-D/release/sparkles" SPARKLES_COMMIT=6b7f32ce SP2B="$lane/sp2b-gen"
export OXIGRAPH=/nix/store/g7ajfdk0g8bypdbsj23hnfq71izm3iz1-oxigraph-0.5.11/bin/oxigraph
export PORT_BASE=28460 SERVER_MEM_MAX=24G CLIENT_CPU=11 MAX_TIME=1800 ANSWERS_RAW_DIR=raw-answers
receipt="$lane/oxigraph-long-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$receipt"
echo "$receipt" > "$lane/active-oxi-long"
prior=$(cat "$lane/active-sp2b")
ln -s "$prior/data/data.nt" "$receipt/data.nt"
cp -a --reflink=auto "$prior/oxigraph/oxigraph.db" "$receipt/oxigraph.db"
(date -u +%FT%TZ; hostname; uptime; free -m; df -h "$lane"; ps -eo pid,pcpu,comm --sort=-pcpu | head -12) > "$receipt/preflight.txt"
sha256sum "$OXIGRAPH" "$ROOT/scripts/bench-answers.py" "$ROOT/scripts/bench-sp2b.sh" > "$receipt/inputs.sha256"
ENGINES=oxigraph QUERIES=q4 SKIP_LOAD=1 ANSWERS_ONLY=1 bash "$ROOT/scripts/bench-sp2b.sh" 10m "$receipt" > "$receipt/query.log" 2>&1
python3 - "$receipt" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1]);a=json.loads((p/'results/answers.json').read_text())['q4']['oxigraph'];expected='row-sha256-v1:94c3adc4557a846c5f12f6c39d3ecb0958b3d45b1dc014640f615e40019a374c';r={'probe_version':1,'method':'extended-budget complete-answer verification, not a warm latency benchmark','request_limit_seconds':1800,'server_cap':'24G','rows':a.get('rows'),'value':a.get('value'),'source_oracle_agreement':a.get('rows')==41073618 and a.get('value')==expected,'error':a.get('error')};(p/'validation.json').write_text(json.dumps(r,indent=2)+'\n');print(json.dumps(r),flush=True)
PY
date -u +%FT%TZ > "$receipt/complete.txt"
