"""Tests of scripts/timing-kit.py with a stub server in place of sparkles.

    python3 -m unittest discover -s scripts/bench-tests -p test_timing_kit.py
"""

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


def stub(d, identity, exec_ms):
    path = Path(d) / f"{identity}.bin"
    path.write_text(STUB.read_text().replace('IDENTITY = "stub"', f'IDENTITY = "{identity}"')
                    .replace("EXEC_MS = 1.0", f"EXEC_MS = {exec_ms}"))
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
        for label, ms in tips:
            args += ["--tip", f"{label}={stub(self.d, label, ms)}"]
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
                                   .replace("FLAG_RATIO=1.10", "FLAG_RATIO=0.9"))
        r = kit("preflight", s)
        self.assertIn("REPS must be a whole number of at least 1, not 0", r.stderr)
        self.assertIn("RERUN_ROUNDS must be a whole number of at least 2, not 1", r.stderr)
        self.assertIn("FLAG_RATIO must be a ratio above 1", r.stderr)

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
        verdict = (s / "verdict.md").read_text()
        self.assertIn("fast (B) against base (A): first pass valid", verdict)
        self.assertIn("flagged none, none slower after the rerun", verdict)
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


if __name__ == "__main__":
    unittest.main()
