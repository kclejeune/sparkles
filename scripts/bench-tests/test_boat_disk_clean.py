"""The warm worker's disk cleanup and the stop paths that run it; no provider operation runs."""
import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import process_checks
from pathlib import Path
import contextlib
import fcntl
import io
import json
import os
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch
from boat_fixtures import fixture
from test_boat_lifecycle import WatchIO
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'boat'))
import controller
import disk_clean
import job_environment as env

GIB = 2**30


class FakeDisk:
    """A 100 GiB filesystem whose used space each removal or gc lowers."""
    def __init__(self, used_gib):
        self.used = used_gib * GIB

    def statvfs(self, path):
        free = 100 * GIB - self.used
        return SimpleNamespace(f_blocks=100 * GIB // 4096, f_bfree=free // 4096, f_bavail=free // 4096, f_frsize=4096)


class FakeWorker:
    """systemctl, git and mbx on a worker, with the commands each call received."""
    def __init__(self, units=(), list_exit=0, disk=None, gc_frees=0, stop_frees=0):
        self.units = list(units); self.list_exit = list_exit; self.calls = []; self.disk = disk; self.gc_frees = gc_frees
        self.stop_frees = stop_frees; self.stopping = []

    def run(self, argv, env=None, seconds=60):
        self.calls.append((argv, env, seconds))
        if argv[:4] == ['systemctl', '--user', 'stop', '--no-block']:
            self.stopping = list(argv[4:])
            return 0, ''
        if argv[:2] == ['systemctl', '--user'] and self.stopping:
            # A queued stop ends one unit at each later list, as slow shutdowns would.
            self.units.remove(self.stopping.pop(0))
        if argv[:2] == ['systemctl', '--user']:
            return self.list_exit, ''.join(u + ' loaded active running x\n' for u in self.units)
        if argv[:2] == ['rm', '-rf']:
            import shutil; shutil.rmtree(argv[-1], ignore_errors=True); return 0, ''
        if argv == ['mbx', 'gc']:
            if self.disk:
                self.disk.used -= self.gc_frees * GIB
            return 0, ''
        return 0, ''


class Clean(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name) / 'home'
        for d in ['gate/repo.git', 'gate/wt-06ca04ef/src', 'gate/wt-ci-1234abcd/src', 'gate/wt-live/src', 'gate/out/06ca04ef',
                  'lanes/nsc-build-work/aaaa/src', '.local/share/mbx-cache/objects']:
            (self.home / d).mkdir(parents=True)
        (self.home / '.local/share/mbx-cache/objects/x').write_text('store entry')
        self.outside = Path(self.tmp.name) / 'outside'; (self.outside / 'keep').mkdir(parents=True)
        (self.home / 'gate/wt-link').symlink_to(self.outside)

    def clean(self, worker, disk, **kw):
        kw.setdefault('threshold', 90)
        return disk_clean.clean(str(self.home), run=worker.run, statvfs=disk.statvfs, now=lambda: 1000.0, **kw)

    def exists(self, rel):
        return (self.home / rel).exists()

    def test_below_the_threshold_nothing_runs(self):
        disk = FakeDisk(89); worker = FakeWorker(disk=disk)
        record = self.clean(worker, disk)
        self.assertFalse(record['ran']); self.assertEqual(record['before']['used_pct'], 89); self.assertEqual(record['after'], record['before'])
        self.assertEqual(worker.calls, []); self.assertTrue(self.exists('gate/wt-06ca04ef'))

    def test_at_the_threshold_finished_work_goes_and_live_work_stays(self):
        disk = FakeDisk(90); worker = FakeWorker(units=['gate-live.service', 'boat-owned-warm-qual-x.service'], disk=disk, gc_frees=20)
        record = self.clean(worker, disk, env={'MBX_CACHE_DIR': str(self.home / '.local/share/mbx-cache')})
        self.assertTrue(record['ran']); self.assertEqual(record['before']['used_pct'], 90); self.assertEqual(record['after']['used_pct'], 70)
        removed = {(r['kind'], Path(r['path']).name) for r in record['removed']}
        self.assertEqual(removed, {('gate-worktree', 'wt-06ca04ef'), ('gate-worktree', 'wt-ci-1234abcd')})
        # A build moves its sources into its checkout before its scope is listed, so build
        # checkouts always stay.
        self.assertTrue(self.exists('lanes/nsc-build-work/aaaa/src'))
        self.assertFalse(self.exists('gate/wt-06ca04ef')); self.assertTrue(self.exists('gate/wt-live/src'))
        # Results, the bare repository, the store and anything behind a link stay.
        self.assertTrue(self.exists('gate/out/06ca04ef')); self.assertTrue(self.exists('gate/repo.git'))
        self.assertTrue(self.exists('.local/share/mbx-cache/objects/x')); self.assertTrue((self.outside / 'keep').exists())
        self.assertTrue((self.home / 'gate/wt-link').is_symlink())
        gc = [c for c in worker.calls if c[0] == ['mbx', 'gc']]
        self.assertEqual(len(gc), 1); self.assertEqual(gc[0][1]['MBX_CACHE_DIR'], str(self.home / '.local/share/mbx-cache'))
        # gc runs after the removals, so it can collect their managed targets.
        self.assertEqual(worker.calls[-1][0], ['mbx', 'gc'])

    def test_the_deadline_bounds_every_command_and_stops_the_removals(self):
        disk = FakeDisk(95); t = [0.0]; worker = FakeWorker(disk=disk)
        real = worker.run
        def slow(argv, env=None, seconds=60):
            t[0] += 4; return real(argv, env, seconds)
        worker.run = slow
        record = self.clean(worker, disk, deadline_seconds=10, clock=lambda: t[0])
        # Each command took 4 seconds. The unit list and one removal left 2 seconds, so the
        # second removal ran with that share and nothing started after the deadline.
        self.assertTrue(all(c[2] <= 10 for c in worker.calls))
        self.assertEqual([c[0][:2] for c in worker.calls][:3], [['systemctl', '--user'], ['rm', '-rf'], ['rm', '-rf']])
        self.assertEqual([c[2] for c in worker.calls][:3], [10, 6, 2])
        self.assertTrue(record['deadline_reached'] or all(c[2] == 0 for c in worker.calls[3:]))
        record = self.clean(FakeWorker(disk=disk), disk, deadline_seconds=1, clock=lambda: 5.0)
        self.assertTrue(record['ran'])

    def test_a_command_past_its_share_is_killed_with_its_children(self):
        import time
        start = time.monotonic()
        with tempfile.NamedTemporaryFile() as pidfile:
            code, _ = disk_clean.run_command(['sh', '-c', 'sleep 30 & echo $! > %s; wait' % pidfile.name], None, 1)
            child = int(Path(pidfile.name).read_text())
        self.assertEqual(code, 'timeout'); self.assertLess(time.monotonic() - start, 10)
        if not process_checks.ended(child):
            self.fail('the background child outlived its process group')
        self.assertEqual(disk_clean.run_command(['true'], None, 0)[0], 'no_time')

    def test_a_gate_scope_or_service_keeps_its_worktree(self):
        for unit in ['gate-06ca04ef.service', 'gate-06ca04ef.scope']:
            with self.subTest(unit=unit):
                disk = FakeDisk(95); self.clean(FakeWorker(units=[unit], disk=disk), disk)
                self.assertTrue(self.exists('gate/wt-06ca04ef/src'))

    def test_units_that_cannot_be_listed_keep_every_worktree(self):
        disk = FakeDisk(95); worker = FakeWorker(list_exit=1, disk=disk)
        record = self.clean(worker, disk)
        self.assertEqual(record['removed'], []); self.assertFalse(record['units_listed'])
        self.assertTrue(self.exists('gate/wt-06ca04ef/src'))
        self.assertIn(['mbx', 'gc'], [c[0] for c in worker.calls])

    def test_force_cleans_below_the_threshold(self):
        disk = FakeDisk(40); record = self.clean(FakeWorker(disk=disk), disk, threshold=None, force=True)
        self.assertTrue(record['ran']); self.assertEqual(record['mode'], 'force'); self.assertFalse(self.exists('gate/wt-06ca04ef'))

    def test_a_second_cleanup_waits_for_none(self):
        disk = FakeDisk(95); (self.home / 'gate/.disk-clean.lock').touch()
        fd = os.open(self.home / 'gate/.disk-clean.lock', os.O_RDWR); self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_EX)
        worker = FakeWorker(disk=disk); record = self.clean(worker, disk)
        self.assertFalse(record['ran']); self.assertIn('skipped', record); self.assertEqual(worker.calls, [])
        self.assertTrue(self.exists('gate/wt-06ca04ef'))

    def test_the_program_runs_as_the_controller_sends_it(self):
        """The controller sends the file on stdin after an argv line, so it has no docstring there."""
        disk = FakeDisk(95); log = self.home / 'gate/disk-clean.jsonl'
        args = ['--home', str(self.home), '--if-above', '90', '--env', '{"MBX_CACHE_DIR":"/c"}', '--log', str(log)]
        source = "import sys\nsys.argv=['remote']+" + repr(args) + '\n' + (Path(disk_clean.__file__)).read_text()
        out = io.StringIO()
        started = []
        class Popen:
            def __init__(self, argv, env=None, **kw):
                started.append((argv, env, kw)); self.returncode = 0; self.pid = -1
            def communicate(self, timeout=None):
                return b'', None
        # The program installs its cancellation handlers, which must not stay in this test process.
        with patch('os.statvfs', disk.statvfs), patch('subprocess.Popen', Popen), patch('signal.signal') as handlers, contextlib.redirect_stdout(out):
            exec(compile(source, 'remote', 'exec'), {'__name__': '__main__'})
        record = json.loads(out.getvalue())
        self.assertTrue(record['ran']); self.assertEqual(json.loads(log.read_text()), record)
        argv, gc_env, kw = started[-1]
        self.assertEqual(argv, ['mbx', 'gc']); self.assertEqual(gc_env['MBX_CACHE_DIR'], '/c'); self.assertTrue(kw['start_new_session'])
        import signal
        self.assertEqual({c.args[0] for c in handlers.call_args_list}, {signal.SIGTERM, signal.SIGHUP, signal.SIGINT})

    def helper(self, extra_sleep_name):
        """Start the cleanup as a program whose mbx gc sleeps and writes its pid."""
        import subprocess, time
        bindir = Path(self.tmp.name) / 'bin'; bindir.mkdir(exist_ok=True); pidfile = Path(self.tmp.name) / extra_sleep_name
        (bindir / 'mbx').write_text('#!/bin/sh\necho $$ > %s\nexec sleep 60\n' % pidfile); (bindir / 'mbx').chmod(0o755)
        (bindir / 'systemctl').write_text('#!/bin/sh\nexit 0\n'); (bindir / 'systemctl').chmod(0o755)
        environ = dict(os.environ, PATH=str(bindir) + ':' + os.environ['PATH'])
        p = subprocess.Popen([sys.executable, '-I', disk_clean.__file__, '--home', str(self.home), '--force', '--deadline-seconds', '120'],
                             env=environ, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            if pidfile.exists() and pidfile.read_text().strip():
                return p, int(pidfile.read_text())
            time.sleep(0.1)
        p.kill(); self.fail('mbx gc did not start')

    def gone(self, pid):
        if process_checks.ended(pid):
            return True
        os.kill(pid, 9); return False

    def test_a_cancelled_or_killed_cleanup_leaves_no_command(self):
        import signal
        for name, sig in [('term', signal.SIGTERM), ('hup', signal.SIGHUP), ('kill', signal.SIGKILL)]:
            with self.subTest(signal=name):
                p, child = self.helper(name)
                p.send_signal(sig); p.wait(10)
                self.assertTrue(self.gone(child), 'mbx gc outlived the cleanup')

    def test_a_command_whose_parent_died_before_its_setup_never_runs(self):
        """A cleanup that died between the fork and the death signal leaves a child of another parent."""
        marker = Path(self.tmp.name) / 'ran'
        with patch.object(disk_clean.os, 'getpid', return_value=1):
            code, _ = disk_clean.run_command(['sh', '-c', 'touch %s' % marker], None, 10)
        self.assertNotEqual(code, 0); self.assertFalse(marker.exists())
        # A death signal that cannot be set stops the command too.
        with patch.object(disk_clean, 'prctl', return_value=-1):
            code, _ = disk_clean.run_command(['sh', '-c', 'touch %s' % marker], None, 10)
        self.assertNotEqual(code, 0); self.assertFalse(marker.exists())
        code, _ = disk_clean.run_command(['sh', '-c', 'touch %s' % marker], None, 10)
        self.assertEqual(code, 0); self.assertTrue(marker.exists())

    def test_at_the_hard_limit_running_jobs_stop_and_then_their_work_goes(self):
        disk = FakeDisk(96)
        (self.home / 'gate/out/live').mkdir(); (self.home / 'gate/out/live/progress.log').write_text('queued\n')
        units = ['gate-live.service', 'gate-prep-06ca04ef.scope', 'boat-owned-warm-qual-x.service', 'sparkles-disk-clean.service']
        worker = FakeWorker(units=units, disk=disk, gc_frees=30)
        with patch.object(disk_clean.time, 'sleep', lambda s: None):
            record = self.clean(worker, disk, stop_above=95)
        # Every stop is requested in one call before any wait.
        stops = [c[0][4:] for c in worker.calls if c[0][:3] == ['systemctl', '--user', 'stop']]
        self.assertEqual(stops, [['gate-live.service', 'gate-prep-06ca04ef.scope']])
        self.assertEqual([u['unit'] for u in record['stopped']], stops[0]); self.assertTrue(record['stop_listed'])
        self.assertIn('stopped by the disk cleanup', (self.home / 'gate/out/live/progress.log').read_text())
        # The stops come before the unit list of the cleanup, which then takes the stopped jobs' worktrees.
        self.assertFalse(self.exists('gate/wt-live')); self.assertFalse(self.exists('gate/wt-06ca04ef'))
        self.assertTrue(self.exists('gate/out/live/progress.log')); self.assertEqual(record['after']['used_pct'], 66)
        self.assertIn('boat-owned-warm-qual-x.service', worker.units)

    def test_many_slow_jobs_all_get_their_stop_within_the_bound(self):
        # Each unit list takes 20 seconds and each pause what it asks; no job ever ends.
        disk = FakeDisk(97); units = ['gate-j%d.service' % i for i in range(5)]
        worker = FakeWorker(units=list(units), disk=disk); t = [0.0]; real = worker.run
        def slow_run(argv, env=None, seconds=60):
            if argv[:2] == ['systemctl', '--user'] and 'list-units' in argv:
                t[0] += min(20, seconds); worker.stopping = []
            return real(argv, env, seconds)
        worker.run = slow_run
        def pause(s): t[0] += s
        with patch.object(disk_clean.time, 'sleep', pause):
            record = self.clean(worker, disk, stop_above=95, deadline_seconds=240, clock=lambda: t[0])
        stops = [c[0][4:] for c in worker.calls if c[0][:3] == ['systemctl', '--user', 'stop']]
        self.assertEqual(stops, [units]); self.assertEqual(len(record['stopped']), 5)
        # The wait ended at its 90-second bound plus the first list, and the whole cleanup within its deadline.
        waits = [c[2] for c in worker.calls if 'list-units' in c[0]]
        self.assertTrue(all(w <= 90 for w in waits[1:]))
        self.assertLessEqual(t[0], 240)

    def test_under_the_hard_limit_nothing_stops(self):
        for used in [89, 92]:
            with self.subTest(used=used):
                disk = FakeDisk(used); worker = FakeWorker(units=['gate-live.service'], disk=disk)
                record = self.clean(worker, disk, stop_above=95)
                self.assertNotIn('stopped', record)
                self.assertFalse(any(c[0][:3] == ['systemctl', '--user', 'stop'] for c in worker.calls))
                self.assertTrue(self.exists('gate/wt-live/src'))

    def test_the_admission_check_cleans_only_when_short_and_reports_the_outcome(self):
        disk = FakeDisk(50); worker = FakeWorker(disk=disk)
        record = self.clean(worker, disk, threshold=None, need_free_bytes=40 * GIB)
        self.assertTrue(record['need_met']); self.assertFalse(record['ran']); self.assertEqual(worker.calls, [])
        disk = FakeDisk(70); worker = FakeWorker(disk=disk, gc_frees=15)
        record = self.clean(worker, disk, threshold=None, need_free_bytes=40 * GIB)
        self.assertTrue(record['ran']); self.assertTrue(record['need_met']); self.assertEqual(record['mode'], 'need-free')
        self.assertFalse(self.exists('gate/wt-06ca04ef'))
        disk = FakeDisk(80); worker = FakeWorker(disk=disk, gc_frees=5)
        record = self.clean(worker, disk, threshold=None, need_free_bytes=40 * GIB)
        self.assertTrue(record['ran']); self.assertFalse(record['need_met'])

    def test_a_short_disk_exits_3_and_logs_the_refusal(self):
        log = self.home / 'gate/disk-clean.jsonl'; real = disk_clean.clean
        def run(free_gib):
            disk = FakeDisk(100 - free_gib)
            with patch.object(disk_clean, 'clean', lambda *a, **k: real(*a, run=FakeWorker(disk=disk).run, statvfs=disk.statvfs, **k)), \
                    patch.object(disk_clean.signal, 'signal'), contextlib.redirect_stdout(io.StringIO()):
                try:
                    disk_clean.main(['--home', str(self.home), '--need-free-gib', '40', '--log', str(log)])
                    return 0
                except SystemExit as e:
                    return e.code
        self.assertEqual(run(60), 0); self.assertFalse(log.exists())
        self.assertEqual(run(30), 3); self.assertFalse(json.loads(log.read_text().splitlines()[-1])['need_met'])

    def test_the_command_line_refuses_bad_values(self):
        for argv in [[], ['--if-above', '0'], ['--if-above', '101'], ['--force', '--env', '["x"]'], ['--force', '--if-above', '90'],
                     ['--force', '--deadline-seconds', '0'], ['--force', '--stop-above', '95'], ['--if-above', '90', '--stop-above', '80'],
                     ['--if-above', '90', '--stop-above', '101'], ['--need-free-gib', '0'], ['--need-free-gib', '10', '--force']]:
            with self.subTest(argv=argv), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                disk_clean.main(['--home', str(self.home)] + argv)


class StopPaths(unittest.TestCase):
    """The closure watch and a finish clean the worker's disk before the stop, and never wait on it."""
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.f = fixture(Path(self.tmp.name)); controller.bind_release_profile(self.f['release']); self.op = self.f['operation']
        self.addCleanup(env.release_job_locks)

    def watch(self, io):
        return controller.closure_watch(self.f['release'], io, 900, (), observe=lambda r, x: None)

    def read(self, name):
        return json.loads((self.op / name).read_text())

    def test_the_watch_cleans_once_under_the_job_lock_before_the_final_check(self):
        io = WatchIO(self.f); seen = []
        def disk_clean(seconds):
            seen.append((seconds, io._jobs is not None, len(io.check_times), io.frozen))
            return {'observed_unix': io.wall(), 'before': {'used_pct': 91}, 'after': {'used_pct': 60}, 'ran': True, 'removed': [], 'gc_exit': 0}
        io.disk_clean = disk_clean
        self.assertEqual(self.watch(io), 'FINISHED')
        self.assertEqual(len(seen), 1); seconds, locked, checks, frozen = seen[0]
        self.assertLessEqual(seconds, controller.DISK_CLEAN_SECONDS); self.assertTrue(locked); self.assertEqual(checks, 1); self.assertFalse(frozen)
        row = self.read('closure-watch-disk-clean.json')
        self.assertTrue(row['cleaned']); self.assertEqual((row['before']['used_pct'], row['after']['used_pct']), (91, 60))
        self.assertEqual(len(io.check_times), 2); self.assertTrue(io.exists('stop-trigger.json'))

    def test_a_failed_cleanup_is_recorded_and_the_stop_goes_ahead(self):
        io = WatchIO(self.f)
        def disk_clean(seconds): raise OSError('worker unreachable')
        io.disk_clean = disk_clean
        self.assertEqual(self.watch(io), 'FINISHED')
        row = self.read('closure-watch-disk-clean.json'); self.assertFalse(row['cleaned']); self.assertIn('reason_code', row)
        self.assertTrue(io.exists('root-job-closed.json'))

    def test_the_bound_counts_from_after_the_check_and_leaves_the_freeze_wait(self):
        for spent, expect in [(400, 'clean'), (600, 'no_time')]:
            with self.subTest(spent=spent):
                self.setUp(); io = WatchIO(self.f); seen = []
                io.on_check = lambda n, io=io, spent=spent: io.wait(spent) if n == 1 else None
                io.disk_clean = lambda seconds, seen=seen: seen.append(seconds) or {'ran': True}
                self.assertEqual(self.watch(io), 'FINISHED')
                row = self.read('closure-watch-disk-clean.json')
                if expect == 'clean':
                    self.assertEqual(len(seen), 1)
                    self.assertLessEqual(seen[0] + controller.WATCH_FLOOR + controller.FREEZE_WAIT + controller.WATCH_DECISION, 900 - spent)
                else:
                    self.assertEqual(seen, []); self.assertEqual(row['reason_code'], 'disk_clean_no_time')

    def test_a_live_command_defers_the_cleanup(self):
        io = WatchIO(self.f, checks=['closed'] * 400); calls = []
        io.disk_clean = lambda seconds: calls.append(seconds) or {}
        env.hold_job_lock(self.op, 0.3)
        self.assertEqual(self.watch(io), 'UNRESOLVED_JOB_OPEN'); self.assertEqual(calls, [])
        self.assertFalse(io.exists('closure-watch-disk-clean.json'))

    def test_the_real_adapter_sends_the_sealed_program_with_the_job_environment(self):
        io = controller.RealIO(self.f['release'], self.op); sent = []
        def remote_python(program, args, seconds):
            sent.append((program, args, seconds)); return {'exit': 0, 'body': {'ran': True, 'before': {}, 'after': {}, 'removed': [], 'gc_exit': 0}}
        io.remote_python = remote_python
        io.disk_clean(240)
        program, args, seconds = sent[0]
        self.assertEqual(program, (Path(controller.__file__).with_name('disk_clean.py')).read_text())
        home = self.f['profile']['worker_home']
        self.assertEqual(args[:3], ['--home', home, '--force'])
        self.assertEqual(json.loads(args[args.index('--env') + 1]), env.environment(controller.profile()))
        self.assertEqual(args[args.index('--log') + 1], home + '/gate/disk-clean.jsonl')
        self.assertLess(int(args[args.index('--deadline-seconds') + 1]) + 15, seconds + 1); self.assertLess(seconds, 240)
        with self.assertRaisesRegex(Exception, 'disk clean bound exhausted'):
            io.disk_clean(20)


if __name__ == '__main__':
    unittest.main()
