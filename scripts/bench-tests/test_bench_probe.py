"""Exercise restart-sensitive preparation through the DBpedia query phase."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PreparationTests(unittest.TestCase):
    def run_dbpedia(self, work, *, fail_timing=False):
        # Run the actual query phase and merge function with fake servers and timing.
        # QLever fails its first timed query's probe, after a complete warm pass.
        source = (ROOT / "scripts/bench-billion.sh").read_text()
        queries = "step_queries() {" + source.split("step_queries() {", 1)[1].split("\nstep_report()", 1)[0]
        merge = "merge() {" + source.split("merge() {", 1)[1].split("\nsetcold()", 1)[0]
        cleanup = "stop_all() {" + source.split("stop_all() {", 1)[1].split("\nPSI_PID=", 1)[0]
        stubs = r'''
set -euo pipefail
source "$ROOT/scripts/bench-probe.sh"
declare -A NAME=([sparkles]=sparkles [qlever]=qlever) URL PORT REQUESTS
ENGINES="sparkles qlever"
NAMES=(q1 q2)
S=$PWD SCALE=test COLD=0 TIMEOUT=1 WARMUP=1 RUNS=1 THROUGHPUT="q2"
HERE=$PWD NREQ=1 CONC=1 COUNT_ONLY=""
mkdir -p results
touch sparkles.loaded qlever.loaded
starts=0 hung=0
event() { printf '%s|%s\n' "$*" "${!PORT[*]}" >> events; }
resolve_tools() { :; }
has() { [[ " $ENGINES " == *" $1 "* ]]; }
selected() { return 0; }
start() {
    if [ "$1" = qlever ]; then
        PORT[qlever]=$((4000 + starts))
        starts=$((starts + 1))
    else PORT[$1]=3000; fi
    URL[$1]="localhost:${PORT[$1]}/"
    hung=0
    event "start $1"
}
stop() { event "stop $1"; unset "PORT[$1]"; }
answering() { [ "$hung" = 0 ]; }
q() { printf 'request %q %q\n' "$1" "$2"; }
request() {
    local engine=sparkles key
    [[ "$1" == localhost:4* ]] && engine=qlever
    event "request $engine $2"
    key=$engine-$2
    REQUESTS[$key]=$(( ${REQUESTS[$key]:-0} + 1 ))
    if [ "$key" = qlever-q1 ] && [ "${REQUESTS[$key]}" = 2 ]; then
        hung=1
        return 28
    fi
}
curl() {
    printf '%s\n' "${@: -1}" >> preparation-calls
    [[ "${@: -1}" == "${URL[qlever]}?cmd=clear-cache&access-token=bench" ]]
}
log() { :; }
sleep() { :; }
pid_on() { event "rss $ENGINES"; echo $$; }
python3() {
    if [[ "${1:-}" == */bench-answers.py ]]; then
        event "answer ${@: -1}"
        echo 1
    else command python3 "$@"; fi
}
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
    event "timing $out ${names[*]}"
    if [ -n "${FAIL_TIMING:-}" ]; then
        # Model an additional retained process while ENGINES is narrowed for timing.
        PORT[retained]=9000
        return 42
    fi
    eval "$prep" || return $?
    command python3 - "$out" "${names[@]}" << 'EOF'
import json, sys
json.dump({"results": [{"command": n, "times": [0.01], "exit_codes": [0]} for n in sys.argv[2:]]}, open(sys.argv[1], "w"))
EOF
}
'''
        return subprocess.run(["bash", "-c", stubs + "\n" + merge + "\n" + cleanup + "\n" + queries + r'''
trap 'stop_all' EXIT
step_queries
echo "$ENGINES" > restored-engines
'''], cwd=work, env=os.environ | {"ROOT": str(ROOT), "FAIL_TIMING": "1" if fail_timing else ""},
                              capture_output=True, text=True, timeout=15)

    def test_dbpedia_isolates_warm_timing_throughput_and_rss(self):
        with tempfile.TemporaryDirectory() as work:
            result = self.run_dbpedia(work)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr + "\n" +
                             ((Path(work) / "preparation-calls").read_text() if
                              (Path(work) / "preparation-calls").exists() else "no preparation calls"))
            calls = (Path(work) / "preparation-calls").read_text().splitlines()
            events = [line.split("|") for line in (Path(work) / "events").read_text().splitlines()]
            for action, active in events:
                if action.startswith(("request ", "timing ", "rss ")):
                    self.assertEqual(len(active.split()), 1, f"measurement lacked isolation: {action}: {active}")
            self.assertEqual(calls, ["localhost:4002/?cmd=clear-cache&access-token=bench"] * 2)
            for engine in ("sparkles", "qlever"):
                first_timing = next(i for i, (action, _) in enumerate(events)
                                    if action.startswith("timing ") and action.endswith(" " + engine))
                warm = [action for action, _ in events[:first_timing] if action.startswith("request " + engine + " ")]
                self.assertEqual(warm[:2], [f"request {engine} q1", f"request {engine} q2"])
            self.assertEqual((Path(work) / "restored-engines").read_text().strip(), "sparkles qlever")
            first = json.loads((Path(work) / "results/q1.json").read_text())["results"]
            failed = next(r for r in first if r["command"] == "qlever")
            self.assertEqual(failed["exit_codes"], [28])
            self.assertTrue(failed["first_request_failed"])
            self.assertTrue(any(r["command"] == "sparkles" for r in first))
            for query in ("q2", "throughput-q2"):
                rows = json.loads((Path(work) / f"results/{query}.json").read_text())["results"]
                self.assertEqual({r["command"] for r in rows}, {"sparkles", "qlever"})
            self.assertEqual(set(json.loads((Path(work) / "results/rss.json").read_text())), {"sparkles", "qlever"})
            restarts = json.loads((Path(work) / "results/restarts.json").read_text())
            self.assertEqual([(r["engine"], r["after"]) for r in restarts], [("qlever", "q1")])

    def test_dbpedia_failure_cleanup_stops_active_and_retained_servers(self):
        with tempfile.TemporaryDirectory() as work:
            result = self.run_dbpedia(work, fail_timing=True)
            self.assertEqual(result.returncode, 42, result.stdout + result.stderr)
            actions = [line.split("|")[0] for line in (Path(work) / "events").read_text().splitlines()]
            self.assertIn("stop retained", actions)
            self.assertEqual(actions.count("stop sparkles"), 2)

    def test_dbpedia_applies_the_memory_scope_to_every_server(self):
        source = (ROOT / "scripts/bench-billion.sh").read_text()
        servers = source.split("# --------------------------------------------------------------------------- servers\n", 1)[1].split("\nstop()", 1)[0]
        with tempfile.TemporaryDirectory(prefix="dbpedia cap ") as work:
            root = Path(work)
            (root / "logs").mkdir()
            (root / "qlever-index").mkdir()
            (root / "fluree").mkdir()
            wrapper = root / "systemd-run"
            wrapper.write_text("""#!/usr/bin/env python3
import json, os, sys
with open(os.environ['CAP_LOG'], 'a') as f:
    f.write(json.dumps({'engine': os.environ['ENGINE'], 'args': sys.argv[1:]}) + '\\n')
args = sys.argv[sys.argv.index('--') + 1:]
os.execv(args[0], args)
""")
            wrapper.chmod(0o755)
            backend = root / "fake engine"
            backend.write_text("#!/bin/sh\nexit 0\n")
            backend.chmod(0o755)
            script = r'''
set -euo pipefail
S=$PWD QUERY_MEM_GB=12 TIMEOUT=1
die() { echo "$*" >&2; exit 1; }
''' + servers + r'''
freeport() { echo 12345; }
curl() { return 0; }
for ENGINE in sparkles qlever jena oxigraph fluree; do
    export ENGINE
    start "$ENGINE"
    wait "${PID[$ENGINE]}"
done
'''
            env = os.environ | {name: str(backend) for name in ("SPARKLES", "QSERVER", "FUSEKI", "OXIGRAPH", "FLUREE")}
            cap_log = root / "cap-calls"
            env.update(PATH=str(root) + os.pathsep + env["PATH"], CAP_LOG=str(cap_log), SERVER_MEM_MAX="1G")
            result = subprocess.run(["bash", "-c", script], cwd=work, env=env,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            calls = [json.loads(line) for line in cap_log.read_text().splitlines()]
            self.assertEqual({c["engine"] for c in calls}, {"sparkles", "qlever", "jena", "oxigraph", "fluree"})
            self.assertEqual(len(calls), 5)
            for call in calls:
                self.assertEqual(call["args"][:9], ["--user", "--scope", "--quiet", "-p", "MemoryMax=1G",
                                                    "-p", "MemorySwapMax=0", "--", str(backend)])

            # With no configured limit, none of the launch commands uses systemd-run.
            cap_log.unlink()
            env.pop("SERVER_MEM_MAX")
            result = subprocess.run(["bash", "-c", script], cwd=work, env=env,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(cap_log.exists())

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
