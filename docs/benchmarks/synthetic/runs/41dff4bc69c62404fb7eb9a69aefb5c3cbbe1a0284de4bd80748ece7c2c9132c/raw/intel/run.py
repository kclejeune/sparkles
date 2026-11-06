"""One fixed three-leg Intel diagnostic using the unchanged existing harness."""
import csv
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import time
import owned

ROOT = Path('/home/kclejeune/lanes/partial-memory-optional-feature-intel-v2')
UNIT = 'partial-memory-optional-feature-intel-v2.service'
def sha(p):
    with Path(p).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()
load = lambda p: json.loads(Path(p).read_text())
write = lambda p, value: Path(p).write_text(json.dumps(value, indent=2) + '\n')

def main():
    initial = time.monotonic()
    release = load(ROOT / 'release.json')
    assert release['released'] and release['host'] == socket.gethostname() == 'forge'
    assert type(release['start_unix']) is int and release['end_unix'] - release['start_unix'] == 2700
    assert release['start_unix'] <= time.time() < release['end_unix']
    deadline = initial + min(2670, release['end_unix'] - time.time() - 30)
    assert deadline - initial >= 2520
    assert release['start_unix'] <= release['released_at_unix'] < release['start_unix'] + 1
    assert release['root'] == str(ROOT) and release['unit'] == UNIT
    assert sha(ROOT / 'inputs.json') == release['inputs_sha256']
    assert sha(ROOT / 'launch-once.py') == release['launcher_sha256']
    assert sha(ROOT / 'parent-trim.json') == release['trim_receipt_sha256']
    trim = load(ROOT / 'parent-trim.json')
    assert trim['host'] == 'forge' and trim['root'] == str(ROOT) and trim['unit'] == UNIT
    assert trim['inputs_sha256'] == release['inputs_sha256']
    assert trim['launcher_sha256'] == release['launcher_sha256']
    assert trim['argv'] == ['/run/wrappers/bin/sudo', '-n', '/run/current-system/sw/bin/fstrim', '-v', '/']
    assert trim['exit'] == 0 and trim['timed_out'] is False and trim['cleanup']['remaining'] == []
    assert trim['cleanup']['leader_exit'] == 0 and trim['both_host_locks_held'] is True
    assert release['start_unix'] - 30 <= trim['start_unix'] <= trim['end_unix'] <= release['released_at_unix']
    assert 0 <= trim['end_monotonic'] - trim['start_monotonic'] <= 30
    assert sha(ROOT / 'parent-trim.stdout') == trim['stdout_sha256']
    assert sha(ROOT / 'parent-trim.stderr') == trim['stderr_sha256']
    inputs = load(ROOT / 'inputs.json')
    plan = load(ROOT / 'plan.json')
    group = Path('/proc/self/cgroup').read_text().split('::', 1)[1].strip()
    cg = Path('/sys/fs/cgroup') / group.lstrip('/')
    assert cg.name == UNIT and (cg / 'memory.max').read_text().strip() == str(12 << 30)
    assert (cg / 'memory.swap.max').read_text().strip() == str(2 << 30)
    owned.require_unit_idle(cg)
    owned.enable_subreaper()
    def interrupted(number, frame):
        raise SystemExit(128 + number)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    def counters():
        return {n: (cg / n).read_text() for n in ['memory.max', 'memory.swap.max', 'memory.peak', 'memory.swap.peak', 'memory.events', 'cgroup.procs']}
    def verify():
        for name, digest in inputs['files'].items():
            assert sha(ROOT / name) == digest, name
        for name, target in inputs.get('links', {}).items():
            assert os.readlink(ROOT / name) == target, name
        for name, digest in inputs['runtime'].items():
            assert sha(name) == digest, name
    def pressure_snapshot(label):
        # Current observations only. Ancestor cumulative counters are not history
        # for this invocation; raw values permit later boundary-delta diagnosis.
        assert label and '/' not in label and label not in ['.', '..']
        base = Path('/sys/fs/cgroup')
        fields = ['memory.current', 'memory.high', 'memory.max', 'memory.events',
                  'memory.events.local', 'memory.stat', 'memory.pressure']
        record = {'version': 1, 'kind': 'current-pressure-boundary', 'label': label,
                  'start_unix': time.time(), 'start_monotonic': time.monotonic(),
                  'cgroup': group, 'hierarchy': []}
        current = cg
        assert current.is_relative_to(base) and current != base
        while True:
            values = {}
            for name in fields:
                try:
                    values[name] = (current / name).read_text()
                except FileNotFoundError:
                    # Root memory controller files may be absent. A missing leaf
                    # or ancestor refuses capture rather than inventing zeroes.
                    if current != base:
                        raise
                    values[name] = None
            record['hierarchy'].append({'path': str(current), 'files': values})
            if current == base:
                break
            current = current.parent
        paths = ['/proc/meminfo', '/proc/pressure/memory', '/proc/net/sockstat',
                 '/proc/net/sockstat6', '/proc/sys/net/ipv4/tcp_mem',
                 '/proc/sys/net/ipv4/tcp_rmem', '/proc/sys/net/ipv4/tcp_wmem',
                 '/proc/sys/net/ipv4/tcp_moderate_rcvbuf',
                 '/proc/sys/net/core/rmem_default', '/proc/sys/net/core/rmem_max',
                 '/proc/sys/net/core/wmem_default', '/proc/sys/net/core/wmem_max',
                 '/proc/sys/net/core/optmem_max']
        record['proc'] = {name: Path(name).read_text() for name in paths}
        record['page_size'] = os.sysconf('SC_PAGE_SIZE')
        record['end_unix'] = time.time()
        record['end_monotonic'] = time.monotonic()
        with (out / 'pressure' / (label + '.json')).open('x') as stream:
            json.dump(record, stream, indent=2)
            stream.write('\n')
    def run_with_pressure(argv, cwd, cap, log):
        pressure_snapshot(log + '.before')
        try:
            run(argv, cwd, cap, log)
        finally:
            # run's original exact owned-group cleanup finishes before this read.
            pressure_snapshot(log + '.after')
    def run(argv, cwd, cap, log):
        assert time.monotonic() + cap + 120 < deadline
        process = None
        with (ROOT / log).open('x') as stdout:
            try:
                process = owned.own_process(subprocess.Popen(argv, cwd=cwd, stdout=stdout, stderr=subprocess.STDOUT, start_new_session=True))
                code = process.wait(timeout=cap - 5)
                write(ROOT / (log + '.exit.json'), {'argv': argv, 'exit': code, 'pid': process.pid})
            finally:
                if process is not None:
                    owned.cleanup_logged(process, ROOT / log)
        assert code == 0, log
        owned.require_unit_idle(cg)
    streams = []
    cleanup = []
    with owned.OutputOwner() as owner:
        out = owner.create(ROOT / 'operation')
        (out / 'pressure').mkdir(exist_ok=False)
        try:
            for name in ['.bench.lock', '.heavy.lock']:
                f = (Path.home() / 'lanes' / name).open('a')
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
                streams.append(f)
            verify()
            assert shutil.disk_usage(ROOT).free >= 20 << 30
            assert int(next(x.split()[1] for x in Path('/proc/meminfo').read_text().splitlines() if x.startswith('MemAvailable:'))) * 1024 >= 12 << 30
            assert set([2, 4]) <= os.sched_getaffinity(0)
            topo = Path('/sys/devices/system/cpu')
            assert (topo / 'cpu2/topology/core_id').read_text() != (topo / 'cpu4/topology/core_id').read_text()
            assert 'GenuineIntel' in Path('/proc/cpuinfo').read_text() and 'i5-13500' in Path('/proc/cpuinfo').read_text()
            with socket.socket() as s:
                s.bind(('127.0.0.1', 3999))
            power = {str(p): p.read_text() for p in topo.glob('cpu[24]/cpufreq/*') if p.name in ['scaling_governor', 'scaling_driver', 'energy_performance_preference', 'scaling_min_freq', 'scaling_max_freq']}
            properties = dict(line.split('=', 1) for line in subprocess.check_output(
                ['systemctl', '--user', 'show', UNIT, '-p', 'InvocationID', '-p', 'MainPID', '-p', 'ControlGroup'],
                text=True, timeout=10).splitlines())
            invocation = properties['InvocationID']
            assert len(invocation) == 32 and all(c in '0123456789abcdef' for c in invocation)
            assert int(properties['MainPID']) == os.getpid() and properties['ControlGroup'] == group
            write(out / 'admission.json', {'release': release, 'cgroup': group, 'invocation': invocation, 'pid': os.getpid(), 'counters': counters(), 'power': power})
            pressure_snapshot('admission')
            for leg, cap in zip(plan['ordered_legs'], [900, 900, 240]):
                directory = ROOT / leg['id']
                assert not (directory / 'results').exists()
                verify()
                run_with_pressure(['/run/current-system/sw/bin/bash', 'nsc-bench-remote.sh'], directory, cap, leg['id'] + '.log')
                run_with_pressure(['/etc/profiles/per-user/kclejeune/bin/python3', '-B', str(ROOT / 'nsc-bench-summary.py'), str(directory)], ROOT, 15, leg['id'] + '-summary.log')
                for variant in ['A', 'B']:
                    assert sha(directory / variant / 'sparkles') == plan['binaries'][leg[variant]]['sha256']
                summary = load(directory / 'summary.json')
                queries = set(plan['queries']['sha256']) if leg['queries'] == 'all30' else set(leg['queries'])
                assert set(summary['queries']) == queries
                assert all(row['exec_ms']['n_a'] == row['exec_ms']['n_b'] == 640 and row['wall_ms']['n_a'] == row['wall_ms']['n_b'] == 640 for row in summary['queries'].values())
                assert (directory / 'results/done').read_text().strip() == 'ok'
                answers = {}
                for variant, query, *fingerprint in csv.reader((directory / 'results/answers.tsv').open(), delimiter='\t'):
                    assert variant in ['A', 'B'] and query in queries and 'error' not in fingerprint
                    assert variant not in answers.setdefault(query, {})
                    answers[query][variant] = fingerprint
                assert set(answers) == queries and all(set(v) == {'A', 'B'} and v['A'] == v['B'] for v in answers.values())
                write(out / (leg['id'] + '-validated.json'), {'queries': len(queries), 'timed_requests': len(queries) * 1280, 'answers_equal': True, 'same_A_control': leg['A'] == leg['B']})
                assert all(int(x.split()[1]) == 0 for x in (cg / 'memory.events').read_text().splitlines())
            verify()
            write(out / 'terminal.json', {'all_three_legs_complete': True, 'timed_requests': 90880, 'warmup_requests': 6816, 'server_epochs': 96, 'automatic_push_approval': False, 'counters': counters()})
        finally:
            owned.cleanup_owned_unit(cg, cleanup)
            owned.require_unit_idle(cg)
            write(out / 'end-counters.json', {'counters': counters(), 'cleanup': cleanup, 'elapsed_seconds': time.monotonic() - initial})
            for f in streams:
                f.close()

if __name__ == '__main__':
    main()
