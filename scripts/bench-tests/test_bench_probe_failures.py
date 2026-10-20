"""A crashed timed engine must not prevent the next engine's measurement."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class TimedFailureTests(unittest.TestCase):
    def test_a_crash_during_timing_is_recorded_and_the_next_engine_runs(self):
        script = r'''
set -euo pipefail
source "$ROOT/scripts/bench-probe.sh"
declare -A NAME=([qlever]=qlever [sparkles]=sparkles) URL=([qlever]=localhost:4000/ [sparkles]=localhost:3000/)
ENGINES='qlever sparkles' WARMUP=1 RUNS=1
mkdir results
q() { echo true; }
log() { :; }
answering() { [ "$1" != qlever ] || [ ! -f dead ]; }
merge() { mv "$1" "$2"; }
hyperfine() {
    local fail=0 out= names=()
    while [ $# -gt 0 ]; do
        case $1 in
            --command-name) names+=("$2"); [ "$2" != qlever ] || fail=1; shift 3 ;;
            --export-json) out=$2; shift 2 ;;
            *) shift ;;
        esac
    done
    python3 - "$out" "${names[@]}" << 'EOF'
import json, sys
json.dump({"results": [{"command": n, "times": [0.01], "exit_codes": [0]} for n in sys.argv[2:]]}, open(sys.argv[1], "w"))
EOF
    if [ "$fail" = 1 ]; then touch dead; return 1; fi
}
time_query q1 results
'''
        with tempfile.TemporaryDirectory() as work:
            result = subprocess.run(["bash", "-c", script], cwd=work,
                                    env=os.environ | {"ROOT": str(ROOT)}, capture_output=True,
                                    text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            rows = json.loads((Path(work) / "results/q1.json").read_text())["results"]
            by_engine = {r["command"]: r for r in rows}
            self.assertNotEqual(by_engine["qlever"]["exit_codes"], [0])
            self.assertFalse(by_engine["qlever"]["first_request_failed"])
            self.assertTrue(by_engine["qlever"]["timed_run_failed"])
            self.assertEqual(by_engine["sparkles"]["exit_codes"], [0])
            self.assertEqual(by_engine["sparkles"]["times"], [0.01])

        # A failed preparation against a responsive server must still stop the
        # harness rather than be hidden as an engine crash.
        with tempfile.TemporaryDirectory() as work:
            live = script.replace('answering() { [ "$1" != qlever ] || [ ! -f dead ]; }',
                                  'answering() { return 0; }')
            result = subprocess.run(["bash", "-c", live], cwd=work,
                                    env=os.environ | {"ROOT": str(ROOT)}, capture_output=True,
                                    text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertFalse((Path(work) / "results/q1.json").exists())
