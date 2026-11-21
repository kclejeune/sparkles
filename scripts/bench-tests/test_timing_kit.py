"""Tests of scripts/timing-kit.py with a stub server in place of sparkles.

    python3 -m unittest discover -s scripts/bench-tests -p test_timing_kit.py
"""

import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
KIT = SCRIPTS / "timing-kit.py"
STUB = Path(__file__).resolve().parent / "fixtures" / "stub-sparkles.py"
NEEDS = ("taskset", "chrt", "zstd", "curl")
CONTROLS = ["class-predicates", "exists-join", "star-join"]


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def stub(d, identity, exec_ms, answer="1"):
    """A stub binary of this identity and execution time, whose every answer is the value answer."""
    path = Path(d) / f"{identity}.bin"
    path.write_text(STUB.read_text().replace('IDENTITY = "stub"', f'IDENTITY = "{identity}"')
                    .replace("EXEC_MS = 1.0", f"EXEC_MS = {exec_ms}")
                    .replace('out = b"?c\\n1\\n"', f'out = b"?c\\n{answer}\\n"')
                    .replace('"value": "1"', f'"value": "{answer}"'))
    path.chmod(0o755)
    return path


def fake_sysfs(d, cpus=4, siblings=None, core=None):
    """A /sys with CPUs 0..cpus-1 online, each its own core unless siblings says otherwise."""
    root = Path(d) / "sys"
    cpu = root / "devices/system/cpu"
    cpu.mkdir(parents=True)
    (cpu / "online").write_text(f"0-{cpus - 1}\n")
    for c in range(cpus):
        (cpu / f"cpu{c}/topology").mkdir(parents=True)
        (cpu / f"cpu{c}/topology/thread_siblings_list").write_text((siblings or {}).get(c, str(c)) + "\n")
    if core is not None:
        (root / "devices/cpu_core").mkdir(parents=True)
        (root / "devices/cpu_core/cpus").write_text(core + "\n")
    return root


def kit(*args, env=None, timeout=300):
    return subprocess.run([sys.executable, str(KIT), *map(str, args)], capture_output=True, text=True,
                          env={**os.environ, **(env or {})}, timeout=timeout)


class KitTests(unittest.TestCase):
    def setUp(self):
        self.d = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.d, True)
        q = self.d / "queries"
        q.mkdir()
        for name in CONTROLS + ["tiny"]:
            (q / f"{name}.rq").write_text("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }\n")
        subprocess.run(["zstd", "-q", "-o", str(self.d / "data.nt.zst")], input=b"", check=True)
        self.locks = self.d / "locks"
        self.locks.mkdir()
        for lock in (".bench.lock", ".heavy.lock"):
            (self.locks / lock).touch()
        self.env = {"TIMING_KIT_SYSFS": str(fake_sysfs(self.d)), "TIMING_KIT_LOCKS": str(self.locks),
                    "MBX_REMOTE_URL": ""}

    def stage(self, *extra, tips=(("fast", 1.0), ("slow", 1.3)), name="stage"):
        base = stub(self.d, "base", 1.0)
        args = ["stage", "--out", self.d / name, "--base", f"base={base}", "--data", self.d / "data.nt.zst",
                "--queries", self.d / "queries", "--controls", ",".join(CONTROLS), "--rounds", "2", "--reps", "2",
                "--warmup", "1", "--server-cpu", "0", "--client-cpu", "1", "--port", free_port(),
                "--rerun-rounds", "2", "--rerun-reps", "3"]
        for label, ms, *answer in tips:
            args += ["--tip", f"{label}={stub(self.d, label, ms, *answer)}"]
        return kit(*args, *extra), self.d / name

    def test_stage_writes_one_interleaved_run_for_a_shared_base(self):
        r, s = self.stage()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        env = (s / "run.env").read_text()
        self.assertIn("VARIANTS='A B C'\n", env)
        self.assertIn("ORDERS='A B C C B A'\n", env)
        self.assertIn("PAIRS='A,B A,C'\n", env)
        self.assertIn("SPIN=1\n", env)
        self.assertIn("DYNAMIC_A=0\n", env)
        arms = json.loads((s / "kit.json").read_text())["arms"]
        self.assertEqual([(a["variant"], a["label"]) for a in arms], [("A", "base"), ("B", "fast"), ("C", "slow")])
        self.assertEqual(len((s / "binaries.sha256").read_text().splitlines()), 3)
        for f in ("nsc-bench-remote.sh", "nsc-bench-summary.py", "timing-kit.py"):
            self.assertTrue((s / f).is_file(), f)
        r = kit("stage", "--out", s, "--base", "x=y", "--data", "z")
        self.assertIn("exists", r.stderr)

    def test_several_clients_time_every_arm_under_each(self):
        r, s = self.stage("--clients", "curl,persistent:0,persistent:2", tips=(("tip", 1.0),))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        env = (s / "run.env").read_text()
        self.assertIn("VARIANTS='A B C D E F'\n", env)
        self.assertIn("PAIRS='A,B C,D E,F'\n", env)
        self.assertIn("XPAIRS='A,C B,D A,E B,F'\n", env)
        self.assertIn("BINARY_OF_D=B\n", env)
        self.assertIn("STORE_OF_E=A\n", env)
        self.assertIn("REQUEST_CLIENT_A=curl\n", env)
        self.assertIn("REQUEST_GAP_MS_F=2\n", env)
        self.assertEqual(sorted(p.name for p in s.glob("bin-*")), ["bin-A", "bin-B"])
        r = kit("preflight", s)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_preflight_refuses_an_unpinned_run_without_busy_loops(self):
        # the setup of a lane run that pinned nothing and ran no busy loops
        r, s = self.stage(tips=(("tip", 1.0),))
        env = (s / "run.env").read_text()
        env = env.replace("SERVER_CPU=0\n", "").replace("CLIENT_CPU=1\n", "").replace("SPIN=1\n", "SPIN=0\n")
        env = env.replace("ORDERS='A B B A'", "ORDERS='A B'")
        (s / "run.env").write_text(env)
        r = kit("preflight", s)
        self.assertEqual(r.returncode, 1)
        for want in ("SERVER_CPU must name one CPU", "CLIENT_CPU must name one CPU", "SPIN must be 1",
                     "runs A 1 time(s) a round", "not a palindrome"):
            self.assertIn(want, r.stderr)

    def test_preflight_refuses_bad_hashes_controls_and_mixed_clients(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        with open(s / "bin-B", "a") as f:
            f.write("# changed\n")
        (s / "queries" / "star-join.rq").unlink()
        (s / "run.env").write_text((s / "run.env").read_text() + "REQUEST_CLIENT_B=persistent\n")
        r = kit("preflight", s)
        self.assertEqual(r.returncode, 1)
        self.assertIn("bin-B does not match", r.stderr)
        self.assertIn("only 2 control queries", r.stderr)
        self.assertIn("mixes request clients", r.stderr)

    def test_preflight_reads_only_plain_assignments(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        (s / "run.env").write_text((s / "run.env").read_text() + "export SERVER_CPU=1\nREPS=$((0))\n")
        r = kit("preflight", s)
        self.assertEqual(r.returncode, 1)
        self.assertIn("is not a plain KEY=VALUE assignment: export SERVER_CPU=1", r.stderr)
        self.assertIn("is not a plain KEY=VALUE assignment: REPS=$((0))", r.stderr)

    def test_preflight_refuses_counts_that_cannot_decide(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        env = (s / "run.env").read_text()
        (s / "run.env").write_text(env.replace("REPS=2\n", "REPS=0\n").replace("RERUN_ROUNDS=2\n", "RERUN_ROUNDS=1\n")
                                   .replace("FLAG_RATIO=1.10", "FLAG_RATIO=0.9") + "RERUN_REPS=0\n")
        r = kit("preflight", s)
        self.assertIn("REPS must be a whole number of at least 1, not 0", r.stderr)
        self.assertIn("RERUN_ROUNDS must be a whole number of at least 2, not 1", r.stderr)
        self.assertIn("FLAG_RATIO must be a ratio above 1", r.stderr)
        self.assertIn("RERUN_REPS must be a whole number of at least 1, not 0", r.stderr)

    def test_preflight_refuses_early_stopping_that_acts(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        self.assertIn("EARLY_STOP=shadow\n", (s / "run.env").read_text())
        (s / "run.env").write_text((s / "run.env").read_text().replace("EARLY_STOP=shadow", "EARLY_STOP=on"))
        r = kit("preflight", s)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("EARLY_STOP must be off or shadow, not on", r.stderr)

    def test_preflight_needs_a_hash_of_every_binary(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        lines = (s / "binaries.sha256").read_text().splitlines()
        (s / "binaries.sha256").write_text(lines[0] + "\n")
        r = kit("preflight", s)
        self.assertIn("binaries.sha256 has no hash of bin-B", r.stderr)
        (s / "binaries.sha256").write_text("")
        r = kit("preflight", s)
        self.assertIn("binaries.sha256 has no hash of bin-A", r.stderr)
        (s / "binaries.sha256").write_text("junk\n")
        r = kit("preflight", s)
        self.assertIn("a line that is not HASH  bin-V: junk", r.stderr)

    def test_launch_refuses_names_a_shell_would_read(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        for args, want in ((["--unit", "x;rm -rf ~"], "unit name"), (["--unit", "ok", "--timeout", "1h; reboot"], "timeout"),
                           (["--unit", "ok", "--remote-dir", "/tmp/a b"], "remote directory"),
                           (["--unit", "ok", "--remote-dir", "/tmp/../etc"], "remote directory")):
            r = kit("launch", "--host", "nohost.invalid", "--dry-run", *args, s)
            self.assertEqual(r.returncode, 1, args)
            self.assertIn(want, r.stderr)
        odd = self.d / "odd name"
        shutil.copytree(s, odd)
        r = kit("launch", "--host", "nohost.invalid", "--unit", "ok", "--dry-run", odd)
        self.assertIn("not a plain name", r.stderr)

    def test_preflight_accepts_a_screen_stage(self):
        # a batch screen's stage: the harness's own A/B run.env, without the kit's settings
        s = self.d / "screen"
        (s / "queries").mkdir(parents=True)
        for f in (self.d / "queries").glob("*.rq"):
            shutil.copy(f, s / "queries")
        for v in "AB":
            shutil.copy(stub(self.d, v, 1.0), s / f"bin-{v}")
        shutil.copy(self.d / "data.nt.zst", s)
        for f in ("nsc-bench-remote.sh", "nsc-bench-summary.py"):
            shutil.copy(SCRIPTS / f, s)
        subprocess.run("sha256sum bin-A bin-B > binaries.sha256", shell=True, cwd=s, check=True)
        (s / "run.env").write_text("DYNAMIC_A=0\nDYNAMIC_B=0\nROUNDS=8\nREPS=20\nWARMUP=3\nRAYON_THREADS=1\n"
                                   "SERVER_CPU=2\nCLIENT_CPU=0\nPORT=%d\nSPIN=1\nSERVE_ARGS=\"\"\nEXECUTION=\n"
                                   "CONTROLS=\"%s\"\n" % (free_port(), " ".join(CONTROLS)))
        r = kit("preflight", "--host-side", s, env=self.env)
        self.assertEqual(r.returncode, 0, r.stderr)
        # the kit sends its own driver beside a stage that does not carry it
        r = kit("launch", "--host", "nohost.invalid", "--unit", "kit-test", "--dry-run", s)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"tar -C {SCRIPTS} -cf - timing-kit.py", r.stdout)
        self.assertIn("python3 timing-kit.py run screen", r.stdout)

    def test_host_checks(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        env = dict(self.env)
        env["TIMING_KIT_SYSFS"] = str(fake_sysfs(self.d / "a", siblings={0: "0-1", 1: "0-1"}, core="2-3"))
        env["TIMING_KIT_LOCKS"] = str(self.d)
        env["MBX_REMOTE_URL"] = "https://cache.example"
        port = int([l for l in (s / "run.env").read_text().splitlines() if l.startswith("PORT=")][0][5:])
        with socket.socket() as held:
            held.bind(("127.0.0.1", port))
            held.listen()
            r = kit("preflight", "--host-side", s, env=env)
        self.assertEqual(r.returncode, 1)
        for want in ("are SMT siblings", "CPU 0 is not one of the host's performance cores", f"port {port} is in use",
                     ".bench.lock does not exist", "MBX_REMOTE_URL is set"):
            self.assertIn(want, r.stderr)
        env["TIMING_KIT_SYSFS"] = str(fake_sysfs(self.d / "b", cpus=1))
        r = kit("preflight", "--host-side", s, env=env)
        self.assertIn("CLIENT_CPU 1 is not online", r.stderr)

    def test_launch_dry_run_sends_nothing(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        r = kit("launch", "--host", "nohost.invalid", "--unit", "kit-test", "--dry-run", s)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("dry run: nothing was sent", r.stdout)
        self.assertIn("systemd-run --user --unit=kit-test -p MemoryMax=12G", r.stdout)
        self.assertIn("--setenv=MBX_REMOTE_URL=", r.stdout)
        self.assertIn(".bench.lock flock -w 28800", r.stdout)
        self.assertIn("preflight --host-side stage", r.stdout)
        (s / "run.env").write_text((s / "run.env").read_text().replace("SPIN=1", "SPIN=0"))
        r = kit("launch", "--host", "nohost.invalid", "--unit", "kit-test", "--dry-run", s)
        self.assertEqual(r.returncode, 1)
        self.assertNotIn("systemd-run", r.stdout)

    @unittest.skipUnless(all(shutil.which(t) for t in NEEDS) and {0, 1} <= os.sched_getaffinity(0),
                         "needs taskset, chrt, zstd, curl and CPUs 0 and 1")
    def test_a_stopped_run_leaves_no_process_behind(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        (s / "run.env").write_text((s / "run.env").read_text().replace("ROUNDS=2", "ROUNDS=50")
                                   .replace("=curl\n", "=persistent\n"))
        p = subprocess.Popen([sys.executable, str(KIT), "run", str(s)], env={**os.environ, **self.env},
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        samples = s / "pass1" / "results" / "samples.tsv"
        deadline = time.time() + 60
        while time.time() < deadline and (not samples.exists() or len(samples.read_text().splitlines()) < 5):
            time.sleep(0.2)
        self.assertTrue(samples.exists())
        p.terminate()
        self.assertEqual(p.wait(120), 128 + 15)

        def left():
            out = []
            for proc in Path("/proc").iterdir():
                try:
                    cmd = (proc / "cmdline").read_bytes().replace(b"\0", b" ").decode()
                    cwd = os.readlink(proc / "cwd")
                except OSError:
                    continue
                if str(s) in cmd or cwd.startswith(str(s)):
                    out.append(cmd)
            return out

        self.assertEqual(left(), [])

    @unittest.skipUnless(all(shutil.which(t) for t in NEEDS) and len(os.sched_getaffinity(0)) >= 2
                         and {0, 1} <= os.sched_getaffinity(0), "needs taskset, chrt, zstd, curl and CPUs 0 and 1")
    def test_run_flags_the_slow_tip_and_reruns_it(self):
        r, s = self.stage()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        r = kit("run", s, env=self.env, timeout=600)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual((s / "flagged.txt").read_text().split(), sorted(CONTROLS + ["tiny"]))
        for pas in ("pass1", "rerun"):
            for pair in ("A-B", "A-C"):
                self.assertTrue((s / pas / f"summary-{pair}.json").is_file(), f"{pas} {pair}")
        p1 = json.loads((s / "pass1" / "summary-A-C.json").read_text())
        self.assertAlmostEqual(p1["queries"]["tiny"]["exec_ms"]["ratio"], 1.3, places=2)
        # the harness lists the queries each pass timed, and the rerun times the flagged ones
        for pas in ("pass1", "rerun"):
            self.assertEqual((s / pas / "results" / "queries.txt").read_text().split(), sorted(CONTROLS + ["tiny"]))
        verdict = (s / "verdict.md").read_text()
        self.assertIn("fast (B) against base (A): first pass valid", verdict)
        self.assertIn("flagged none, none slower after the rerun", verdict)
        # shadow mode records the rule's decisions without acting, and with two rounds the
        # rule cannot stop before the last
        self.assertIn("slow (C) against base (A): agrees with all rounds", (s / "shadow.md").read_text())
        sh = json.loads((s / "pass1" / "shadow-A-C.json").read_text())
        self.assertTrue(sh["agree"])
        self.assertEqual(sh["request_share"], 1.0)
        self.assertIn("slow (C) against base (A): first pass valid", verdict)
        self.assertIn("slower after the rerun: class-predicates 1.300", verdict)
        # the run removes its copies of the binaries and data
        self.assertFalse(list((s / "pass1").glob("bin-*")))
        epochs = []
        for line in (s / "pass1" / "results" / "samples.tsv").read_text().splitlines()[1:]:
            p = line.split("\t")
            if not epochs or epochs[-1][0] != p[1]:
                epochs.append((p[1], p[2]))
        self.assertEqual("".join(v for _, v in epochs), "ABCCBA" * 2)

    @unittest.skipUnless(all(shutil.which(t) for t in NEEDS) and len(os.sched_getaffinity(0)) >= 2
                         and {0, 1} <= os.sched_getaffinity(0), "needs taskset, chrt, zstd, curl and CPUs 0 and 1")
    def test_a_tip_with_different_answers_cannot_decide(self):
        # B answers as the base does, C is as fast as the base and gives other answers, and D
        # is 30% slower with other answers, so that its flagged rerun is judged too
        r, s = self.stage(tips=(("same", 1.0), ("wrong", 1.0, "2"), ("slowwrong", 1.3, "3")))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        r = kit("run", s, env=self.env, timeout=600)
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        verdict = (s / "verdict.md").read_text()
        # B answers as A does, but the other arms of its run do not, so it cannot decide either
        self.assertIn("same (B) against base (A): first pass INVALID (spread 0.0%, answers of the run's arms differ for", verdict)
        self.assertIn("wrong (C) against base (A): first pass INVALID", verdict)
        self.assertIn("slowwrong (D) against base (A): first pass INVALID", verdict)
        for line in verdict.splitlines():
            if "wrong" in line:
                self.assertIn("answers differ for " + ", ".join(sorted(CONTROLS + ["tiny"])), line)
            if "slowwrong" in line:
                self.assertIn("rerun INVALID", line)
        for pair, status in (("A-B", "same"), ("A-C", "differ"), ("A-D", "differ")):
            p1 = json.loads((s / "pass1" / f"summary-{pair}.json").read_text())
            self.assertEqual(set(p1["answers"].values()), {status}, pair)
            self.assertEqual(set(p1["answers_all_arms"].values()), {"differ"}, pair)
        self.assertEqual(set(json.loads((s / "rerun" / "summary-A-D.json").read_text())["answers"].values()), {"differ"})


def load_kit():
    spec = importlib.util.spec_from_file_location("timing_kit", KIT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def write_samples(d, ratio, rounds=8, noise=None, controls=CONTROLS, query="q", spread=0.0):
    """A pass of A,B,B,A rounds of 2 requests, where B takes ratio times A's 1 ms on query,
    with noise[k] scaling B in round k, and B is as fast as A on the controls. With spread,
    the second server process of each variant in a round is that much slower on every query."""
    res = Path(d) / "results"
    res.mkdir(parents=True)
    lines = ["round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms"]
    epoch = 0
    for k in range(1, rounds + 1):
        for i, v in enumerate("ABBA"):
            epoch += 1
            slow = 1 + spread * (i >= 2)
            for q in [query, *controls]:
                ms = (ratio * (noise[k - 1] if noise else 1) if v == "B" and q == query else 1.0) * slow
                for rep in (1, 2):
                    lines.append(f"{k}\t{epoch}\t{v}\t{q}\t{rep}\t{ms}\t{ms + 1}")
    (res / "samples.tsv").write_text("\n".join(lines) + "\n")
    return res.parent


class ShadowTests(unittest.TestCase):
    def setUp(self):
        self.kit = load_kit()
        self.d = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.d, True)

    def test_stop_rule_stops_inside_outside_or_not_at_all(self):
        rule = self.kit.stop_rule
        flat = {k: {"A": [1.0, 1.0], "B": [1.0 + 0.001 * k, 1.0]} for k in range(1, 9)}
        self.assertEqual(rule(flat, 1.10), (3, False, "inside"))
        slow = {k: {"A": [1.0], "B": [1.3 + 0.001 * k]} for k in range(1, 9)}
        self.assertEqual(rule(slow, 1.10), (3, True, "outside"))
        # rounds that swing across the bound never stop, and all rounds decide
        swing = {k: {"A": [1.0], "B": [1.25 if k % 2 else 0.98]} for k in range(1, 9)}
        self.assertEqual(rule(swing, 1.10), (8, True, "all"))
        # an interval inside the bound with a pooled ratio at the bound does not stop
        # inside, as in the replayed rule
        self.assertEqual(rule({1: {"A": [1.0], "B": [1.0]}, 2: {"A": [1.0], "B": [1.0]}}, 1.10), (2, False, "all"))
        gap = {**flat, 2: {"A": [1.0], "B": []}}
        self.assertEqual(rule(gap, 1.10), (8, False, "unjudged"))
        # more rounds than the t table holds use its last quantile
        long = {k: {"A": [1.0], "B": [1.25 if k % 2 else 0.98]} for k in range(1, 41)}
        self.assertEqual(rule(long, 1.10)[2], "all")

    def test_shadow_records_the_rule_beside_all_rounds(self):
        d = write_samples(self.d / "same", 1.0)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertTrue(s["agree"])
        self.assertEqual(s["queries"]["q"]["how"], "inside")
        self.assertEqual(s["queries"]["q"]["stop_round"], 3)
        for c in CONTROLS:
            self.assertEqual((s["queries"][c]["how"], s["queries"][c]["stop_round"]), ("control", 8))
        # q stops after 3 of 8 rounds, and the 3 controls run all 8
        self.assertAlmostEqual(s["request_share"], (3 + 3 * 8) / (4 * 8))
        self.assertEqual(len(s["samples_sha256"]), 64)
        self.assertEqual(s["rule"]["kmin"], 3)
        d = write_samples(self.d / "slow", 1.3)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertEqual((s["queries"]["q"]["how"], s["queries"]["q"]["flagged"]), ("outside", True))
        self.assertTrue(s["agree"])
        # a ratio that the first rounds put inside and the later rounds put well outside
        # stops inside and disagrees with all rounds
        d = write_samples(self.d / "late", 1.0, noise=[1.0, 1.0, 1.0, 1.4, 1.4, 1.4, 1.4, 1.4])
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertFalse(s["agree"])
        self.assertEqual(s["flags_differ"], ["q"])

    def test_shadow_falls_back_in_the_band_and_judges_validity_by_the_controls(self):
        d = write_samples(self.d / "band", 1.0, spread=0.025)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertTrue(s["validity"]["fallback"])
        self.assertEqual(s["request_share"], 1.0)
        self.assertTrue(s["agree"])
        d = write_samples(self.d / "noisy", 1.0, spread=0.05)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertEqual((s["validity"]["all_rounds"], s["validity"]["shadow"], s["agree"]), (False, False, True))
        # without enough controls the shadow cannot be valid
        d = write_samples(self.d / "bare", 1.0, controls=["class-predicates"])
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertEqual((s["validity"]["all_rounds"], s["validity"]["shadow"], s["agree"]), (True, False, False))

    def test_shadow_command_reads_a_screen_pass(self):
        d = write_samples(self.d / "out" / "pass1", 1.0)
        (self.d / "out" / "run.env").write_text("ROUNDS=8\nFLAG_RATIO=1.20\n")
        r = kit("shadow", d)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("B against A: agrees with all rounds", r.stdout)
        s = json.loads((d / "shadow-A-B.json").read_text())
        self.assertEqual(s["rule"]["bound"], 1.20)
        self.assertEqual(s["controls"], CONTROLS)


if __name__ == "__main__":
    unittest.main()
