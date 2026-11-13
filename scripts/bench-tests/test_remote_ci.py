"""scripts/remote-ci.sh and its job script against a fake host; no ssh or provider call runs."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

FAKE_RX = r'''
RX_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# The fake host is this machine with HOME set to the fake worker's home.
rx_exec() { shift; HOME=$FAKE_HOME PATH=$FAKE_BIN:$PATH bash -c "$1"; }
rx_admit() { echo "admit $*" >> "$FAKE_LOG"; echo '{}'; }
rx_runtime() { echo "runtime $*" >> "$FAKE_LOG"; echo "${FAKE_RUNTIME:-5000}"; }
rx_bundle() { echo "bundle $*" >> "$FAKE_LOG"; git -C "$FAKE_HOME/gate/repo.git" fetch -q "$FAKE_SRC" "$2"; git -C "$FAKE_HOME/gate/repo.git" fetch -q "$FAKE_SRC" "$3"; }
rx_sync() { :; }
rx_get() { cp -r "$FAKE_HOME/$2/." "$3/"; }
'''

# systemd-run runs the unit's command at once, with the --setenv values, and records its
# properties; systemctl reports every unit inactive.
FAKE_SYSTEMD_RUN = r'''#!/usr/bin/env bash
props=(); envs=()
while [ $# -gt 0 ]; do
  case $1 in
    --user | --collect | -q) shift ;;
    --unit) echo "unit $2" >> "$FAKE_LOG"; shift 2 ;;
    -p) props+=("$2"); shift 2 ;;
    --setenv=*) envs+=("${1#--setenv=}"); shift ;;
    *) break ;;
  esac
done
printf 'prop %s\n' "${props[@]}" >> "$FAKE_LOG"
env -i FAKE_LOG="$FAKE_LOG" FAKE_CI_EXIT="${FAKE_CI_EXIT:-0}" "${envs[@]}" "$@"
'''


class RemoteCI(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        t = Path(self.tmp.name); self.t = t
        # A source repository with the script under test, a fake ci.sh and premerge.sh.
        src = t / 'src'; (src / 'scripts/gate/remote').mkdir(parents=True)
        shutil.copy(ROOT / 'scripts/remote-ci.sh', src / 'scripts/remote-ci.sh')
        shutil.copy(ROOT / 'scripts/gate/remote/ci-job.sh', src / 'scripts/gate/remote/ci-job.sh')
        (src / 'scripts/remote-executor.sh').write_text(FAKE_RX)
        (src / 'scripts/ci.sh').write_text('#!/usr/bin/env bash\necho "ci $*"; mkdir -p .artifacts/ci/logs; echo log > .artifacts/ci/logs/test.log; exit "${FAKE_CI_EXIT:-0}"\n')
        (src / 'scripts/premerge.sh').write_text('#!/usr/bin/env bash\necho premerge; mkdir -p .artifacts/premerge; echo ok > .artifacts/premerge/doc-paths.log\n')
        for f in ['ci.sh', 'premerge.sh', 'remote-ci.sh']:
            (src / 'scripts' / f).chmod(0o755)
        g = ['git', '-C', str(src)]
        subprocess.run(['git', 'init', '-q', str(src)], check=True)
        subprocess.run(g + ['-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-q', '--allow-empty', '-m', 'base'], check=True)
        subprocess.run(g + ['update-ref', 'refs/remotes/origin/main', 'HEAD'], check=True)
        subprocess.run(g + ['add', '-A'], check=True)
        subprocess.run(g + ['-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-q', '-m', 'tip'], check=True)
        self.src = src
        home = t / 'home'; (home / 'gate/bin').mkdir(parents=True); (home / 'gate/out').mkdir()
        subprocess.run(['git', 'init', '-q', '--bare', str(home / 'gate/repo.git')], check=True)
        self.home = home
        fake = t / 'bin'; fake.mkdir()
        (fake / 'systemd-run').write_text(FAKE_SYSTEMD_RUN)
        (fake / 'systemctl').write_text('#!/bin/sh\n[ "$2" = is-active ] && { echo inactive; exit 3; }\nexit 0\n')
        (fake / 'mise').write_text('#!/bin/sh\nexit 0\n')
        (fake / 'flock').write_text('#!/bin/sh\necho flock >> "$FAKE_LOG"; exit 0\n')
        for f in fake.iterdir():
            f.chmod(0o755)
        self.log = t / 'log'
        self.env = dict(os.environ, FAKE_HOME=str(home), FAKE_BIN=str(fake), FAKE_SRC=str(src), FAKE_LOG=str(self.log),
                        PATH=str(fake) + ':' + os.environ['PATH'])

    def run_ci(self, *args, **env):
        return subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh'), '--host', 'boat:bx_test', *args],
                              cwd=self.src, env=dict(self.env, **env), capture_output=True, text=True, timeout=120)

    def rev(self, r):
        return subprocess.run(['git', '-C', str(self.src), 'rev-parse', r], capture_output=True, text=True, check=True).stdout.strip()

    def test_a_lane_ci_runs_in_its_unit_and_returns_its_results(self):
        r = self.run_ci('--base', 'origin/main')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        name = 'ci-%s-%s' % (self.rev('HEAD')[:8], self.rev('origin/main')[:8])
        log = self.log.read_text()
        self.assertIn('unit gate-' + name, log); self.assertIn('flock', log)
        self.assertIn('admit boat:bx_test --minimum 2400', log); self.assertIn('runtime', log)
        self.assertIn('--reserve 1200', log)
        rt = [l for l in log.splitlines() if l.startswith('prop RuntimeMaxSec=')]
        self.assertEqual(len(rt), 1); self.assertLessEqual(int(rt[0].split('=')[1]), 5000)
        self.assertIn('prop UMask=0022', log)
        out = self.home / 'gate/out' / name
        self.assertEqual((out / 'ci.exit').read_text().strip(), '0')
        self.assertIn('ci --fast --changed ' + self.rev('origin/main'), (out / 'ci.log').read_text())
        self.assertTrue((out / 'ci/logs/test.log').exists())
        # The job removed its worktree and kept its results.
        self.assertFalse((self.home / 'gate' / ('wt-' + name)).exists())

    def test_a_failing_ci_returns_its_exit_code(self):
        r = self.run_ci(FAKE_CI_EXIT='1')
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)

    def test_premerge_runs_the_premerge_script(self):
        r = self.run_ci('--premerge')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        out = self.home / 'gate/out' / ('pm-' + self.rev('HEAD')[:8])
        self.assertEqual((out / 'ci.log').read_text().strip(), 'premerge')
        self.assertTrue((out / 'premerge/doc-paths.log').exists())

    def test_too_little_time_refuses_the_start(self):
        r = self.run_ci('--no-wait', FAKE_RUNTIME='300')
        self.assertNotEqual(r.returncode, 0); self.assertIn('not starting', r.stderr)
        self.assertNotIn('unit gate-', self.log.read_text())

    def test_a_job_with_too_little_time_inside_its_unit_does_not_build(self):
        r = self.run_ci('--no-wait', FAKE_RUNTIME='700', REMOTE_CI_MIN_SECONDS='600')
        self.assertEqual(r.returncode, 0, r.stderr)
        r = self.run_ci('--no-wait', FAKE_RUNTIME='100', REMOTE_CI_MIN_SECONDS='50')
        name = 'ci-%s-%s' % (self.rev('HEAD')[:8], self.rev('origin/main')[:8])
        self.assertEqual((self.home / 'gate/out' / name / 'ci.exit').read_text().strip(), '3')

    def test_the_command_line_refuses_bad_input(self):
        for args in [['--status', 'a b'], ['--bogus']]:
            with self.subTest(args=args):
                self.assertEqual(self.run_ci(*args).returncode, 2)
        r = subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh')], cwd=self.src, env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 2); self.assertIn('no host', r.stderr)


if __name__ == '__main__':
    unittest.main()
