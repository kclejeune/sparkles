"""Exercise restart-sensitive preparation through the DBpedia query phase."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PreparationTests(unittest.TestCase):
    def test_dbpedia_preparation_uses_the_restarted_qlever_endpoint(self):
        # Run the actual query phase with fake servers and hyperfine. The first
        # QLever request times out, and restarting it moves its listening port.
        source = (ROOT / "scripts/bench-billion.sh").read_text()
        queries = "step_queries() {" + source.split("step_queries() {", 1)[1].split("\nstep_report()", 1)[0]
        stubs = r'''
set -euo pipefail
source "$ROOT/scripts/bench-probe.sh"
declare -A NAME=([sparkles]=sparkles [qlever]=qlever) URL PORT
ENGINES="sparkles qlever"
NAMES=(q1 q2)
S=$PWD SCALE=test COLD=0 TIMEOUT=1 WARMUP=1 RUNS=1 THROUGHPUT="q2"
HERE=$PWD NREQ=1 CONC=1 COUNT_ONLY=""
mkdir -p results
touch sparkles.loaded qlever.loaded
starts=0
resolve_tools() { :; }
has() { [[ " $ENGINES " == *" $1 "* ]]; }
selected() { return 0; }
start() {
    if [ "$1" = qlever ]; then
        PORT[qlever]=$((4000 + starts))
        starts=$((starts + 1))
    else PORT[$1]=3000; fi
    URL[$1]="localhost:${PORT[$1]}/"
}
stop() { unset "PORT[$1]"; }
answering() { [ "${PORT[$1]}" != 4000 ]; }
q() { if [ "$1" = localhost:4000/ ]; then echo "(exit 28)"; else echo true; fi; }
curl() {
    printf '%s\n' "${@: -1}" >> preparation-calls
    [[ "${@: -1}" == 'localhost:4001/?cmd=clear-cache&access-token=bench' ]]
}
log() { :; }
sleep() { :; }
pid_on() { echo $$; }
python3() {
    if [[ "${1:-}" == */bench-answers.py ]]; then echo 1; else command python3 "$@"; fi
}
merge() { mv "$1" "$2"; }
hyperfine() {
    local prep= out= name names=()
    while [ $# -gt 0 ]; do
        case $1 in
            --prepare) prep=$2; shift 2 ;;
            --export-json) out=$2; shift 2 ;;
            --command-name) names+=("$2"); shift 3 ;;
            *) shift ;;
        esac
    done
    eval "$prep" || return $?
    command python3 - "$out" "${names[@]}" << 'EOF'
import json, sys
json.dump({"results": [{"command": n, "times": [0.01], "exit_codes": [0]} for n in sys.argv[2:]]}, open(sys.argv[1], "w"))
EOF
}
'''
        with tempfile.TemporaryDirectory() as work:
            result = subprocess.run(["bash", "-c", stubs + "\n" + queries + "\nstep_queries"],
                                    cwd=work, env=os.environ | {"ROOT": str(ROOT)},
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr + "\n" +
                             ((Path(work) / "preparation-calls").read_text() if
                              (Path(work) / "preparation-calls").exists() else "no preparation calls"))
            calls = (Path(work) / "preparation-calls").read_text().splitlines()
            self.assertEqual(calls, ["localhost:4001/?cmd=clear-cache&access-token=bench"] * 3)
            first = json.loads((Path(work) / "results/q1.json").read_text())["results"]
            failed = next(r for r in first if r["command"] == "qlever")
            self.assertEqual(failed["exit_codes"], [28])
            self.assertTrue(failed["first_request_failed"])
            self.assertTrue(any(r["command"] == "sparkles" for r in first))
            restarts = json.loads((Path(work) / "results/restarts.json").read_text())
            self.assertEqual([(r["engine"], r["after"]) for r in restarts], [("qlever", "q1")])

    def test_preparation_does_not_contact_an_absent_or_dead_qlever(self):
        for engines, dead in [("sparkles", ""), ("sparkles qlever", "qlever")]:
            with self.subTest(engines=engines, dead=dead):
                script = r'''
set -euo pipefail
source "$ROOT/scripts/bench-probe.sh"
declare -A URL
curl() { exit 99; }
eval "$(cache_prepare)"
'''
                result = subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                                        env=os.environ | {"ROOT": str(ROOT), "ENGINES": engines,
                                                          "DEAD_ENGINES": dead}, timeout=5)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
