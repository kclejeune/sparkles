import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import resource
import signal
import owned

root = Path.home() / 'lanes/partial-memory-optional-feature-intel-v2'
unit = 'partial-memory-optional-feature-intel-v2.service'
def sha(p):
    with Path(p).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()
assert socket.gethostname() == 'forge'
assert not any((root / name).exists() for name in ['release.json', 'operation', 'parent-admission.json', 'parent-trim.json', 'parent-trim.stdout', 'parent-trim.stderr', 'parent-launch.json'])
assert sha(root / 'inputs.json') == '3817d05084d400f1d130a6ded869cfbdf2d20b060460d3798ebe1db1ae20a7e2'
assert sha(root / 'run.py') == '206ef942ead20369e1327cb53b227da30392700da14040f8928a0f2aa8297978'
listing = subprocess.check_output(['systemctl', '--user', 'list-units', '--all', '--plain', '--no-legend', unit], text=True, timeout=10)
assert not listing.strip()
assert Path('/run/wrappers/bin/sudo').is_file() and Path('/run/current-system/sw/bin/fstrim').is_file()
with contextlib.ExitStack() as stack:
    for name in ['.bench.lock', '.heavy.lock']:
        f = stack.enter_context((Path.home() / 'lanes' / name).open('a'))
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    ps = subprocess.check_output(['ps', '-eo', 'pid=,comm=,pcpu=,rss='], text=True, timeout=10)
    heavy = {'rustc', 'cargo', 'rustdoc', 'mbx', 'java', 'sparkles', 'A', 'B', 'C', 'modeprobe', 'qlever-server', 'oxigraph', 'fluree', 'sparkles-server', 'Konclude'}
    assert not any(line.split()[1] in heavy for line in ps.splitlines() if len(line.split()) >= 2), ps
    available = int(next(x.split()[1] for x in Path('/proc/meminfo').read_text().splitlines() if x.startswith('MemAvailable:'))) * 1024
    assert available >= 12 << 30 and shutil.disk_usage(root).free >= 20 << 30
    assert set([2, 4]) <= os.sched_getaffinity(0)
    with socket.socket() as s:
        s.bind(('127.0.0.1', 3999))
    ephemeral = [int(x) for x in Path('/proc/sys/net/ipv4/ip_local_port_range').read_text().split()]
    assert 3999 < ephemeral[0]
    inputs = json.loads((root / 'inputs.json').read_text())
    for p, digest in inputs['runtime'].items():
        assert sha(p) == digest
    for name, digest in inputs['files'].items():
        assert sha(root / name) == digest, name
    for name, target in inputs.get('links', {}).items():
        assert os.readlink(root / name) == target, name
    admission = {'actual_host': socket.gethostname(), 'MemAvailable': available, 'disk_free': shutil.disk_usage(root).free,
        'process_comm_cpu_rss': ps, 'both_host_locks_free': True, 'port3999_bindable': True, 'ephemeral_range': ephemeral,
        'no_build_or_engine_started_before_this_release': True}
    (root / 'parent-admission.json').write_text(json.dumps(admission, indent=2) + '\n')
    # Authenticated ssh -A parent only. No agent socket is copied to the service.
    def counters():
        usage = resource.getrusage(resource.RUSAGE_CHILDREN)
        return {'MemAvailable_kB': int(next(x.split()[1] for x in Path('/proc/meminfo').read_text().splitlines() if x.startswith('MemAvailable:'))),
            'disk_free': shutil.disk_usage(root).free, 'children_user_seconds': usage.ru_utime,
            'children_system_seconds': usage.ru_stime, 'children_maxrss_kB': usage.ru_maxrss}
    trim_argv = ['/run/wrappers/bin/sudo', '-n', '/run/current-system/sw/bin/fstrim', '-v', '/']
    trim = {'host': socket.gethostname(), 'root': str(root), 'unit': unit, 'parent_pid': os.getpid(),
        'inputs_sha256': sha(root / 'inputs.json'), 'launcher_sha256': sha(root / 'launch-once.py'),
        'argv': trim_argv, 'both_host_locks_held': True, 'start_unix': time.time(),
        'start_monotonic': time.monotonic(), 'before_counters': counters(), 'exit': None, 'timed_out': False}
    trim_signals = []
    trim_interruptible = False
    def trim_interrupted(number, frame):
        global trim_interruptible
        trim_signals.append(number)
        if trim_interruptible:
            # Disable raising before unwinding: further signals defer through cleanup.
            trim_interruptible = False
            raise SystemExit(128 + number)
    for number in [signal.SIGTERM, signal.SIGINT, signal.SIGHUP]:
        signal.signal(number, trim_interrupted)
    process = None
    try:
        with (root / 'parent-trim.stdout').open('xb') as stdout, (root / 'parent-trim.stderr').open('xb') as stderr:
            try:
                if trim_signals:
                    raise SystemExit(128 + trim_signals[0])
                # Handlers defer until the newly returned child handle is retained.
                process = subprocess.Popen(trim_argv, cwd=root, stdout=stdout, stderr=stderr, start_new_session=True)
                owned.own_process(process)
                trim['pid'] = process.pid
                trim_interruptible = True
                if trim_signals:
                    trim_interruptible = False
                    raise SystemExit(128 + trim_signals[0])
                try:
                    trim['exit'] = process.wait(timeout=25)
                except subprocess.TimeoutExpired:
                    trim['timed_out'] = True
                    raise
                finally:
                    trim_interruptible = False
            finally:
                if process is not None:
                    trim['cleanup'] = owned.stop_owned(process)
                    trim['exit'] = process.returncode
        if trim_signals:
            raise SystemExit(128 + trim_signals[0])
    except BaseException as error:
        trim['failure'] = {'exception': type(error).__name__, 'message': str(error)}
        raise
    finally:
        trim_interruptible = False
        trim['interrupted_signals'] = trim_signals
        trim.update({'end_unix': time.time(), 'end_monotonic': time.monotonic(), 'after_counters': counters()})
        for name in ['stdout', 'stderr']:
            path = root / ('parent-trim.' + name)
            if path.exists():
                trim[name + '_sha256'] = sha(path)
        with (root / 'parent-trim.json').open('x') as f:
            json.dump(trim, f, indent=2)
            f.write('\n')
    # A signal during receipt finalization is deferred, retained and refuses release.
    trim_interruptible = True
    if trim_signals:
        trim_interruptible = False
        trim['failure'] = {'exception': 'SystemExit', 'message': str(128 + trim_signals[0])}
        (root / 'parent-trim.json').write_text(json.dumps(trim, indent=2) + '\n')
        raise SystemExit(128 + trim_signals[0])
    assert trim['exit'] == 0 and not trim['timed_out'] and trim['cleanup']['remaining'] == []
    assert trim['end_monotonic'] - trim['start_monotonic'] <= 30
    trim_receipt_sha256 = sha(root / 'parent-trim.json')
    launcher_sha256 = sha(root / 'launch-once.py')
    inputs_sha256 = sha(root / 'inputs.json')
    released_at = time.time()
    start = int(released_at)
    release = {'released': True, 'host': 'forge', 'root': str(root), 'unit': unit, 'start_unix': start, 'end_unix': start + 2700,
        'trim_receipt_sha256': trim_receipt_sha256, 'launcher_sha256': launcher_sha256,
        'inputs_sha256': inputs_sha256, 'root_plan_review': 'b22176a335abfb0d75fc29cc60f27ba82a916b7b3fcb7f027afe58cf43553fdd',
        'released_at_unix': released_at}
    with (root / 'release.json').open('x') as f:
        json.dump(release, f, indent=2)
        f.write('\n')
argv = ['systemd-run', '--user', '--unit=partial-memory-optional-feature-intel-v2', '--service-type=exec', '--remain-after-exit',
    '--working-directory=' + str(root), '-p', 'MemoryMax=12G', '-p', 'MemorySwapMax=2G', '-p', 'OOMPolicy=continue',
    '-p', 'RuntimeMaxSec=2670', '-p', 'TimeoutStopSec=30', '-p', 'KillMode=control-group',
    '-p', 'StandardOutput=append:' + str(root / 'outer.stdout'), '-p', 'StandardError=append:' + str(root / 'outer.stderr'),
    '/run/current-system/sw/bin/env', '-i', 'HOME=/home/kclejeune',
    'PATH=/run/wrappers/bin:/etc/profiles/per-user/kclejeune/bin:/run/current-system/sw/bin', 'LANG=C', 'LC_ALL=C',
    'TZ=UTC', 'USER=kclejeune', 'XDG_RUNTIME_DIR=/run/user/1000',
    '/etc/profiles/per-user/kclejeune/bin/python3', '-B', str(root / 'run.py')]
r = subprocess.run(argv, capture_output=True, text=True, timeout=15)
result = {'argv': argv, 'exit': r.returncode, 'stdout': r.stdout, 'stderr': r.stderr, 'release': release}
(root / 'parent-launch.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
raise SystemExit(r.returncode)
