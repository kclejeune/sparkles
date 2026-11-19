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
# FAKE_SHELL stands in for the login shell an ssh host runs the command in.
rx_exec() { shift; HOME=$FAKE_HOME PATH=$FAKE_BIN:$PATH ${FAKE_SHELL:-bash} -c "$1"; }
rx_admit() { echo "admit $*" >> "$FAKE_LOG"; echo '{}'; }
rx_runtime() { echo "runtime $*" >> "$FAKE_LOG"; echo "${FAKE_RUNTIME:-5000}"; }
rx_bundle() { echo "bundle $*" >> "$FAKE_LOG"; git -C "$FAKE_HOME/gate/repo.git" fetch -q "$FAKE_SRC" "$2"; }
# sync takes `-- RSYNC-ARGS SRC DEST` with SRC as HOST:PATH/ and copies a directory.
rx_sync() { local a=("$@"); local n=${#a[@]}; local src=${a[n-2]} dst=${a[n-1]}; echo "sync ${src}" >> "$FAKE_LOG"; cp -r "$FAKE_HOME/${src##*:}." "$dst"; }
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
# systemd-run itself succeeds once the unit has started, whatever the unit's exit.
env -i FAKE_LOG="$FAKE_LOG" FAKE_CI_EXIT="${FAKE_CI_EXIT:-0}" FAKE_CLEAN_EXIT="${FAKE_CLEAN_EXIT:-0}" "${envs[@]}" "$@" || true
'''

# df reports the KiB in ~/df-kib as free, about 950 GiB by default, as `df -Pk` or as
# gate.sh's `df -BG --output=avail`.
FAKE_DF = r'''#!/bin/sh
k=$(cat "$HOME/df-kib" 2> /dev/null || echo 1000000000)
case " $* " in *" --output=avail "*) echo Avail; echo "$((k / 1048576))G"; exit 0 ;; esac
echo "Filesystem 1024-blocks Used Available Capacity Mounted on"
echo "fake 2000000000 1 $k 1% /"
'''

# mbx gc records its gc.min_free_size and frees what ~/df-after-gc says.
FAKE_MBX = r'''#!/bin/sh
echo "mbx $* min_free=${MBX_GC_MIN_FREE_SIZE:-unset}" >> "$FAKE_LOG"
[ ! -f "$HOME/df-after-gc" ] || cp "$HOME/df-after-gc" "$HOME/df-kib"
'''

# git pushes to an ssh host's ~/gate/repo.git go to the fake home's repository.
FAKE_GIT = r'''#!/usr/bin/env bash
a=()
for x in "$@"; do case $x in ssh://*/gate/repo.git) a+=("$FAKE_HOME/gate/repo.git") ;; *) a+=("$x") ;; esac; done
exec REAL_GIT "${a[@]}"
'''

GIB = 1048576


class RemoteCI(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        t = Path(self.tmp.name); self.t = t
        # A source repository with the script under test, a fake ci.sh and premerge.sh.
        src = t / 'src'; (src / 'scripts/gate/remote').mkdir(parents=True)
        shutil.copy(ROOT / 'scripts/remote-ci.sh', src / 'scripts/remote-ci.sh')
        shutil.copy(ROOT / 'scripts/gate/remote/ci-job.sh', src / 'scripts/gate/remote/ci-job.sh')
        shutil.copy(ROOT / 'scripts/gate/config.sh', src / 'scripts/gate/config.sh')
        (src / 'scripts/remote-executor.sh').write_text(FAKE_RX)
        # With ~/ci-fills-disk the build leaves 1000 KiB free and keeps running.
        (src / 'scripts/ci.sh').write_text('#!/usr/bin/env bash\necho "ci $*"; echo "tmpdir $TMPDIR"; mkdir -p .artifacts/ci/logs; echo log > .artifacts/ci/logs/test.log\n'
                                           'if [ -f "$HOME/ci-fills-disk" ]; then echo 1000 > "$HOME/df-kib"; sleep 60; echo "survived the watch"; fi\nexit "${FAKE_CI_EXIT:-0}"\n')
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
        # The worker's admission checker, which setup-host installs; it finds room by default.
        (home / 'gate/bin/disk_clean.py').write_text('import sys\nopen(sys.argv[0] + ".args", "w").write(" ".join(sys.argv[1:]))\nsys.exit(int(__import__("os").environ.get("FAKE_CLEAN_EXIT", "0")))\n')
        self.home = home
        fake = t / 'bin'; fake.mkdir()
        (fake / 'systemd-run').write_text(FAKE_SYSTEMD_RUN)
        (fake / 'systemctl').write_text('#!/bin/sh\necho "systemctl $*" >> "$FAKE_LOG"\n[ "$2" = is-active ] && { echo inactive; exit 3; }\n[ "$2" = show ] && echo "${FAKE_STATE-inactive}"\nexit 0\n')
        (fake / 'mise').write_text('#!/bin/sh\nexit 0\n')
        (fake / 'df').write_text(FAKE_DF); (fake / 'mbx').write_text(FAKE_MBX)
        (fake / 'git').write_text(FAKE_GIT.replace('REAL_GIT', shutil.which('git')))
        (fake / 'flock').write_text('#!/bin/sh\necho "flock $*" >> "$FAKE_LOG"\n[ "$3" != "${FAKE_FLOCK_FAIL:-none}" ]\n')
        for f in fake.iterdir():
            f.chmod(0o755)
        self.log = t / 'log'
        self.env = dict(os.environ, FAKE_HOME=str(home), FAKE_BIN=str(fake), FAKE_SRC=str(src), FAKE_LOG=str(self.log),
                        PATH=str(fake) + ':' + os.environ['PATH'],
                        SPARKLES_GATE_CONFIG=str(t / 'no-gate.env'), SPARKLES_BENCH_HOST='forge')

    def run_ci(self, *args, **env):
        return subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh'), '--host', 'boat:bx_test', *args],
                              cwd=self.src, env=dict(self.env, **env), capture_output=True, text=True, timeout=120)

    def run_ssh(self, *args, **env):
        # An ssh host has no disk cleanup of its own.
        (self.home / 'gate/bin/disk_clean.py').unlink(missing_ok=True)
        return subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh'), '--host', 'wally', *args],
                              cwd=self.src, env=dict(self.env, **env), capture_output=True, text=True, timeout=120)

    def name(self):
        return 'ci-%s-%s' % (self.rev('HEAD')[:8], self.rev('origin/main')[:8])

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
        # The results came back through the directory sync, and the start held the start lock.
        self.assertIn('sync boat:bx_test:gate/out/%s/' % name, log)
        self.assertIn('flock -w 120 8', log); self.assertIn('prop TimeoutStopSec=60', log)
        self.assertTrue((self.src / '.artifacts/remote-ci' / name / 'ci/logs/test.log').exists())
        # The worktree was created under the cleanup's lock, after the bounded heavy-lock wait.
        self.assertIn('flock -w 300 7', log)
        w = [l for l in log.splitlines() if l.startswith('flock -w') and l.endswith(' 9')]
        self.assertEqual(len(w), 1); self.assertLessEqual(int(w[0].split()[2]), 5000 - 120)
        # The job removed its worktree and kept its results.
        self.assertFalse((self.home / 'gate' / ('wt-' + name)).exists())

    def test_a_boat_job_has_the_cap_that_qualification_admits(self):
        # The worker's own GATE_MEMORY_MAX does not raise a Boat job over the admitted job.
        r = self.run_ci('--base', 'origin/main', GATE_MEMORY_MAX='20G')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        log = self.log.read_text()
        self.assertIn('prop MemoryMax=12G', log); self.assertNotIn('MemoryMax=20G', log)

    @unittest.skipUnless(shutil.which('zsh'), 'zsh is not installed')
    def test_a_host_whose_login_shell_is_zsh_starts_the_job_and_forwards_its_settings(self):
        # An ssh host runs the start command in its login shell, which is zsh on wally.
        r = self.run_ci('--base', 'origin/main', FAKE_SHELL='zsh', MBX_REMOTE_URL='', RUSTUP_TOOLCHAIN='x y')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn('started gate-', r.stdout)
        name = 'ci-%s-%s' % (self.rev('HEAD')[:8], self.rev('origin/main')[:8])
        self.assertEqual((self.home / 'gate/out' / name / 'ci.exit').read_text().strip(), '0')

    def test_a_base_that_moved_on_uses_the_merge_base_the_tip_carries(self):
        g = ['git', '-C', str(self.src), '-c', 'user.name=t', '-c', 'user.email=t@t']
        mb = self.rev('origin/main')
        subprocess.run(g + ['checkout', '-q', '-b', 'side', 'origin/main'], check=True)
        subprocess.run(g + ['commit', '-q', '--allow-empty', '-m', 'main moved'], check=True)
        subprocess.run(g + ['update-ref', 'refs/remotes/origin/main', 'HEAD'], check=True)
        subprocess.run(g + ['checkout', '-q', '-'], check=True)
        r = self.run_ci('--base', 'origin/main')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        out = self.home / 'gate/out' / ('ci-%s-%s' % (self.rev('HEAD')[:8], mb[:8]))
        self.assertIn('ci --fast --changed ' + mb, (out / 'ci.log').read_text())

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

    def run_job(self, end, **env):
        # The job script alone, as its unit runs it.
        out = self.home / 'gate/out/job'
        e = dict(self.env, HOME=str(self.home), NAME='job', FULL=self.rev('HEAD'), BASE=self.rev('origin/main'),
                 MODE='ci', END=str(end), **env)
        subprocess.run(['git', '-C', str(self.home / 'gate/repo.git'), 'fetch', '-q', str(self.src), 'HEAD'], check=True)
        r = subprocess.run(['bash', str(self.src / 'scripts/gate/remote/ci-job.sh')], env=e, capture_output=True, text=True, timeout=60)
        return r, out

    def test_a_job_whose_end_has_passed_waits_for_no_lock(self):
        import time
        r, out = self.run_job(int(time.time()) + 60)
        self.assertEqual(r.returncode, 3)
        self.assertEqual((out / 'ci.exit').read_text().strip(), '3')
        log = self.log.read_text() if self.log.exists() else ''
        self.assertNotIn('flock', log); self.assertNotIn('stop --no-block', log)

    def test_a_heavy_lock_that_stays_held_ends_the_job_before_its_end(self):
        import time
        r, out = self.run_job(int(time.time()) + 3000, FAKE_FLOCK_FAIL='9')
        self.assertEqual(r.returncode, 3)
        self.assertIn('did not come free', (out / 'progress.log').read_text())
        self.assertFalse((self.home / 'gate/wt-job').exists())

    def test_the_job_ends_its_watchdog(self):
        import time
        r, out = self.run_job(int(time.time()) + 3000)
        self.assertEqual(r.returncode, 0, r.stderr)
        # Had the watchdog outlived the job, subprocess.run would have waited on its pipe.
        self.assertNotIn('stop --no-block', self.log.read_text())

    def test_a_unit_still_shutting_down_keeps_its_results(self):
        name = 'ci-%s-%s' % (self.rev('HEAD')[:8], self.rev('origin/main')[:8])
        out = self.home / 'gate/out' / name; out.mkdir(parents=True); (out / 'progress.log').write_text('live\n')
        for st in ['deactivating', 'active', 'activating', '']:
            with self.subTest(state=st):
                r = self.run_ci('--no-wait', FAKE_STATE=st)
                self.assertEqual(r.returncode, 1, r.stderr); self.assertIn('must have stopped first', r.stderr)
                self.assertEqual((out / 'progress.log').read_text(), 'live\n')
        self.assertEqual(sorted(p.name for p in (self.home / 'gate/out').iterdir()), [name])
        self.assertNotIn('unit gate-', self.log.read_text())

    def test_a_disk_without_room_for_the_build_refuses_the_job(self):
        clean = self.home / 'gate/bin/disk_clean.py'
        r = self.run_ci(FAKE_CLEAN_EXIT='3', REMOTE_CI_MIN_FREE_GIB='120')
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        name = 'ci-%s-%s' % (self.rev('HEAD')[:8], self.rev('origin/main')[:8])
        out = self.home / 'gate/out' / name
        self.assertIn('under 120 GiB free', (out / 'progress.log').read_text())
        self.assertIn('--need-free-gib 120', Path(str(clean) + '.args').read_text())
        self.assertFalse((out / 'ci.log').exists()); self.assertFalse((self.home / 'gate' / ('wt-' + name)).exists())
        self.assertEqual(self.run_ci('--no-wait', REMOTE_CI_MIN_FREE_GIB='x').returncode, 2)

    def test_an_ssh_host_with_room_runs_the_job_without_collecting(self):
        r = self.run_ssh()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        log = self.log.read_text(); out = self.home / 'gate/out' / self.name()
        self.assertNotIn('admit', log); self.assertNotIn('mbx gc', log)
        self.assertIn('prop MemoryMax=13G', log); self.assertIn('sync wally:gate/out/', log)
        self.assertFalse((out / 'outcome').exists())
        # The run's temporary files live in its worktree, which the job removes.
        self.assertIn('tmpdir %s/gate/wt-%s/.artifacts/tmp' % (self.home, self.name()), (out / 'ci.log').read_text())

    def test_an_ssh_host_short_of_the_floor_collects_the_shortfall_and_runs(self):
        (self.home / 'df-kib').write_text(str(40 * GIB)); (self.home / 'df-after-gc').write_text(str(90 * GIB))
        r = self.run_ssh()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn('mbx gc min_free=85GiB', self.log.read_text())
        self.assertIn('40 GiB free and the job needs 85 GiB', (self.home / 'gate/out' / self.name() / 'progress.log').read_text())

    def test_an_ssh_host_that_stays_short_of_the_floor_refuses_the_job(self):
        (self.home / 'df-kib').write_text(str(100 * GIB)); (self.home / 'df-after-gc').write_text(str(110 * GIB))
        r = self.run_ssh(REMOTE_CI_MIN_FREE_GIB='120')
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertIn('host disk below floor', r.stderr)
        out = self.home / 'gate/out' / self.name()
        self.assertIn('mbx gc min_free=120GiB', self.log.read_text())
        self.assertIn('110 GiB free after mbx gc, under the 120 GiB floor', (out / 'progress.log').read_text())
        self.assertEqual((out / 'outcome').read_text().strip(), 'disk-below-floor')
        self.assertFalse((out / 'ci.log').exists()); self.assertFalse((self.home / 'gate' / ('wt-' + self.name())).exists())

    def test_an_unreadable_disk_counts_as_full(self):
        (self.home / 'df-kib').write_text('')
        r = self.run_ssh()
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertIn('mbx gc', self.log.read_text())

    def check_the_disk_watch(self, run):
        (self.home / 'ci-fills-disk').touch()
        r = run(REMOTE_CI_WATCH_SECONDS='1')
        self.assertEqual(r.returncode, 5, r.stdout + r.stderr)
        self.assertIn('host disk below floor', r.stderr)
        out = self.home / 'gate/out' / self.name()
        self.assertEqual((out / 'outcome').read_text().strip(), 'disk-below-floor')
        self.assertEqual((out / 'ci.exit').read_text().strip(), '5')
        self.assertIn('0 MiB free during the run, under the 4 GiB floor', (out / 'progress.log').read_text())
        self.assertNotIn('survived the watch', (out / 'ci.log').read_text())
        self.assertFalse((self.home / 'gate' / ('wt-' + self.name())).exists())

    def test_a_disk_that_fills_during_the_run_on_an_ssh_host_stops_it(self):
        self.check_the_disk_watch(self.run_ssh)

    def test_a_disk_that_fills_during_the_run_on_a_boat_worker_stops_it(self):
        self.check_the_disk_watch(self.run_ci)

    def test_the_disk_settings_must_be_positive_numbers(self):
        for n in ['REMOTE_CI_MIN_FREE_GIB', 'REMOTE_CI_RUN_FLOOR_GIB', 'REMOTE_CI_WATCH_SECONDS']:
            for v in ['x', '0', '-1', '08']:
                with self.subTest(setting=n, value=v):
                    r = self.run_ci('--no-wait', **{n: v})
                    self.assertEqual(r.returncode, 2); self.assertIn(n, r.stderr)

    def test_a_boat_job_without_the_checker_refuses(self):
        (self.home / 'gate/bin/disk_clean.py').unlink()
        r = self.run_ci()
        name = 'ci-%s-%s' % (self.rev('HEAD')[:8], self.rev('origin/main')[:8])
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertIn('no ~/gate/bin/disk_clean.py', (self.home / 'gate/out' / name / 'progress.log').read_text())

    def test_the_benchmark_host_never_runs_ci(self):
        for h in ['forge', 'user@forge.local']:
            with self.subTest(host=h):
                r = self.run_ci('--host', h)
                self.assertEqual(r.returncode, 2); self.assertIn('benchmark host', r.stderr)

    def test_the_command_line_refuses_bad_input(self):
        for args in [['--status', 'a b'], ['--bogus']]:
            with self.subTest(args=args):
                self.assertEqual(self.run_ci(*args).returncode, 2)
        r = subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh')], cwd=self.src, env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 2); self.assertIn('no host', r.stderr)



def shell_functions(path, *names):
    """The text of the named top-level shell functions of a script, in file order."""
    lines, picked, inside = path.read_text().splitlines(), [], False
    for line in lines:
        if any(line.startswith(n + '() {') for n in names):
            inside = True
        if inside:
            picked.append(line)
            if line == '}' or (line.startswith(tuple(n + '() {' for n in names)) and line.endswith('}')):
                inside = False
    return '\n'.join(picked) + '\n'


class GateDiskFloor(unittest.TestCase):
    """The ssh-host floor of scripts/gate/gate.sh and its remote gate-inner.sh."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        t = Path(self.tmp.name); self.t = t
        self.home = t / 'home'; (self.home / 'gate/out').mkdir(parents=True)
        fake = t / 'bin'; fake.mkdir()
        (fake / 'df').write_text(FAKE_DF); (fake / 'mbx').write_text(FAKE_MBX)
        for f in fake.iterdir():
            f.chmod(0o755)
        self.log = t / 'log'
        self.env = dict(os.environ, HOME=str(self.home), FAKE_LOG=str(self.log), PATH=str(fake) + ':' + os.environ['PATH'])

    def test_the_admission_check_is_the_same_in_lane_ci_and_the_gate(self):
        names = ('free_kib', 'ssh_admit')
        ci = shell_functions(ROOT / 'scripts/gate/remote/ci-job.sh', *names)
        self.assertIn('MBX_GC_MIN_FREE_SIZE=${need}GiB', ci)
        self.assertEqual(ci, shell_functions(ROOT / 'scripts/gate/remote/gate-inner.sh', *names))

    def test_an_ssh_gate_checks_its_floor_under_the_heavy_lock(self):
        inner = (ROOT / 'scripts/gate/remote/gate-inner.sh').read_text().splitlines()
        lock = inner.index('flock 9')
        check = next(i for i, l in enumerate(inner) if 'ssh_admit "${GATE_MIN_FREE_GIB:-110}"' in l)
        self.assertLess(lock, check); self.assertTrue(inner[check].startswith('if [ "${SPARKLES_REMOTE_EXECUTOR:-ssh}" = ssh ]'))
        self.assertEqual([l.strip() for l in inner[check + 1:check + 3]], ['echo disk-below-floor > "$out/outcome"', 'finish 4'])
        gate = (ROOT / 'scripts/gate/gate.sh').read_text()
        self.assertIn('*) if [ "$p" = conformance ]; then args+=" --min-free-gib $need_conf"; else args+=" --min-free-gib $need_ci"; fi ;;', gate)

    def admit(self, free, after, need):
        (self.home / 'df-kib').write_text(str(free * GIB)); (self.home / 'df-after-gc').write_text(str(after * GIB))
        program = 'out=$HOME/gate/out\nlog() { echo "$*" >> "$out/progress.log"; }\n' + \
            shell_functions(ROOT / 'scripts/gate/remote/gate-inner.sh', 'free_kib', 'ssh_admit') + 'ssh_admit %d 30\n' % need
        return subprocess.run(['bash', '-c', program], env=self.env, capture_output=True, text=True, timeout=60).returncode

    def test_the_admission_frees_the_shortfall_or_refuses(self):
        self.assertEqual(self.admit(200, 200, 110), 0); self.assertFalse(self.log.exists())
        self.assertEqual(self.admit(50, 120, 110), 0); self.assertIn('min_free=110GiB', self.log.read_text())
        self.assertEqual(self.admit(50, 100, 110), 1)
        self.assertIn('host disk below floor: 100 GiB free after mbx gc, under the 110 GiB floor', (self.home / 'gate/out/progress.log').read_text())

    def ensure(self, free, after, need):
        (self.home / 'df-kib').write_text(str(free * GIB)); (self.home / 'df-after-gc').write_text(str(after * GIB))
        program = 'die() { echo "gate.sh: $*" >&2; exit 2; }\nrx_exec() { shift; bash -c "$1"; }\n' + \
            shell_functions(ROOT / 'scripts/gate/gate.sh', 'free_gib', 'ensure_disk') + 'ensure_disk wally %d "ci half"\n' % need
        return subprocess.run(['bash', '-c', program], env=self.env, capture_output=True, text=True, timeout=60)

    def test_the_driver_collects_to_the_floor_before_it_starts(self):
        self.assertEqual(self.ensure(150, 150, 110).returncode, 0); self.assertFalse(self.log.exists())
        r = self.ensure(60, 115, 110)
        self.assertEqual(r.returncode, 0, r.stderr); self.assertIn('min_free=110GiB', self.log.read_text())
        r = self.ensure(60, 90, 110)
        self.assertEqual(r.returncode, 2); self.assertIn('90 GiB free after mbx gc', r.stderr)

    def test_hosts_reports_each_host_against_the_floors(self):
        src = self.t / 'src'; (src / 'scripts/gate').mkdir(parents=True)
        for f in ['gate/gate.sh', 'gate/config.sh']:
            shutil.copy(ROOT / 'scripts' / f, src / 'scripts' / f)
        # Each fake host answers its probe with the lock, units and free GiB in FAKE_PROBE_<host>.
        (src / 'scripts/remote-executor.sh').write_text('rx_exec() { local v="FAKE_PROBE_$1"; [ -n "${!v:-}" ] || return 255; echo "${!v}"; }\n')
        subprocess.run(['git', 'init', '-q', str(src)], check=True)
        r = subprocess.run(['bash', str(src / 'scripts/gate/gate.sh'), '--hosts'], cwd=src, capture_output=True, text=True, timeout=60,
                           env=dict(self.env, SPARKLES_GATE_CONFIG=str(self.t / 'none'), SPARKLES_BENCH_HOST='forge', GATE_HOSTS='h1 h2 h3 h4',
                                    GATE_NEED_CONF_GB='40', FAKE_PROBE_h1='free 0 150', FAKE_PROBE_h2='held 1 60', FAKE_PROBE_h3='free 0 20'))
        self.assertEqual(r.returncode, 0, r.stderr)
        lines = r.stdout.splitlines()
        self.assertIn('disk floors: 110 GiB for a whole gate, 40 GiB for a conformance half', lines)
        self.assertIn('h1     free 0 150 room-for-whole', lines); self.assertIn('h2     held 1 60 room-for-half', lines)
        self.assertIn('h3     free 0 20 below-floor', lines); self.assertIn('h4     unreachable', lines)
        r = subprocess.run(['bash', str(src / 'scripts/gate/gate.sh'), '--hosts'], cwd=src, capture_output=True, text=True, timeout=60,
                           env=dict(self.env, SPARKLES_GATE_CONFIG=str(self.t / 'none'), GATE_NEED_CI_GB='x'))
        self.assertEqual(r.returncode, 2); self.assertIn('bad GATE_NEED_CI_GB', r.stderr)


if __name__ == '__main__':
    unittest.main()
