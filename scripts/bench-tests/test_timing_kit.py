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

    def test_preflight_refuses_an_expected_answer_change_without_a_reason_or_a_staged_query(self):
        r, s = self.stage("--expected-answer-change", "tiny,star-join", "--expected-answer-change-reason",
                          "the fix of the join returns the rows it lost", tips=(("tip", 1.0),))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        env = (s / "run.env").read_text()
        self.assertIn("EXPECTED_ANSWER_CHANGE=tiny,star-join\n", env)
        self.assertIn("EXPECTED_ANSWER_CHANGE_REASON='the fix of the join returns the rows it lost'\n", env)
        for change, reason, want in (("tiny,nosuch", "a fix", "nosuch is not a staged query"),
                                     ("tiny", "", "gives no reason"),
                                     ("", "a fix", "names no queries"),
                                     ("tiny star-join", "a fix", "comma-separated list of query names")):
            text = "".join(line + "\n" for line in env.splitlines() if not line.startswith("EXPECTED_ANSWER_CHANGE"))
            text += f"EXPECTED_ANSWER_CHANGE='{change}'\nEXPECTED_ANSWER_CHANGE_REASON='{reason}'\n"
            (s / "run.env").write_text(text)
            r = kit("preflight", s)
            self.assertEqual(r.returncode, 1, change)
            self.assertIn(want, r.stderr, change)
        # a reason that run.env cannot hold as one plain quoted string is refused at stage time
        r, _ = self.stage("--expected-answer-change", "tiny", "--expected-answer-change-reason", "it's fixed",
                          tips=(("tip", 1.0),), name="quote")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("reason", r.stderr)
        r, _ = self.stage("--expected-answer-change", "tiny", tips=(("tip", 1.0),), name="bare")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("reason", r.stderr)

    def test_stage_chooses_the_busy_loop_and_preflight_refuses_others(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("SPIN_LOOP=user\n", (s / "run.env").read_text())
        r, s = self.stage("--spin-loop", "syscall", tips=(("tip", 1.0),), name="syscall")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        env = (s / "run.env").read_text()
        self.assertIn("SPIN_LOOP=syscall\n", env)
        (s / "run.env").write_text(env.replace("SPIN_LOOP=syscall", "SPIN_LOOP=fast"))
        r = kit("preflight", s)
        self.assertEqual(r.returncode, 1)
        self.assertIn("SPIN_LOOP must be user or syscall, not fast", r.stderr)

    def test_preflight_refuses_early_stopping_that_acts(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        self.assertIn("EARLY_STOP=shadow\n", (s / "run.env").read_text())
        (s / "run.env").write_text((s / "run.env").read_text().replace("EARLY_STOP=shadow", "EARLY_STOP=on"))
        r = kit("preflight", s)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("EARLY_STOP must be off or shadow, not on", r.stderr)

    def test_preflight_refuses_an_unknown_early_stopping_question(self):
        r, s = self.stage(tips=(("tip", 1.0),))
        env = (s / "run.env").read_text()
        self.assertIn("EARLY_STOP_RULES=bound\n", env)
        (s / "run.env").write_text(env.replace("EARLY_STOP_RULES=bound", "EARLY_STOP_RULES=bound,median"))
        r = kit("preflight", s)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("EARLY_STOP_RULES must be a comma-separated list of bound, every-round, not bound,median", r.stderr)

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

    def test_an_empty_own_binary_mapping_keeps_its_hash_check(self):
        # the harness reads BINARY_OF_A='' as A's own binary, so preflight must verify bin-A
        r, s = self.stage(tips=(("tip", 1.0),))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(s / "bin-A", "a") as f:
            f.write("# changed after staging\n")
        env = (s / "run.env").read_text()
        for mapping in ("", "BINARY_OF_A=''\n", "BINARY_OF_A=\n", 'BINARY_OF_A=""\n', "BINARY_OF_A=A\n"):
            (s / "run.env").write_text(env + mapping)
            r = kit("preflight", s)
            self.assertEqual(r.returncode, 1, mapping)
            self.assertIn("bin-A does not match binaries.sha256", r.stderr, mapping)

    @unittest.skipUnless(all(shutil.which(t) for t in NEEDS), "needs taskset, chrt, zstd and curl")
    def test_a_harness_rerun_without_the_list_drops_the_old_exemption(self):
        # a run listing q writes results/expected-answer-change.txt, and a later run in the same
        # directory whose run.env lists nothing, or an empty list, must not inherit it
        d = self.d / "rerun"
        (d / "queries").mkdir(parents=True)
        (d / "queries" / "q.rq").write_text("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }\n")
        shutil.copy(SCRIPTS / "nsc-bench-remote.sh", d)
        for v, answer in (("A", "1"), ("B", "2")):
            shutil.copy(stub(self.d, v, 1.0, answer), d / f"bin-{v}")
        shutil.copy(self.d / "data.nt.zst", d)
        base = ("SERVER_CPU=0\nCLIENT_CPU=0\nSPIN=0\nDYNAMIC_A=0\nDYNAMIC_B=0\n"
                f"ROUNDS=1\nREPS=1\nWARMUP=0\nPORT={free_port()}\n")
        listed = "EXPECTED_ANSWER_CHANGE=q\nEXPECTED_ANSWER_CHANGE_REASON='the fix'\n"
        clean = {k: v for k, v in os.environ.items() if not k.startswith("EXPECTED_ANSWER_CHANGE")}
        for extra, want_status, want_rc in ((listed, "changed", 0), ("", "differ", 1), (listed, "changed", 0),
                                            ("EXPECTED_ANSWER_CHANGE=\nEXPECTED_ANSWER_CHANGE_REASON=\n", "differ", 1)):
            (d / "run.env").write_text(base + extra)
            r = subprocess.run(["bash", str(d / "nsc-bench-remote.sh")], capture_output=True, text=True,
                               env=clean, timeout=120)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual((d / "results" / "done").read_text().strip(), "ok")
            self.assertEqual((d / "results" / "expected-answer-change.txt").exists(), bool(extra == listed), extra)
            subprocess.run([sys.executable, str(SCRIPTS / "nsc-bench-summary.py"), str(d)],
                           capture_output=True, check=True)
            self.assertEqual(json.loads((d / "summary.json").read_text())["answers"]["q"], want_status, extra)
            check = subprocess.run([sys.executable, str(SCRIPTS / "nsc-bench-summary.py"), "--check",
                                    str(d / "summary.json"), "first"], capture_output=True, text=True)
            self.assertEqual(check.returncode, want_rc, extra + check.stdout + check.stderr)

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
    def test_an_expected_answer_change_decides_for_the_listed_queries_only(self):
        why = "the fix returns the rows the old join lost"
        every = ",".join(sorted(CONTROLS + ["tiny"]))
        r, s = self.stage("--expected-answer-change", every, "--expected-answer-change-reason", why,
                          tips=(("fixed", 1.0, "2"),))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        r = kit("run", s, env=self.env, timeout=600)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        verdict = (s / "verdict.md").read_text()
        self.assertIn("fixed (B) against base (A): first pass valid", verdict)
        self.assertIn(f"answers changed as run.env expects for {', '.join(sorted(CONTROLS + ['tiny']))} ({why})", verdict)
        p1 = json.loads((s / "pass1" / "summary-A-B.json").read_text())
        self.assertEqual(set(p1["answers"].values()), {"changed"})
        self.assertEqual(p1["expected_answer_change"]["reason"], why)
        # with only tiny listed, the controls' other answers still stop the pass
        r, s = self.stage("--expected-answer-change", "tiny", "--expected-answer-change-reason", why,
                          tips=(("fixed", 1.0, "2"),), name="one")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        r = kit("run", s, env=self.env, timeout=600)
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        verdict = (s / "verdict.md").read_text()
        self.assertIn("fixed (B) against base (A): first pass INVALID", verdict)
        self.assertIn("answers differ for " + ", ".join(sorted(CONTROLS)), verdict)
        self.assertIn(f"answers changed as run.env expects for tiny ({why})", verdict)

    @unittest.skipUnless(all(shutil.which(t) for t in NEEDS) and len(os.sched_getaffinity(0)) >= 2
                         and {0, 1} <= os.sched_getaffinity(0), "needs taskset, chrt, zstd, curl and CPUs 0 and 1")
    def test_the_busy_loop_that_enters_the_kernel_is_recorded_and_runs(self):
        r, s = self.stage("--spin-loop", "syscall", tips=(("tip", 1.0),))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        r = kit("run", s, env=self.env, timeout=600)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("busy loops: syscall (while :; do : > /dev/null; done)",
                      (s / "pass1" / "results" / "env.txt").read_text())
        self.assertIn("Busy loops: syscall.", (s / "verdict.md").read_text())

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


def write_samples(d, ratio, rounds=8, noise=None, controls=CONTROLS, query="q", spread=0.0, answers=None,
                  wall_noise=None, schedule=True):
    """A pass of A,B,B,A rounds of 2 requests, where B takes ratio times A's 1 ms on query,
    with noise[k] scaling B in round k, and B is as fast as A on the controls. With spread,
    the second server process of each variant in a round is that much slower on every query.
    Wall time is execution time plus 1 ms, and wall_noise[k] scales B's wall time on query in
    round k on top of that. Both variants give the same answer to every query, unless answers
    gives answers.tsv. With schedule, the pass's run.env gives its rounds, as run_pass writes it."""
    res = Path(d) / "results"
    res.mkdir(parents=True)
    if schedule:
        (Path(d) / "run.env").write_text(f"ROUNDS={rounds}\nREPS=2\n")
    (res / "queries.txt").write_text("".join(f"{q}\n" for q in [query, *controls]))
    (res / "answers.tsv").write_text(answers if answers is not None else
                                     "".join(f"{v}\t{q}\t1\tx\n" for v in "AB" for q in [query, *controls]))
    lines = ["round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms"]
    epoch = 0
    for k in range(1, rounds + 1):
        for i, v in enumerate("ABBA"):
            epoch += 1
            slow = 1 + spread * (i >= 2)
            for q in [query, *controls]:
                ms = (ratio * (noise[k - 1] if noise else 1) if v == "B" and q == query else 1.0) * slow
                wall = (ms + 1) * (wall_noise[k - 1] if wall_noise and v == "B" and q == query else 1)
                for rep in (1, 2):
                    lines.append(f"{k}\t{epoch}\t{v}\t{q}\t{rep}\t{ms}\t{wall}")
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

    def test_shadow_stops_a_query_only_once_every_scored_metric_has_stopped(self):
        # execution time stays flat, and B's wall time is 40% slower from round 4, so the
        # first three rounds put both metrics inside the bound, while all rounds flag wall
        # time. The shadow must report that the stop loses the wall flag.
        late = [1.0, 1.0, 1.0, 1.4, 1.4, 1.4, 1.4, 1.4]
        s = self.kit.shadow(write_samples(self.d / "wall-late", 1.0, wall_noise=late), ("A", "B"), CONTROLS, 1.10)
        q = s["queries"]["q"]
        self.assertEqual(q["metrics"]["exec_ms bound"]["flagged_all_rounds"], False)
        self.assertEqual((q["metrics"]["wall_ms bound"]["flagged"], q["metrics"]["wall_ms bound"]["flagged_all_rounds"]),
                         (False, True))
        self.assertFalse(s["agree"])
        self.assertEqual(s["flags_differ"], ["q"])
        self.assertEqual(s["decisions_differ"], ["q wall_ms bound"])
        # wall time that swings around 1 keeps its interval wide, so the query runs on past
        # the round at which execution time alone would stop it, and both decisions hold
        swing = [1.0, 1.08, 0.94, 1.07, 0.95, 1.0, 1.06, 0.96]
        s = self.kit.shadow(write_samples(self.d / "wall-swing", 1.0, wall_noise=swing), ("A", "B"), CONTROLS, 1.10)
        q = s["queries"]["q"]
        self.assertEqual(q["metrics"]["exec_ms bound"]["stop_round"], 3)
        self.assertGreater(q["metrics"]["wall_ms bound"]["stop_round"], 3)
        self.assertEqual(q["stop_round"], q["metrics"]["wall_ms bound"]["stop_round"])
        self.assertTrue(s["agree"])
        # each metric's flag is taken on the rounds the query keeps, not at that metric's own stop
        for m in q["metrics"].values():
            self.assertEqual(m["flagged"], m["flagged_all_rounds"])
        self.assertEqual(s["rule"]["metrics"], ["exec_ms", "wall_ms"])

    def test_an_every_round_rule_uses_the_exact_stop(self):
        # every round of B is 2% slower but round 5, which is 1% faster. The interval would
        # stop at round 3 with every round so far above 1, which answers the every-round
        # question wrongly. The exact stop ends the query at round 5, where the answer is no.
        rounds = [1.0, 1.0, 1.0, 1.0, 0.97, 1.0, 1.0, 1.0]
        d = write_samples(self.d / "dip", 1.02, noise=rounds)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=("every-round",))
        q = s["queries"]["q"]
        self.assertEqual(q["stop_round"], 5)
        self.assertEqual(q["metrics"]["exec_ms every-round"]["how"], "exact")
        self.assertEqual(q["metrics"]["exec_ms every-round"]["flagged"], False)
        self.assertTrue(s["agree"])
        self.assertEqual(s["rule"]["rules"], ["every-round"])
        # with the bound rule as well, the later of the two stops holds
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=("bound", "every-round"))
        self.assertEqual(s["queries"]["q"]["stop_round"], 5)
        self.assertEqual(s["queries"]["q"]["metrics"]["exec_ms bound"]["stop_round"], 3)
        self.assertTrue(s["agree"])
        # a cost in every round runs every round and is flagged, since it adds 20 us
        s = self.kit.shadow(write_samples(self.d / "cost", 1.02), ("A", "B"), CONTROLS, 1.10, rules=("every-round",))
        q = s["queries"]["q"]
        self.assertEqual((q["stop_round"], q["metrics"]["exec_ms every-round"]["flagged"]), (8, True))
        self.assertTrue(s["agree"])
        # and a cost in every round that adds no more than 0.5 us is noise
        s = self.kit.shadow(write_samples(self.d / "tiny", 1.0004), ("A", "B"), CONTROLS, 1.10, rules=("every-round",))
        self.assertEqual(s["queries"]["q"]["metrics"]["exec_ms every-round"]["flagged"], False)
        with self.assertRaises(ValueError):
            self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=("median",))

    def test_an_every_round_rule_without_a_paired_round_decides_no(self):
        # A's requests to q fail in the even rounds and B's in the odd ones, so no round has
        # both, and the every-round question has no round to judge and cannot flag
        d = write_samples(self.d / "unpaired", 1.3)
        t = d / "results" / "samples.tsv"
        lines = t.read_text().splitlines()
        out = [lines[0]]
        for line in lines[1:]:
            x = line.split("\t")
            if x[3] == "q" and (x[2] == "A") == (int(x[0]) % 2 == 0):
                x[5] = x[6] = "error"
            out.append("\t".join(x))
        t.write_text("\n".join(out) + "\n")
        for rules in (("every-round",), ("bound", "every-round")):
            s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=rules)
            m = s["queries"]["q"]["metrics"]["exec_ms every-round"]
            self.assertEqual((m["how"], m["flagged"], m["flagged_all_rounds"], m["undecidable"]),
                             ("unjudged", False, False, True), rules)
            self.assertEqual(s["queries"]["q"]["stop_round"], 8)

    def unpair(self, d, rounds, variant, drop=False):
        """Fail variant's requests to q in rounds, or with drop leave q's rows out of them."""
        t = d / "results" / "samples.tsv"
        lines = t.read_text().splitlines()
        out = [lines[0]]
        for line in lines[1:]:
            x = line.split("\t")
            if x[3] == "q" and int(x[0]) in rounds and (drop or x[2] == variant):
                if drop:
                    continue
                x[5] = x[6] = "error"
            out.append("\t".join(x))
        t.write_text("\n".join(out) + "\n")
        return d

    def test_an_every_round_rule_cannot_claim_a_cost_with_an_unpaired_round(self):
        # B is 30% slower in every round that has both variants, but one scheduled round has
        # no usable pair: one variant's requests failed, or q has no rows in that round at
        # all. Every round above 1 is then unestablished, so neither metric may flag a cost,
        # and the record says the question cannot be decided. The bound rule's pooled flag
        # does not depend on every round and still flags.
        cases = [(f"{v}{k}", k, v, False) for v in "AB" for k in (1, 5, 8)] + [("dropped5", 5, None, True),
                                                                             ("dropped8", 8, None, True)]
        for name, k, v, drop in cases:
            d = self.unpair(write_samples(self.d / name, 1.3), {k}, v, drop)
            for rules in (("every-round",), ("bound", "every-round")):
                s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=rules)
                q = s["queries"]["q"]
                self.assertEqual((s["validity"]["all_rounds"], s["validity"]["shadow"]), (True, True), (name, rules))
                self.assertEqual(q["stop_round"], 8, (name, rules))
                for m in self.kit.STOP_METRICS:
                    dec = q["metrics"][f"{m} every-round"]
                    self.assertEqual((dec["how"], dec["flagged"], dec["flagged_all_rounds"], dec["undecidable"],
                                      dec["undecidable_all_rounds"]), ("unjudged", False, False, True, True),
                                     (name, rules, m))
                    if "bound" in rules:
                        dec = q["metrics"][f"{m} bound"]
                        self.assertEqual((dec["flagged"], dec["flagged_all_rounds"], dec["undecidable"]),
                                         (True, True, False), (name, rules, m))
                self.assertEqual(s["undecidable"], [f"q {m} every-round" for m in self.kit.STOP_METRICS], (name, rules))
                self.assertIn("cannot decide q exec_ms every-round, q wall_ms every-round", self.kit.shadow_line("x", s))
        # the stopping rule alone is unchanged: a bound-only shadow records no undecidable question
        s = self.kit.shadow(self.unpair(write_samples(self.d / "boundonly", 1.3), {5}, "B"), ("A", "B"), CONTROLS, 1.10)
        self.assertEqual((s["queries"]["q"]["metrics"]["exec_ms bound"]["flagged"], s["undecidable"]), (True, []))

    def test_an_every_round_rule_still_answers_no_beside_an_unpaired_round(self):
        # a round at or below 1 answers no whatever the other rounds hold, before or after the
        # round without a pair
        noise = [1.0, 1.0, 1.0, 1.0, 0.97, 1.0, 1.0, 1.0]
        for name, k, stop in (("before", 2, 8), ("after", 7, 5)):
            d = self.unpair(write_samples(self.d / name, 1.02, noise=noise, wall_noise=noise), {k}, "B")
            for rules in (("every-round",), ("bound", "every-round")):
                s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=rules)
                q = s["queries"]["q"]
                self.assertEqual(q["stop_round"], stop if rules == ("every-round",) else max(stop, q["stop_round"]))
                for m in self.kit.STOP_METRICS:
                    dec = q["metrics"][f"{m} every-round"]
                    self.assertEqual((dec["flagged"], dec["flagged_all_rounds"], dec["undecidable"],
                                      dec["undecidable_all_rounds"]), (False, False, False, False), (name, rules, m))
                self.assertEqual(s["undecidable"], [], (name, rules))
                self.assertTrue(s["agree"], (name, rules))

    def test_the_fallback_and_controls_carry_an_undecidable_every_round_question(self):
        # in the fallback band every decision returns to all rounds, undecidability with it
        d = self.unpair(write_samples(self.d / "band", 1.3, spread=0.025), {5}, "B")
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=("bound", "every-round"))
        self.assertTrue(s["validity"]["fallback"])
        for m in self.kit.STOP_METRICS:
            dec = s["queries"]["q"]["metrics"][f"{m} every-round"]
            self.assertEqual((dec["stop_round"], dec["flagged"], dec["undecidable"]), (8, False, True), m)
        self.assertEqual(s["undecidable"], [f"q {m} every-round" for m in self.kit.STOP_METRICS])
        # a control runs every round and is judged the same way
        d = self.unpair(write_samples(self.d / "ctl", 1.3), {5}, "B")
        s = self.kit.shadow(d, ("A", "B"), ["q", *CONTROLS], 1.10, rules=("every-round",))
        for m in self.kit.STOP_METRICS:
            dec = s["queries"]["q"]["metrics"][f"{m} every-round"]
            self.assertEqual((dec["how"], dec["flagged"], dec["undecidable"], dec["undecidable_all_rounds"]),
                             ("control", False, True, True), m)

    def drop_round(self, d, k):
        """Leave every row of round k out of the pass, as if the round had left no samples."""
        t = d / "results" / "samples.tsv"
        lines = t.read_text().splitlines()
        t.write_text("\n".join([lines[0]] + [line for line in lines[1:] if int(line.split("\t")[0]) != k]) + "\n")
        return d

    def assert_undecidable(self, s, what, how=None):
        q = s["queries"]["q"]
        self.assertEqual(q["rounds"], 8, what)
        for m in self.kit.STOP_METRICS:
            dec = q["metrics"][f"{m} every-round"]
            got = (dec["flagged"], dec["flagged_all_rounds"], dec["undecidable"], dec["undecidable_all_rounds"])
            self.assertEqual(got, (False, False, True, True), (what, m))
            if how:
                self.assertEqual(dec["how"], how, (what, m))
            if "bound" in s["rule"]["rules"]:
                dec = q["metrics"][f"{m} bound"]
                self.assertEqual((dec["flagged"], dec["flagged_all_rounds"], dec["undecidable"]), (True, True, False),
                                 (what, m))
        self.assertEqual(s["undecidable"], [f"q {m} every-round" for m in self.kit.STOP_METRICS], what)

    def test_an_every_round_rule_judges_every_scheduled_round_even_one_with_no_rows(self):
        # B is 30% slower in every round that has samples, but a scheduled round left no rows
        # for any query of the pair, inside the pass or at its end. The pass's run.env gives 8
        # rounds, so the every-round question cannot be decided on the 7 that remain, in the
        # ordinary shadow, in the fallback band and for a control, and the bound's pooled flag
        # still flags.
        for k in (5, 8):
            for rules in (("every-round",), ("bound", "every-round")):
                d = self.drop_round(write_samples(self.d / f"gap{k}-{len(rules)}", 1.3), k)
                s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=rules)
                self.assertEqual(s["queries"]["q"]["stop_round"], 8, (k, rules))
                self.assert_undecidable(s, (k, rules), "unjudged")
                d = self.drop_round(write_samples(self.d / f"band{k}-{len(rules)}", 1.3, spread=0.025), k)
                s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=rules)
                self.assertTrue(s["validity"]["fallback"], (k, rules))
                self.assert_undecidable(s, ("fallback", k, rules))
                d = self.drop_round(write_samples(self.d / f"ctl{k}-{len(rules)}", 1.3), k)
                s = self.kit.shadow(d, ("A", "B"), ["q", *CONTROLS], 1.10, rules=rules)
                self.assert_undecidable(s, ("control", k, rules), "control")
                self.assertEqual(s["queries"]["q"]["stop_round"], 8, ("control", k, rules))

    def test_an_every_round_rule_without_the_pass_schedule_cannot_claim_a_cost(self):
        # neither the pass nor its parent says how many rounds were scheduled, so a round
        # missing altogether cannot be ruled out and no cost is claimed, even on complete rounds
        for rules in (("every-round",), ("bound", "every-round")):
            d = write_samples(self.d / f"bare-{len(rules)}" / "pass1", 1.3, schedule=False)
            s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=rules)
            self.assert_undecidable(s, rules)
        # the schedule-free pass still answers no from a round at or below 1
        noise = [1.0, 1.0, 1.0, 1.0, 0.97, 1.0, 1.0, 1.0]
        d = write_samples(self.d / "bare-no" / "pass1", 1.02, noise=noise, wall_noise=noise, schedule=False)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10, rules=("every-round",))
        for m in self.kit.STOP_METRICS:
            dec = s["queries"]["q"]["metrics"][f"{m} every-round"]
            self.assertEqual((dec["flagged"], dec["undecidable"], dec["undecidable_all_rounds"]), (False, False, False), m)
        # and the parent's run.env gives the schedule when the pass has none
        (self.d / "bare-1" / "run.env").write_text("ROUNDS=8\n")
        s = self.kit.shadow(self.d / "bare-1" / "pass1", ("A", "B"), CONTROLS, 1.10, rules=("every-round",))
        self.assertEqual((s["queries"]["q"]["metrics"]["exec_ms every-round"]["flagged"], s["undecidable"]), (True, []))
        # a parent schedule longer than the samples counts its missing final rounds
        (self.d / "bare-1" / "run.env").write_text("ROUNDS=9\n")
        s = self.kit.shadow(self.d / "bare-1" / "pass1", ("A", "B"), CONTROLS, 1.10, rules=("every-round",))
        self.assertEqual((s["queries"]["q"]["rounds"], s["undecidable"]),
                         (9, [f"q {m} every-round" for m in self.kit.STOP_METRICS]))

    def test_shadow_command_takes_the_schedule_from_the_pass_or_its_parent(self):
        for where in ("pass", "parent"):
            for rules in ("every-round", "bound,every-round"):
                out = self.d / f"cli-{where}-{rules}"
                d = self.drop_round(write_samples(out / "pass1", 1.3, schedule=where == "pass"), 8)
                if where == "parent":
                    (out / "run.env").write_text("ROUNDS=8\n")
                r = kit("shadow", "--rules", rules, d)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("cannot decide q exec_ms every-round, q wall_ms every-round", r.stdout, (where, rules))
                self.assert_undecidable(json.loads((d / "shadow-A-B.json").read_text()), (where, rules))

    def test_shadow_falls_back_in_the_band_and_judges_validity_by_the_controls(self):
        d = write_samples(self.d / "band", 1.0, spread=0.025)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertTrue(s["validity"]["fallback"])
        self.assertEqual(s["request_share"], 1.0)
        self.assertTrue(s["agree"])
        # the fallback keeps the pair's identity and sends every metric back to all rounds
        self.assertEqual(s["pair"], {"A": "A", "B": "B"})
        for m in s["queries"]["q"]["metrics"].values():
            self.assertEqual(m["stop_round"], 8)
        d = write_samples(self.d / "noisy", 1.0, spread=0.05)
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertEqual((s["validity"]["all_rounds"], s["validity"]["shadow"], s["agree"]), (False, False, True))
        # without enough controls the shadow cannot be valid
        d = write_samples(self.d / "bare", 1.0, controls=["class-predicates"])
        s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
        self.assertEqual((s["validity"]["all_rounds"], s["validity"]["shadow"], s["agree"]), (True, False, False))

    def test_shadow_is_invalid_wherever_the_pass_cannot_decide(self):
        same = "".join(f"{v}\t{q}\t1\tx\n" for v in "AB" for q in ["q", *CONTROLS])
        cases = {"wrong answers": write_samples(self.d / "wrong", 1.0, answers=same.replace("B\tq\t1\tx", "B\tq\t2\ty")),
                 "no answers": write_samples(self.d / "none", 1.0, answers="")}
        # a required query that left no sample
        d = write_samples(self.d / "absent", 1.0)
        (d / "results" / "queries.txt").write_text("q\nr\n" + "".join(f"{c}\n" for c in CONTROLS))
        (d / "results" / "answers.tsv").write_text(same + "A\tr\t1\tx\nB\tr\t1\tx\n")
        cases["absent query"] = d
        # B has no timings of q
        d = write_samples(self.d / "nob", 1.0)
        t = d / "results" / "samples.tsv"
        t.write_text("".join(line + "\n" for line in t.read_text().splitlines() if "\tB\tq\t" not in line))
        cases["no B timings"] = d
        # a short query whose B timings come from one server process
        d = write_samples(self.d / "thin", 1.0, query="tiny")
        t = d / "results" / "samples.tsv"
        lines = t.read_text().splitlines()
        t.write_text("".join(line.replace("\t1.0\t2.0", "\t0.1\t1.1") + "\n" for line in lines
                             if "\ttiny\t" not in line or "\tB\t" not in line or line.split("\t")[1] == "2"))
        cases["one process"] = d
        for name, d in cases.items():
            s = self.kit.shadow(d, ("A", "B"), CONTROLS, 1.10)
            v = s["validity"]
            self.assertEqual((v["all_rounds"], v["shadow"], s["agree"]), (False, False, True), name)
            self.assertTrue(v["pass_problems"], name)
            # the pass's own files are left as they were
            self.assertFalse(list(d.glob("summary*")), name)

    def test_shadow_command_reads_a_screen_pass(self):
        d = write_samples(self.d / "out" / "pass1", 1.0, schedule=False)
        (self.d / "out" / "run.env").write_text("ROUNDS=8\nFLAG_RATIO=1.20\n")
        r = kit("shadow", d)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("B against A: agrees with all rounds", r.stdout)
        s = json.loads((d / "shadow-A-B.json").read_text())
        self.assertEqual(s["rule"]["bound"], 1.20)
        self.assertEqual(s["controls"], CONTROLS)
        self.assertEqual(s["rule"]["rules"], ["bound"])
        # the questions come from run.env, and --rules overrides them
        (self.d / "out" / "run.env").write_text("ROUNDS=8\nFLAG_RATIO=1.20\nEARLY_STOP_RULES=bound,every-round\n")
        self.assertEqual(kit("shadow", d).returncode, 0)
        self.assertEqual(json.loads((d / "shadow-A-B.json").read_text())["rule"]["rules"], ["bound", "every-round"])
        self.assertEqual(kit("shadow", "--rules", "every-round", d).returncode, 0)
        self.assertEqual(json.loads((d / "shadow-A-B.json").read_text())["rule"]["rules"], ["every-round"])


if __name__ == "__main__":
    unittest.main()
