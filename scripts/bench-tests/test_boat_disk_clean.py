"""The warm worker's disk cleanup and the stop paths that run it; no provider operation runs."""
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
    def __init__(self, units=(), list_exit=0, disk=None, gc_frees=0):
        self.units = list(units); self.list_exit = list_exit; self.calls = []; self.disk = disk; self.gc_frees = gc_frees

    def run(self, argv, env=None, seconds=60):
        self.calls.append((argv, env, seconds))
        if argv[:2] == ['systemctl', '--user']:
            return self.list_exit, ''.join(u + ' loaded active running x\n' for u in self.units)
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
        self.assertEqual(removed, {('gate-worktree', 'wt-06ca04ef'), ('gate-worktree', 'wt-ci-1234abcd'), ('build-checkout', 'aaaa')})
        self.assertFalse(self.exists('gate/wt-06ca04ef')); self.assertTrue(self.exists('gate/wt-live/src'))
        # Results, the bare repository, the store and anything behind a link stay.
        self.assertTrue(self.exists('gate/out/06ca04ef')); self.assertTrue(self.exists('gate/repo.git'))
        self.assertTrue(self.exists('.local/share/mbx-cache/objects/x')); self.assertTrue((self.outside / 'keep').exists())
        self.assertTrue((self.home / 'gate/wt-link').is_symlink())
        gc = [c for c in worker.calls if c[0] == ['mbx', 'gc']]
        self.assertEqual(len(gc), 1); self.assertEqual(gc[0][1]['MBX_CACHE_DIR'], str(self.home / '.local/share/mbx-cache'))
        # gc runs after the removals, so it can collect their managed targets.
        self.assertEqual(worker.calls[-1][0], ['mbx', 'gc'])

    def test_a_running_build_keeps_every_build_checkout(self):
        disk = FakeDisk(95); worker = FakeWorker(units=['nsc-build-a1.scope'], disk=disk)
        record = self.clean(worker, disk)
        self.assertTrue(self.exists('lanes/nsc-build-work/aaaa/src'))
        self.assertNotIn('build-checkout', {r['kind'] for r in record['removed']})

    def test_a_gate_scope_or_service_keeps_its_worktree(self):
        for unit in ['gate-06ca04ef.service', 'gate-06ca04ef.scope']:
            with self.subTest(unit=unit):
                disk = FakeDisk(95); self.clean(FakeWorker(units=[unit], disk=disk), disk)
                self.assertTrue(self.exists('gate/wt-06ca04ef/src'))

    def test_units_that_cannot_be_listed_keep_every_worktree(self):
        disk = FakeDisk(95); worker = FakeWorker(list_exit=1, disk=disk)
        record = self.clean(worker, disk)
        self.assertEqual(record['removed'], []); self.assertFalse(record['units_listed'])
        self.assertTrue(self.exists('gate/wt-06ca04ef/src')); self.assertTrue(self.exists('lanes/nsc-build-work/aaaa/src'))
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
        with patch('os.statvfs', disk.statvfs), patch('subprocess.run') as run, contextlib.redirect_stdout(out):
            run.return_value = SimpleNamespace(returncode=0, stdout=b'')
            exec(compile(source, 'remote', 'exec'), {'__name__': '__main__'})
        record = json.loads(out.getvalue())
        self.assertTrue(record['ran']); self.assertEqual(json.loads(log.read_text()), record)
        self.assertEqual(run.call_args_list[-1].args[0], ['mbx', 'gc']); self.assertEqual(run.call_args_list[-1].kwargs['env']['MBX_CACHE_DIR'], '/c')

    def test_the_command_line_refuses_bad_values(self):
        for argv in [[], ['--if-above', '0'], ['--if-above', '101'], ['--force', '--env', '["x"]'], ['--force', '--if-above', '90']]:
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
        self.assertLess(int(args[args.index('--gc-seconds') + 1]), seconds); self.assertLess(seconds, 240)
        with self.assertRaisesRegex(Exception, 'disk clean bound exhausted'):
            io.disk_clean(20)


if __name__ == '__main__':
    unittest.main()
