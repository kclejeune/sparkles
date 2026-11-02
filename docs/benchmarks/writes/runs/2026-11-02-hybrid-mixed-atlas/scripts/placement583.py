#!/usr/bin/env python3
"""Item 449: query latency of Sparkles under controlled CPU placement on a hybrid host.

  placement.py OUT ROUNDS CONFIG... [--bin NAME=PATH ...]

Every round starts one server process per (binary, config) pair, in a rotated order. A
config names the server's CPU affinity and its RAYON_NUM_THREADS. Each process serves the
BSBM 10M store as `bench` and the DBpedia 50M slice as `dbp`, answers every query UNTIMED
times untimed and TIMED times timed, one request at a time over a new connection, and
records the client wall time, the server's own time from its access log and the run time
of every server thread from /proc/<pid>/task/*/schedstat, by the CPU type it last ran on.
The timer runs on CLIENT_CPU (default 19). Result caches are off.
"""
import hashlib, http.client, json, os, re, statistics, subprocess, sys, time, urllib.parse
from pathlib import Path

L = Path.home() / 'lanes/hybrid-583'
BSBM = L / 'bsbm-10m'
QDIR = BSBM / 'queries'
UNTIMED = int(os.environ.get('UNTIMED', 3))
TIMED = int(os.environ.get('TIMED', 15))
CLIENT_CPU = int(os.environ.get('CLIENT_CPU', 19))
PORT = int(os.environ.get('PORT', 28471))
P_CPUS = set(range(0, 12))
E_CPUS = set(range(12, 20))


def cpus(spec):
    out = set()
    for part in spec.split(','):
        a, _, b = part.partition('-')
        out.update(range(int(a), int(b or a) + 1))
    return out


# name: (cpu list, threads or None for the default)
CONFIGS = {
    'all20': ('0-19', None),
    'all12': ('0-19', 12),
    'p12': ('0-11', None),
    'e7': ('12-18', None),
    'pe19': ('0-18', None),
    'p6': ('0,2,4,6,8,10', None),
    'p12e7x12': ('0-18', 12),
    'p12x20': ('0-11', 20),
    'all16': ('0-19', 16),
    'all8': ('0-19', 8),
    'pe19x12': ('0-18', 12),
    'all14': ('0-19', 14),
    'all18': ('0-19', 18),
    # rayon workers on every CPU, every other thread (request drivers) on the P-cores
    'split20': ('0-19', None, '0-11'),
    'split16': ('0-19', 16, '0-11'),
    'split12': ('0-19', 12, '0-11'),
    'split14': ('0-19', 14, '0-11'),
    'split18': ('0-19', 18, '0-11'),
    # the server's own placement: a fourth element adds serve arguments
    'auto': ('0-19', None),
    'auto18': ('0-19', 18),
    'reqall': ('0-19', None, None, ['--request-cpus', 'all']),
    # item 583: SPARKLES_PLACEMENT_EXP of the experiment build
    'adapt8': ('0-19', None, None, [], 'adaptive:8'),
    'adapt6': ('0-19', None, None, [], 'adaptive:6'),
}

args = sys.argv[1:]
out = Path(args.pop(0))
rounds = int(args.pop(0))
bins = {}
configs = []
while args:
    a = args.pop(0)
    if a == '--bin':
        k, _, v = args.pop(0).partition('=')
        bins[k] = Path(v)
    else:
        configs.append(a)
if not bins:
    bins = {'base': L / 'bin/sparkles-4bd70845'}
only = os.environ.get('QUERIES')
queries = []
for f in sorted(QDIR.glob('bi-q*.rq')):
    queries.append(('bench', f.stem, f.read_text()))
if only:
    keep = only.split()
    queries = [q for q in queries if any(q[1] == k or q[1].startswith(k + '-') for k in keep)]
out.mkdir(parents=True, exist_ok=True)
os.sched_setaffinity(0, {CLIENT_CPU})


def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()


def request(ds, query, path='sparql', body=None, ctype='application/x-www-form-urlencoded', accept='application/sparql-results+json'):
    c = http.client.HTTPConnection('127.0.0.1', PORT, timeout=600)
    if body is None:
        body = urllib.parse.urlencode({'query': query, 'nocache': 'true'})
    try:
        t0 = time.perf_counter()
        c.request('POST', f'/{ds}/{path}', body, {'Content-Type': ctype, 'Accept': accept})
        r = c.getresponse()
        data = r.read()
        ms = (time.perf_counter() - t0) * 1000
        assert r.status == 200, (r.status, data[:400])
        return ms, data
    finally:
        c.close()


def fingerprint(data):
    rows = json.loads(data)['results']['bindings']
    canon = sorted(json.dumps(r, sort_keys=True) for r in rows)
    return len(rows), hashlib.sha256('\n'.join(canon).encode()).hexdigest()[:16]


def threads(pid):
    """tid -> (name, runtime ns, last cpu)"""
    res = {}
    for t in os.listdir(f'/proc/{pid}/task'):
        try:
            name = Path(f'/proc/{pid}/task/{t}/comm').read_text().strip()
            run = int(Path(f'/proc/{pid}/task/{t}/schedstat').read_text().split()[0])
            stat = Path(f'/proc/{pid}/task/{t}/stat').read_text()
            cpu = int(stat.rsplit(')', 1)[1].split()[36])
        except (FileNotFoundError, ProcessLookupError, IndexError):
            continue
        res[t] = (name, run, cpu)
    return res


def proc_stat(pid):
    f = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    # minflt majflt utime stime (fields 10, 12, 14, 15)
    return int(f[7]), int(f[9]), int(f[11]), int(f[12])


def thp(pid):
    for line in Path(f'/proc/{pid}/smaps_rollup').read_text().splitlines():
        if line.startswith('AnonHugePages:'):
            return int(line.split()[1])
    return None


def kind(name):
    if name.startswith('query-'):
        return 'rayon'
    if name.startswith('tokio'):
        return 'tokio'
    return 'other'


def attribute(before, after):
    """runtime ms by thread kind and core type, and per rayon worker"""
    acc = {}
    workers = {}
    for t, (name, run, cpu) in after.items():
        d = run - before.get(t, (name, 0, cpu))[1]
        if d <= 0:
            continue
        ct = 'P' if cpu in P_CPUS else 'E'
        k = f'{kind(name)}-{ct}'
        acc[k] = acc.get(k, 0) + d / 1e6
        if name.startswith('query-'):
            workers[name] = (round(d / 1e6, 3), cpu)
    return {k: round(v, 3) for k, v in acc.items()}, workers


ANSI = re.compile(r'\x1b\[[0-9;]*m')
TOTAL = re.compile(r'total_ms\W{0,3}([0-9.]+)')
EXEC = re.compile(r'exec_ms\W{0,3}([0-9.]+)')


def server_ms(log, start):
    """the server-side ms of every sparql request logged after byte offset start"""
    with open(log, 'rb') as f:
        f.seek(start)
        text = ANSI.sub('', f.read().decode(errors='replace'))
    tot, ex = [], []
    for line in text.splitlines():
        m = TOTAL.search(line)
        if m:
            tot.append(float(m.group(1)))
            e = EXEC.search(line)
            ex.append(float(e.group(1)) if e else None)
    return {'total': tot, 'exec': ex}


def split_threads(pid, spec):
    """pin every thread but the rayon workers to the CPUs of spec"""
    aff = cpus(spec)
    for t in os.listdir(f'/proc/{pid}/task'):
        try:
            name = Path(f'/proc/{pid}/task/{t}/comm').read_text().strip()
            if not name.startswith('query-'):
                os.sched_setaffinity(int(t), aff)
        except (FileNotFoundError, ProcessLookupError, OSError):
            pass


def start(binary, cfg, tag):
    spec, n = CONFIGS[cfg][:2]
    side = out / f'side-{tag}'
    log = out / f'server-{tag}.log'
    env = dict(os.environ)
    env.pop('RAYON_NUM_THREADS', None)
    if n:
        env['RAYON_NUM_THREADS'] = str(n)
    env['RUST_LOG'] = env.get('SERVER_RUST_LOG', 'info')
    env.pop('SPARKLES_PLACEMENT_EXP', None)
    if len(CONFIGS[cfg]) > 4:
        env['SPARKLES_PLACEMENT_EXP'] = CONFIGS[cfg][4]
    env['NO_COLOR'] = '1'
    cmd = [str(binary), '--result-cache-mb', '0', 'serve', '--data', str(side),
           '--loc', f'bench={BSBM / "sparkles.db"}',
           '--port', str(PORT), '--timeout', '600', '--query-memory-mb', '12288']
    if len(CONFIGS[cfg]) > 3:
        cmd += CONFIGS[cfg][3]
    aff = cpus(spec)
    with log.open('ab') as lf:
        proc = subprocess.Popen(cmd, stdout=lf, stderr=lf, env=env, cwd=L,
                                preexec_fn=lambda: os.sched_setaffinity(0, aff))
    for _ in range(2400):
        if proc.poll() is not None:
            raise RuntimeError(f'server exited, see {log}')
        c = http.client.HTTPConnection('127.0.0.1', PORT, timeout=.5)
        try:
            c.request('GET', '/$/ready')
            r = c.getresponse()
            r.read()
            if r.status == 200:
                if len(CONFIGS[cfg]) > 2 and CONFIGS[cfg][2]:
                    split_threads(proc.pid, CONFIGS[cfg][2])
                return proc, log
        except (OSError, TimeoutError):
            pass
        finally:
            c.close()
        time.sleep(.05)
    proc.kill()
    raise RuntimeError('startup timeout')


def stop(proc):
    proc.terminate()
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def explain(ds, query):
    body = json.dumps({'query': query, 'profile': 'run'})
    _, data = request(ds, query, 'sparql/explain', body, 'application/json', 'application/json')
    return json.loads(data)


def governor():
    g = {}
    for c in (0, 12):
        base = Path(f'/sys/devices/system/cpu/cpu{c}/cpufreq')
        g[c] = {k: (base / k).read_text().strip() for k in ('scaling_governor', 'energy_performance_preference', 'scaling_max_freq', 'scaling_min_freq')}
    return g


meta = {
    'host': os.uname().nodename, 'kernel': os.uname().release, 'governor': governor(),
    'binaries': {k: {'path': str(v), 'sha256': sha(v)} for k, v in bins.items()},
    'configs': {c: list(CONFIGS[c.split('/')[-1]]) for c in configs}, 'client_cpu': CLIENT_CPU, 'untimed': UNTIMED, 'timed': TIMED,
    'rounds': rounds, 'queries': [q[1] for q in queries], 'started': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
    'loadavg': Path('/proc/loadavg').read_text().strip(),
}
(out / 'meta.json').write_text(json.dumps(meta, indent=1))
EXPLAIN = os.environ.get('EXPLAIN', '').split()
ref = {}
records = []
# BIN/CONFIG tokens name the pairs explicitly, plain configs pair with every binary
if any('/' in c for c in configs):
    pairs = [tuple(c.split('/', 1)) for c in configs]
else:
    pairs = [(b, c) for b in bins for c in configs]
for rnd in range(1, rounds + 1):
    k = (rnd - 1) % len(pairs)
    order = pairs[k:] + pairs[:k]
    if rnd % 2 == 0:
        order = order[::-1]
    for b, cfg in order:
        tag = f'r{rnd}-{b}-{cfg}'
        proc, log = start(bins[b], cfg, tag)
        try:
            nthreads = sum(1 for n, _, _ in threads(proc.pid).values() if n.startswith('query-'))
            rec = {'round': rnd, 'bin': b, 'config': cfg, 'pid': proc.pid, 'rayon_threads': nthreads, 'queries': {}}
            for ds, name, q in queries:
                for _ in range(UNTIMED):
                    _, data = request(ds, q)
                if len(CONFIGS[cfg]) > 2 and CONFIGS[cfg][2]:
                    split_threads(proc.pid, CONFIGS[cfg][2])
                fp = fingerprint(data)
                assert ref.setdefault(name, fp) == fp, (name, fp, ref[name])
                off = log.stat().st_size
                before = threads(proc.pid)
                ps0 = proc_stat(proc.pid)
                wall = []
                for _ in range(TIMED):
                    ms, data = request(ds, q)
                    wall.append(ms)
                after = threads(proc.pid)
                ps1 = proc_stat(proc.pid)
                time.sleep(0.05)
                srv = server_ms(log, off)
                acc, workers = attribute(before, after)
                rec['queries'][name] = {'wall_ms': [round(x, 3) for x in wall], 'server_ms': srv,
                                        'cpu_ms': acc, 'workers': workers,
                                        'minflt': (ps1[0] - ps0[0]) / TIMED, 'majflt': (ps1[1] - ps0[1]) / TIMED,
                                        'utime_ticks': ps1[2] - ps0[2], 'stime_ticks': ps1[3] - ps0[3]}
                if name in EXPLAIN:
                    try:
                        ex = [explain(ds, q) for _ in range(int(os.environ.get('EXPLAIN_N', 1)))]
                        rec['queries'][name]['explain'] = ex[0]
                        rec['queries'][name]['explains'] = ex
                    except Exception as e:  # noqa: BLE001
                        rec['queries'][name]['explain_error'] = repr(e)
            rec['anon_huge_kb'] = thp(proc.pid)
            records.append(rec)
            summ = ' '.join(f'{n.replace("bi-", "")}={statistics.median(v["wall_ms"]):.1f}' for n, v in rec['queries'].items() if n.endswith('-1') or n == 'births-by-decade')
            print(rnd, b, cfg, f'threads={nthreads}', summ, flush=True)
            (out / 'records.json').write_text(json.dumps({'meta': meta, 'answers': ref, 'records': records}))
        finally:
            stop(proc)
meta['finished'] = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
(out / 'records.json').write_text(json.dumps({'meta': meta, 'answers': ref, 'records': records}))
print('PLACEMENT-DONE', flush=True)
