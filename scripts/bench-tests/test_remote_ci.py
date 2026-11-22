"""scripts/remote-ci.sh and its job script against a fake host; no ssh or provider call runs."""
import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import hashlib
from pathlib import Path
import os
import shlex
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
# systemd-run itself succeeds once the unit has started, whatever the unit's exit. With
# FAKE_ASYNC the unit runs on after the start returns, as a real unit does.
[ -z "${FAKE_CGROUP:-}" ] || envs+=("CI_JOB_CGROUP=$FAKE_CGROUP")
if [ -n "${FAKE_ASYNC:-}" ]; then
  env -i FAKE_LOG="$FAKE_LOG" FAKE_CI_EXIT="${FAKE_CI_EXIT:-0}" FAKE_CLEAN_EXIT="${FAKE_CLEAN_EXIT:-0}" "${envs[@]}" "$@" > /dev/null 2>&1 < /dev/null &
  exit 0
fi
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
[ "$1" != --version ] || { echo "mbx 0.0.1"; exit 0; }
echo "mbx $* min_free=${MBX_GC_MIN_FREE_SIZE:-unset}" >> "$FAKE_LOG"
[ ! -f "$HOME/df-after-gc" ] || cp "$HOME/df-after-gc" "$HOME/df-kib"
'''

# git pushes to an ssh host's ~/gate/repo.git go to the fake home's repository.
FAKE_GIT = r'''#!/usr/bin/env bash
a=()
for x in "$@"; do case $x in ssh://*/gate/repo.git) a+=("$FAKE_HOME/gate/repo.git") ;; *) a+=("$x") ;; esac; done
exec REAL_GIT "${a[@]}"
'''

# Isolate the test from the machine's startup files. When requested by fake mise,
# emulate Bash builds that source a system profile even in a noninteractive shell.
FAKE_BASH = r'''#!/bin/sh
profile_path=${FAKE_BASH_PROFILE_PATH:-}
for arg; do
  case "$arg" in
    --noprofile) profile_path=; break ;;
    -c | --) break ;;
    -*) ;;
    *) break ;;
  esac
done
[ -z "$profile_path" ] || export PATH="$profile_path"
exec REAL_BASH --noprofile --norc "$@"
'''

FIXTURE_IDENTITY = dict(GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@t',
                        GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@t')

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
        # With ~/ci-fills-disk the build leaves 1000 KiB free and keeps running for the seconds
        # that the file names, 60 by default. With ~/ci-leaves-child it also starts a
        # descendant that ignores TERM, and with ~/ci-says-exit it writes an exit-like line
        # into progress.log and runs on for 3 seconds.
        (src / 'scripts/ci.sh').write_text('#!/usr/bin/env bash\necho "ci $*"; echo "tmpdir ${TMPDIR:-unset}"; mkdir -p .artifacts/ci/logs; echo log > .artifacts/ci/logs/test.log\n'
                                           'if [ -f "$HOME/ci-leaves-child" ]; then (trap "" TERM; exec sleep 300) & echo $! > "$HOME/child.pid"; fi\n'
                                           'if [ -f "$HOME/ci-replaces" ]; then echo another-start > "$HOME/gate/out/$NAME/run-id"; fi\n'
                                           'if [ -f "$HOME/ci-says-exit" ]; then echo "mbx check || exit 7" >> "$HOME/gate/out/$NAME/progress.log"; sleep 3; fi\n'
                                           'if [ -f "$HOME/ci-fills-disk" ]; then echo 1000 > "$HOME/df-kib"; sleep "$(cat "$HOME/ci-fills-disk" | grep . || echo 60)"; echo "survived the watch"; fi\nexit "${FAKE_CI_EXIT:-0}"\n')
        (src / '.gitignore').write_text('.artifacts/\n')
        (src / 'rust-toolchain.toml').write_text('[toolchain]\nchannel = "1.2.3"\n')
        (src / 'scripts/premerge.sh').write_text('#!/usr/bin/env bash\necho premerge; mkdir -p .artifacts/premerge; echo ok > .artifacts/premerge/doc-paths.log\n')
        for f in ['ci.sh', 'premerge.sh', 'remote-ci.sh']:
            (src / 'scripts' / f).chmod(0o755)
        g = ['git', '-C', str(src)]
        self.git_env = dict(os.environ, **FIXTURE_IDENTITY)
        subprocess.run(['git', 'init', '-q', str(src)], check=True)
        subprocess.run(g + ['commit', '--no-gpg-sign', '-q', '--allow-empty', '-m', 'base'],
                       env=self.git_env, check=True)
        subprocess.run(g + ['update-ref', 'refs/remotes/origin/main', 'HEAD'], check=True)
        subprocess.run(g + ['add', '-A'], check=True)
        subprocess.run(g + ['commit', '--no-gpg-sign', '-q', '-m', 'tip'], env=self.git_env, check=True)
        self.src = src
        home = t / 'home'; (home / 'gate/bin').mkdir(parents=True); (home / 'gate/out').mkdir()
        subprocess.run(['git', 'init', '-q', '--bare', str(home / 'gate/repo.git')], check=True)
        # The worker's admission checker, which setup-host installs; it finds room by default.
        (home / 'gate/bin/disk_clean.py').write_text('import sys\nopen(sys.argv[0] + ".args", "w").write(" ".join(sys.argv[1:]))\nsys.exit(int(__import__("os").environ.get("FAKE_CLEAN_EXIT", "0")))\n')
        self.home = home
        fake = t / 'bin'; fake.mkdir()
        (fake / 'systemd-run').write_text(FAKE_SYSTEMD_RUN)
        # Every unit is inactive, except that with FAKE_ASYNC a unit is active until its job is done.
        (fake / 'systemctl').write_text('#!/bin/sh\necho "systemctl $*" >> "$FAKE_LOG"\n'
                                        'if [ "$2" = is-active ] && [ -n "${FAKE_ASYNC:-}" ] && [ ! -f "$HOME/gate/out/${3#gate-}/done_at" ]; then echo active; exit 0; fi\n'
                                        '[ "$2" = is-active ] && { echo inactive; exit 3; }\n[ "$2" = show ] && echo "${FAKE_STATE-inactive}"\nexit 0\n')
        # mise exec runs its command, and every other mise command does nothing.
        (fake / 'mise').write_text('#!/bin/sh\nif [ "$1" = exec ]; then shift; [ "$1" != -- ] || shift; exec "$@"; fi\nexit 0\n')
        (fake / 'bash').write_text(FAKE_BASH.replace('REAL_BASH', shlex.quote(shutil.which('bash'))))
        (fake / 'rustc').write_text('#!/bin/sh\necho "rustc ${RUSTUP_TOOLCHAIN:-default} (fake)"\n')
        (fake / 'df').write_text(FAKE_DF); (fake / 'mbx').write_text(FAKE_MBX)
        (fake / 'git').write_text(FAKE_GIT.replace('REAL_GIT', shutil.which('git')))
        (fake / 'flock').write_text('#!/bin/sh\necho "flock $*" >> "$FAKE_LOG"\n[ "$3" != "${FAKE_FLOCK_FAIL:-none}" ]\n')
        for f in fake.iterdir():
            f.chmod(0o755)
        self.log = t / 'log'
        self.env = dict(os.environ, FAKE_HOME=str(home), FAKE_BIN=str(fake), FAKE_SRC=str(src), FAKE_LOG=str(self.log),
                        PATH=str(fake) + ':' + os.environ['PATH'],
                        SPARKLES_GATE_CONFIG=str(t / 'no-gate.env'), SPARKLES_BENCH_HOST='forge')
        self.env.pop('BASH_ENV', None)

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
        g = ['git', '-C', str(self.src)]
        mb = self.rev('origin/main')
        subprocess.run(g + ['checkout', '-q', '-b', 'side', 'origin/main'], check=True)
        subprocess.run(g + ['commit', '--no-gpg-sign', '-q', '--allow-empty', '-m', 'main moved'],
                       env=self.git_env, check=True)
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
        # A directory at the old fixed TMPDIR path is not this start's and stays.
        other = self.home / 'gate' / ('tmp-' + self.name()); other.mkdir(parents=True); (other / 'keep').write_text('x')
        r = self.run_ssh()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        log = self.log.read_text(); out = self.home / 'gate/out' / self.name()
        self.assertNotIn('admit', log); self.assertNotIn('mbx gc', log)
        self.assertIn('prop MemoryMax=13G', log); self.assertIn('sync wally:gate/out/', log)
        self.assertFalse((out / 'outcome').exists())
        # The run's temporary files live beside its worktree, and the job removes both.
        self.assertIn('tmpdir %s/gate/tmp-%s.' % (self.home, self.name()), (out / 'ci.log').read_text())
        self.assertEqual(list((self.home / 'gate').glob('tmp-*.*')), []); self.assertTrue((other / 'keep').exists())

    def test_an_ssh_host_short_of_the_floor_collects_the_shortfall_and_runs(self):
        (self.home / 'df-kib').write_text(str(40 * GIB)); (self.home / 'df-after-gc').write_text(str(90 * GIB))
        r = self.run_ssh()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn('mbx gc min_free=85GiB', self.log.read_text())
        self.assertIn('40 GiB free and the job needs 85 GiB', (self.home / 'gate/out' / self.name() / 'progress.log').read_text())

    def test_an_ssh_host_that_stays_short_of_the_floor_refuses_the_job(self):
        (self.home / 'df-kib').write_text(str(100 * GIB)); (self.home / 'df-after-gc').write_text(str(110 * GIB))
        other = self.home / 'gate' / ('tmp-' + self.name()); other.mkdir(parents=True); (other / 'keep').write_text('x')
        r = self.run_ssh(REMOTE_CI_MIN_FREE_GIB='120')
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertTrue((other / 'keep').exists())
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

    def test_a_boat_worker_keeps_its_own_disk_limit_and_temporary_directory(self):
        (self.home / 'ci-fills-disk').write_text('2')
        r = self.run_ci(REMOTE_CI_WATCH_SECONDS='1')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        out = self.home / 'gate/out' / self.name()
        self.assertFalse((out / 'outcome').exists())
        log = (out / 'ci.log').read_text()
        self.assertIn('survived the watch', log); self.assertIn('tmpdir unset', log)

    def test_a_descendant_that_ignores_the_stop_is_ended_before_the_cleanup(self):
        (self.home / 'ci-leaves-child').touch()
        (self.home / 'ci-fills-disk').touch()
        r = self.run_ssh(REMOTE_CI_WATCH_SECONDS='1')
        self.assertEqual(r.returncode, 5, r.stdout + r.stderr)
        pid = int((self.home / 'child.pid').read_text())
        self.assertFalse(Path('/proc/%d' % pid).exists() and Path('/proc/%d/stat' % pid).read_text().split(') ', 1)[1][0] != 'Z')
        self.assertIn('ended the processes the run left', (self.home / 'gate/out' / self.name() / 'progress.log').read_text())

    def test_a_run_in_its_cgroup_that_ended_cleanly_exits_0(self):
        # A stand-in cgroup whose run directory reads empty, and an rmdir that removes it as
        # the kernel does, so that finish() must not look at it again.
        cg = self.t / 'cg'; run = cg / 'run'; run.mkdir(parents=True)
        for f, text in [('cgroup.procs', ''), ('cgroup.kill', ''), ('cgroup.events', 'populated 0\nfrozen 0\n')]:
            (run / f).write_text(text)
        fake = self.t / 'bin'
        (fake / 'rmdir').write_text('#!/bin/sh\nrm -rf "$1"\n'); (fake / 'rmdir').chmod(0o755)
        r = self.run_ssh(FAKE_CGROUP=str(cg))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertFalse(run.exists())
        self.assertNotIn('could not be shown ended', (self.home / 'gate/out' / self.name() / 'progress.log').read_text())

    def test_a_disk_that_fills_just_before_a_failed_run_ends_marks_it(self):
        (self.home / 'ci-fills-disk').write_text('0')
        r = self.run_ssh(FAKE_CI_EXIT='1', REMOTE_CI_WATCH_SECONDS='600')
        self.assertEqual(r.returncode, 5, r.stdout + r.stderr)
        self.assertIn('host disk below floor', r.stderr)

    def test_the_wait_takes_only_the_exit_record_and_fetches_a_fresh_copy(self):
        name = self.name()
        res = self.src / '.artifacts/remote-ci' / name
        stale = res.with_name(name + '.earlier')
        stale.mkdir(parents=True)
        (stale / 'outcome').write_text('disk-below-floor\n')
        (stale / 'ci.exit').write_text('4\n')
        res.symlink_to(stale.name)
        (self.home / 'ci-says-exit').touch()
        r = self.run_ssh(FAKE_ASYNC='1', REMOTE_CI_POLL_SECONDS='1')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn('host disk below floor', r.stderr)
        self.assertEqual((res / 'ci.exit').read_text().strip(), '0')
        self.assertFalse((res / 'outcome').exists())
        self.assertEqual((stale / 'ci.exit').read_text().strip(), '4')

    def test_a_legacy_result_stays_in_place_when_mv_cannot_exchange_it(self):
        name = self.name()
        base = self.src / '.artifacts/remote-ci'
        earlier = base / name
        earlier.mkdir(parents=True)
        (earlier / 'ci.exit').write_text('0\n')
        (earlier / 'ci.log').write_text('earlier result\n')
        mv = self.t / 'bin/mv'
        mv.write_text('#!/bin/sh\n[ "$1" != --exchange ] || exit 1\nexec %s "$@"\n' % shlex.quote(shutil.which('mv')))
        mv.chmod(0o755)

        r = self.run_ssh(FAKE_CI_EXIT='7')
        self.assertEqual(r.returncode, 7, r.stdout + r.stderr)
        self.assertIn('could not point to the new copy', r.stderr)
        run_id = (self.home / 'gate/out' / name / 'run-id').read_text().strip()
        fresh = base / (name + '.' + run_id)
        self.assertEqual((fresh / 'ci.exit').read_text().strip(), '7')

        r = self.run_ssh('--fetch', name)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn('could not point to the new copy', r.stderr)
        self.assertFalse(earlier.is_symlink())
        self.assertEqual((earlier / 'ci.exit').read_text(), '0\n')
        self.assertEqual((earlier / 'ci.log').read_text(), 'earlier result\n')

    def test_a_legacy_result_is_archived_when_mv_can_exchange_it(self):
        name = self.name()
        base = self.src / '.artifacts/remote-ci'
        earlier = base / name
        earlier.mkdir(parents=True)
        (earlier / 'ci.exit').write_text('4\n')
        (earlier / 'ci.log').write_text('earlier result\n')
        # Probe on the result filesystem with the same directory and symlink types.
        directory = base / 'probe-dir'; directory.mkdir()
        link = base / 'probe-link'; link.symlink_to(earlier.name)
        probe = subprocess.run(['mv', '--exchange', '-T', str(link), str(directory)],
                               env=self.env, capture_output=True, timeout=10)
        if probe.returncode != 0:
            self.skipTest('mv cannot atomically exchange a symlink and a directory here')

        r = self.run_ssh()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertTrue(earlier.is_symlink(), r.stderr)
        self.assertEqual((earlier / 'ci.exit').read_text().strip(), '0')
        archived, = base.glob(name + '.kept-*')
        self.assertEqual((archived / 'ci.exit').read_text(), '4\n')
        self.assertEqual((archived / 'ci.log').read_text(), 'earlier result\n')

    def run_cmd(self, cmd, *args, **env):
        return self.run_ssh('--cmd', cmd, *args, **env)

    def test_a_command_runs_with_the_pinned_toolchain_and_returns_its_exit(self):
        r = self.run_cmd('echo "hello $RUSTUP_TOOLCHAIN"; test -f rust-toolchain.toml; exit 7')
        self.assertEqual(r.returncode, 7, r.stdout + r.stderr)
        name = 'cmd-%s-' % self.rev('HEAD')[:8]
        self.assertIn('gate-' + name, r.stdout)
        # The log comes back on stdout and names the compiler and the mbx it used.
        for line in ['remote-ci: echo "hello $RUSTUP_TOOLCHAIN"; test -f rust-toolchain.toml; exit 7',
                     'remote-ci: RUSTUP_TOOLCHAIN=1.2.3', 'remote-ci: rustc 1.2.3 (fake)', 'remote-ci: mbx 0.0.1', 'hello 1.2.3']:
            self.assertIn(line, r.stdout.splitlines())
        out = next((self.home / 'gate/out').glob(name + '*'))
        self.assertEqual((out / 'ci.exit').read_text().strip(), '7')
        self.assertFalse((self.home / 'gate' / ('wt-' + out.name)).exists())
        # Another command of the same commit is another job.
        r = self.run_cmd('true')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(len(list((self.home / 'gate/out').glob(name + '*'))), 2)

    def test_a_command_keeps_the_mise_path_during_bash_startup(self):
        tools = self.home / 'mise-bin'; tools.mkdir()
        mbx = tools / 'mbx'
        mbx.write_text('#!/bin/sh\necho mbx from mise\n')
        mbx.chmod(0o755)
        (self.home / 'bash-env').write_text('export PATH=%s\n' % shlex.quote(self.env['PATH']))
        mise = self.t / 'bin/mise'
        command = 'printf "shell %s with %s arguments\\n" "${0##*/}" "$#"; mbx --version; exit 7'
        for startup in ('export FAKE_BASH_PROFILE_PATH="$PATH"', 'export BASH_ENV="$HOME/bash-env"'):
            with self.subTest(startup=startup):
                mise.write_text('#!/bin/sh\n[ "$1" = exec ] || exit 0\nshift 2\n'
                                + startup + '\nexport PATH="$HOME/mise-bin:$PATH"\nexec "$@"\n')
                r = self.run_cmd(command)
                self.assertEqual(r.returncode, 7, r.stdout + r.stderr)
                self.assertIn('remote-ci: mbx from mise', r.stdout.splitlines())
                self.assertIn('mbx from mise', r.stdout.splitlines())
                self.assertIn('shell bash with 0 arguments', r.stdout.splitlines())

    def test_a_command_on_the_working_tree_sees_uncommitted_files_and_changes_nothing(self):
        (self.src / 'scripts/ci.sh').write_text('#!/bin/sh\necho edited\n')
        (self.src / 'new-file.txt').write_text('untracked\n')
        before = subprocess.run(['git', '-C', str(self.src), 'status', '--porcelain'], capture_output=True, text=True).stdout
        head = self.rev('HEAD')
        r = self.run_cmd('cat new-file.txt scripts/ci.sh', '--worktree', **FIXTURE_IDENTITY)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn('untracked', r.stdout.splitlines()); self.assertIn('echo edited', r.stdout.splitlines())
        self.assertEqual(self.rev('HEAD'), head)
        self.assertEqual(subprocess.run(['git', '-C', str(self.src), 'status', '--porcelain'], capture_output=True, text=True).stdout, before)
        self.assertEqual(subprocess.run(['git', '-C', str(self.src), 'stash', 'list'], capture_output=True, text=True).stdout, '')

    def test_a_command_job_keeps_the_disk_floor(self):
        (self.home / 'df-kib').write_text(str(10 * GIB))
        r = self.run_cmd('true')
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr); self.assertIn('host disk below floor', r.stderr)

    def test_the_wait_refuses_results_that_another_start_replaced(self):
        (self.home / 'ci-replaces').touch(); (self.home / 'ci-says-exit').touch()
        r = self.run_ssh(FAKE_ASYNC='1', REMOTE_CI_POLL_SECONDS='1')
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn('another start of gate-%s replaced this one' % self.name(), r.stderr)
        self.assertFalse((self.src / '.artifacts/remote-ci' / self.name()).exists())

    def test_the_results_carry_their_start_and_no_reserve(self):
        r = self.run_ssh()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        res = self.src / '.artifacts/remote-ci' / self.name()
        self.assertRegex((res / 'run-id').read_text().strip(), r'^[0-9]+-[0-9]+$')
        self.assertEqual(sorted(p.name for p in res.glob('.reserve*')), [])
        # The name is a link to the start's own copy, which no later fetch replaces.
        self.assertTrue(res.is_symlink())
        self.assertEqual(res.resolve().name, '%s.%s' % (self.name(), (res / 'run-id').read_text().strip()))

    def test_a_manual_fetch_that_cannot_publish_keeps_the_earlier_copy(self):
        r = self.run_ssh()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        res = self.src / '.artifacts/remote-ci' / self.name()
        # An mv that fails for the publication leaves the earlier copy where it was.
        fake = self.t / 'bin'
        (fake / 'mv').write_text('#!/usr/bin/env bash\nfor a in "$@"; do case $a in */.fetch-*) exit 1 ;; esac; done\nexec %s "$@"\n' % shutil.which('mv'))
        (fake / 'mv').chmod(0o755)
        r = subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh'), '--host', 'wally', '--fetch', self.name()],
                           cwd=self.src, env=self.env, capture_output=True, text=True, timeout=60)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual((res / 'ci.exit').read_text().strip(), '0')
        self.assertEqual(list((self.src / '.artifacts/remote-ci').glob('.fetch-*')), [])
        # A link swap that fails leaves the name pointing at the earlier copy.
        (fake / 'mv').write_text('#!/usr/bin/env bash\nfor a in "$@"; do case $a in */.link-*) exit 1 ;; esac; done\nexec %s "$@"\n' % shutil.which('mv'))
        before = res.resolve()
        r = subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh'), '--host', 'wally', '--fetch', self.name()],
                           cwd=self.src, env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 1, r.stderr); self.assertIn('could not point to the new copy', r.stderr)
        self.assertEqual(res.resolve(), before)
        # Both copies are there, and the new one is named in the output.
        new = [p for p in (self.src / '.artifacts/remote-ci').glob(self.name() + '.fetch-*')]
        self.assertEqual(len(new), 1); self.assertIn('results in %s' % new[0], r.stdout)
        # Without the fault a fetch by name gets a copy of its own, and the name follows it.
        (fake / 'mv').unlink()
        r = subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh'), '--host', 'wally', '--fetch', self.name()],
                           cwd=self.src, env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(res.is_symlink()); self.assertIn('.fetch-', res.resolve().name)
        self.assertTrue(before.exists())

    def test_a_publication_lock_that_cannot_be_opened_keeps_the_exit_and_the_earlier_copy(self):
        base = self.src / '.artifacts/remote-ci'
        name = 'cmd-%s-%s' % (self.rev('HEAD')[:8], hashlib.sha256(b'exit 3').hexdigest()[:8])
        (base / ('.lock-' + name)).mkdir(parents=True)
        # The waiter still returns the job's own exit status, read from its own copy.
        r = self.run_cmd('exit 3')
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        self.assertIn('could not open the publication lock', r.stderr)
        self.assertFalse((base / name).exists())
        self.assertEqual(len(list(base.glob(name + '.*'))), 1)
        # A fetch by name fails, since the name could not follow its new copy.
        r = subprocess.run(['bash', str(self.src / 'scripts/remote-ci.sh'), '--host', 'wally', '--fetch', name],
                           cwd=self.src, env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertFalse((base / name).exists())

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
        self.env.pop('BASH_ENV', None)

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

    def test_the_driver_only_reads_the_disk_and_warns_when_short(self):
        r = self.ensure(150, 150, 110)
        self.assertEqual(r.returncode, 0, r.stderr); self.assertEqual(r.stderr, '')
        r = self.ensure(60, 200, 110)
        self.assertEqual(r.returncode, 0, r.stderr); self.assertIn('wally has 60 GiB free and the ci half needs 110 GiB', r.stderr)
        # Another job may hold the host, so the driver never collects there.
        self.assertFalse(self.log.exists())

    def merge(self, ci, conf, ci_disk=False, conf_disk=False, fresh=True):
        """Merge a split gate whose halves exited ci and conf, with fake hosts p and h."""
        fake = self.t / 'hosts'
        for host, code, disk in (('p', ci, ci_disk), ('h', conf, conf_disk)) if fresh else ():
            out = fake / host / 'gate/out' / ('n' if host == 'p' else 'n-conf')
            shutil.rmtree(fake / host, ignore_errors=True); out.mkdir(parents=True)
            (out / 'part.exit').write_text('%d\n' % code); (out / 'steps.txt').write_text('%s step\n' % host)
            if disk:
                (out / 'outcome').write_text('disk-below-floor\n')
        dest = self.t / 'dest'; shutil.rmtree(dest, ignore_errors=True)
        (self.t / '.artifacts/gates').mkdir(parents=True, exist_ok=True)
        program = r'''
rx_exec() { (cd "$FAKE_HOSTS/$1" && HOME=$FAKE_HOSTS/$1 bash -c "systemctl() { return 3; }; $2"); }
rx_sync() {
  local a=("$@") n=${#a[@]} del=0 x
  for x in "$@"; do [ "$x" != --delete ] || del=1; done
  local src=${a[n-2]} dst=${a[n-1]}
  case $src in *:*) src=$FAKE_HOSTS/${src%%:*}/${src#*:} ;; esac
  case $dst in *:*) dst=$FAKE_HOSTS/${dst%%:*}/${dst#*:} ;; esac
  if [ -d "$src" ]; then mkdir -p "$dst"; [ "$del" = 0 ] || rm -rf "${dst:?}"/*; cp -a "$src/." "$dst/"; else cp -a "$src" "$dst"; fi
}
''' + shell_functions(ROOT / 'scripts/gate/gate.sh', 'wall', 'finished', 'fetch') + '''
primary=p helper=h name=n hname=n-conf pout=gate/out/n hout=gate/out/n-conf
fetch > /dev/null || exit 90
cat "$dest/gate.exit"
'''
        r = subprocess.run(['bash', '-c', program], capture_output=True, text=True, timeout=60,
                           env=dict(self.env, FAKE_HOSTS=str(fake), dest=str(dest), root=str(self.t)))
        self.assertEqual(r.returncode, 0, r.stderr)
        return int(r.stdout.strip()), dest

    def test_a_split_gate_keeps_a_part_s_disk_refusal(self):
        code, dest = self.merge(0, 4, conf_disk=True)
        self.assertEqual(code, 4)
        self.assertEqual((dest / 'gate-outcome').read_text().strip(), 'disk-below-floor')
        self.assertFalse((dest / 'outcome').exists())
        self.assertEqual((dest / 'outcome-host').read_text().strip(), 'conformance half on h')
        code, dest = self.merge(4, 0, ci_disk=True)
        self.assertEqual(code, 4); self.assertEqual((dest / 'outcome-host').read_text().strip(), 'ci half on p')
        code, dest = self.merge(4, 4, ci_disk=True, conf_disk=True)
        self.assertEqual(code, 4); self.assertEqual((dest / 'outcome-host').read_text().strip(), 'ci half on p, conformance half on h')
        # A part that failed its tests still fails the merge, whatever the other part's disk,
        # and a disk marker without exit 4 is not a refusal.
        self.assertEqual(self.merge(1, 4, conf_disk=True)[0], 1)
        self.assertEqual(self.merge(1, 0, ci_disk=True)[0], 1)
        self.assertEqual(self.merge(0, 0)[0], 0); self.assertEqual(self.merge(0, 1)[0], 1)

    def test_a_retried_merge_never_reads_an_earlier_merge_s_record_as_a_part_s(self):
        # A copy back that stopped before gate.exit left the merge's records on the primary.
        code, dest = self.merge(1, 4, conf_disk=True)
        self.assertEqual(code, 1)
        p = self.t / 'hosts/p/gate/out/n'
        for f in ['gate-outcome', 'outcome-host', 'outcome-conf']:
            (p / f).write_text('disk-below-floor\n')
        (p / 'gate.exit').unlink(missing_ok=True)
        code, dest = self.merge(1, 4, fresh=False)
        self.assertEqual(code, 1)
        self.assertFalse((dest / 'outcome').exists())
        self.assertEqual((dest / 'outcome-host').read_text().strip(), 'conformance half on h')

    def test_hosts_reports_each_host_against_the_floors(self):
        src = self.t / 'src'; (src / 'scripts/gate').mkdir(parents=True)
        for f in ['gate/gate.sh', 'gate/config.sh']:
            shutil.copy(ROOT / 'scripts' / f, src / 'scripts' / f)
        # Each fake host answers its probe with the lock, units and free GiB in FAKE_PROBE_<host>.
        (src / 'scripts/remote-executor.sh').write_text('rx_exec() { local v="FAKE_PROBE_$1"; [ -n "${!v:-}" ] || return 255; echo "${!v}"; }\n')
        subprocess.run(['git', 'init', '-q', str(src)], check=True)
        r = subprocess.run(['bash', str(src / 'scripts/gate/gate.sh'), '--hosts'], cwd=src, capture_output=True, text=True, timeout=60,
                           env=dict(self.env, SPARKLES_GATE_CONFIG=str(self.t / 'none'), SPARKLES_BENCH_HOST='forge', GATE_HOSTS='h1 h2 h3 h4 h5',
                                    GATE_NEED_CONF_GB='25', FAKE_PROBE_h1='free 0 150', FAKE_PROBE_h2='held 1 60', FAKE_PROBE_h3='free 0 20',
                                    FAKE_PROBE_h5='free 0 30'))
        self.assertEqual(r.returncode, 0, r.stderr)
        lines = r.stdout.splitlines()
        self.assertIn('disk floors: 110 GiB for a ci half or a whole gate and 25 GiB for a conformance half, met under the heavy lock; selection from 40 and 20 GiB free', lines)
        self.assertIn('h1     free 0 150 room-for-whole', lines); self.assertIn('h2     held 1 60 whole-after-gc room-for-half', lines)
        self.assertIn('h3     free 0 20 half-after-gc', lines); self.assertIn('h4     unreachable', lines)
        self.assertIn('h5     free 0 30 room-for-half', lines)
        # With the floors the other way round, the conformance half's need still shows.
        r = subprocess.run(['bash', str(src / 'scripts/gate/gate.sh'), '--hosts'], cwd=src, capture_output=True, text=True, timeout=60,
                           env=dict(self.env, SPARKLES_GATE_CONFIG=str(self.t / 'none'), SPARKLES_BENCH_HOST='forge', GATE_HOSTS='h1',
                                    GATE_NEED_CI_GB='25', GATE_NEED_CONF_GB='110', FAKE_PROBE_h1='free 0 60'))
        self.assertIn('h1     free 0 60 room-for-whole half-after-gc', r.stdout.splitlines(), r.stdout + r.stderr)
        r = subprocess.run(['bash', str(src / 'scripts/gate/gate.sh'), '--hosts'], cwd=src, capture_output=True, text=True, timeout=60,
                           env=dict(self.env, SPARKLES_GATE_CONFIG=str(self.t / 'none'), GATE_NEED_CI_GB='x'))
        self.assertEqual(r.returncode, 2); self.assertIn('bad GATE_NEED_CI_GB', r.stderr)


if __name__ == '__main__':
    unittest.main()
