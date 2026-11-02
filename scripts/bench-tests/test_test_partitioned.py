"""Exercise how scripts/test-partitioned.sh stops a remote share before it releases the
share's directory and lock."""
import os
import signal
import subprocess
import tempfile
import threading
import time
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/test-partitioned.sh"


def functions(*names):
    """The bash source of the named top-level functions of the script."""
    lines, out, inside = SCRIPT.read_text().splitlines(), [], False
    for line in lines:
        if any(line.startswith(f"{n}() {{") for n in names):
            inside = True
        if inside:
            out.append(line)
            if line == "}":
                inside = False
    return "\n".join(out)


def alive(pgid):
    try:
        os.killpg(pgid, 0)
        return True
    except ProcessLookupError:
        return False


class StopShare(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "checkout"
        self.share = self.root / ".artifacts/test-share"
        self.share.mkdir(parents=True)
        (self.root / ".sparkles-test-share").touch()
        Path(f"{self.root}.share-lock").mkdir()
        self.groups = []

    def tearDown(self):
        for g in self.groups:
            try:
                os.killpg(g, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.tmp.cleanup()

    def start(self, body):
        """Start a share stand-in as its own process group, as run.sh is, and wait until it
        has recorded its pid."""
        (self.share / "launched").touch()
        p = subprocess.Popen(["bash", "-c", f"echo $$ > {self.share}/pid; {body}"], start_new_session=True,
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.groups.append(p.pid)
        # A remote share's parent reaps it. Here the test does, or the zombie leader would
        # keep the group alive.
        threading.Thread(target=p.wait, daemon=True).start()
        while not (self.share / "pid").exists() or not (self.share / "pid").read_text().strip():
            time.sleep(0.05)
        return p

    def stop(self):
        program = subprocess.run(["bash", "-c", functions("stop_share") + "\nstop_share"],
                                 env=dict(os.environ, root=str(self.root), keep="", STOP_SHARE_WAIT="2"),
                                 check=True, capture_output=True, text=True).stdout
        return subprocess.run(["bash", "-s"], input=program, capture_output=True, text=True)

    def released(self):
        return not self.root.exists() and not Path(f"{self.root}.share-lock").exists()

    def test_a_child_that_outlives_the_leader_is_stopped_first(self):
        p = self.start("(trap '' TERM; while :; do sleep 0.2; done) & sleep 0.3; exit 0")
        while p.poll() is None:
            time.sleep(0.05)
        self.assertTrue(alive(p.pid), "the child keeps the group alive after the leader exits")
        r = self.stop()
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertFalse(alive(p.pid))
        self.assertTrue(self.released())

    def test_a_share_that_ignores_term_is_killed_before_the_release(self):
        p = self.start("trap '' TERM; while :; do sleep 0.2; done")
        r = self.stop()
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertFalse(alive(p.pid))
        self.assertTrue(self.released())

    def test_a_launched_share_without_a_pid_keeps_its_directory_and_lock(self):
        (self.share / "launched").touch()
        r = self.stop()
        self.assertEqual(r.returncode, 1)
        self.assertIn("recorded no process", r.stdout)
        self.assertTrue(self.root.exists())
        self.assertTrue(Path(f"{self.root}.share-lock").exists())

    def test_the_exit_cleanup_runs_the_program_under_bash_whatever_the_login_shell(self):
        p = self.start("trap '' TERM; while :; do sleep 0.2; done")
        out = Path(self.tmp.name) / "out"
        (out / "share-1").mkdir(parents=True)
        (out / "share-1/remote-locked").write_text("host-a\n")
        bin_dir = Path(self.tmp.name) / "bin"
        bin_dir.mkdir()
        # An ssh whose remote login shell runs nothing but `bash -s`, as a shell without
        # bash's `kill --` would fail the program's liveness checks.
        (bin_dir / "ssh").write_text('#!/usr/bin/env bash\nfor a; do :; done\n'
                                     '[ "$a" = "bash -s" ] || exit 97\nexec bash -s\n')
        (bin_dir / "ssh").chmod(0o755)
        (bin_dir / "nsc").write_text("#!/bin/sh\nexit 0\n")
        (bin_dir / "nsc").chmod(0o755)
        r = subprocess.run(["bash", "-c", functions("stop_share", "cleanup") + "\nssh_opts=()\ncleanup"],
                           env=dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                                    root=str(self.root), keep="", out=str(out), STOP_SHARE_WAIT="2"),
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stderr, "")
        self.assertFalse(alive(p.pid))
        self.assertTrue(self.released())
        self.assertFalse((out / "share-1/remote-locked").exists())


if __name__ == "__main__":
    unittest.main()
