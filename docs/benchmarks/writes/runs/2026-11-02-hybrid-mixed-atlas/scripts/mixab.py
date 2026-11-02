#!/usr/bin/env python3
"""Item 583: the mixed read/write load of scripts/bench.sh (MODE=mixed) at 1.05M under
several server placements, with the scheduler's view of every server thread.

  mixab.py OUT ROUNDS CONFIG... [--bin NAME=PATH ...]

Every round runs each (binary, config) pair once in a rotated order, every second round
reversed. A run copies the loaded store, starts the server, and for SECONDS (default 30)
runs CONC (default 16) oha readers of star-join and one bench-writes.py writer exactly as
bench.sh's mixed mode does. It records bench.sh's figures, the run and run-queue wait time
of every server thread by kind from /proc/<pid>/task/*/schedstat, and the busy share of the
performance and efficiency CPUs from /proc/stat over the window.
"""
import json, os, signal, subprocess, sys, time, urllib.request
from pathlib import Path

L = Path.home() / 'lanes/hybrid-583'
SRC = L / 'wt'
BASE = L / 'base.db'
OHA = os.environ.get('OHA', '/nix/store/j7xhdjciiadnhxm9x82yfi27wqg7r2br-oha-1.16.0/bin/oha')
SECONDS = int(os.environ.get('SECONDS_', 30))
CONC = int(os.environ.get('CONC', 16))
PORT = int(os.environ.get('PORT', 28583))
P_CPUS = set(range(0, 12))
E_CPUS = set(range(12, 20))

# name: (serve arguments, environment)
CONFIGS = {
    'auto': ([], {}),
    'all': (['--request-cpus', 'all'], {}),
    'auto20': ([], {'RAYON_NUM_THREADS': '20'}),
    'all18': (['--request-cpus', 'all'], {'RAYON_NUM_THREADS': '18'}),
    'noinplace': ([], {'SPARKLES_NO_IN_PLACE': '1'}),
    'allnoinplace': (['--request-cpus', 'all'], {'SPARKLES_NO_IN_PLACE': '1'}),
}
for k in list(os.environ):
    # PLACE_<name>=<SPARKLES_PLACEMENT_EXP value> adds an experimental config
    if k.startswith('PLACE_'):
        CONFIGS[k[6:].lower()] = ([], {'SPARKLES_PLACEMENT_EXP': os.environ[k]})

args = sys.argv[1:]
out = Path(args.pop(0))
rounds = int(args.pop(0))
bins, configs = {}, []
while args:
    a = args.pop(0)
    if a == '--bin':
        k, _, v = args.pop(0).partition('=')
        bins[k] = Path(v)
    else:
        configs.append(a)
out.mkdir(parents=True, exist_ok=True)
form = out / 'star-join.form'
q = (SRC / 'scripts/bench.sh').read_text()
P = 'PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> '
STAR = P + 'SELECT ?p ?n ?a ?o WHERE { ?p a ex:Researcher ; foaf:name ?n ; foaf:age ?a ; ex:worksFor ?o . ?o ex:city "Kyoto" }'
assert 'add star-join \'' + STAR[len(P):] + "'" in q, 'star-join differs from bench.sh'
import urllib.parse
form.write_text(urllib.parse.urlencode({'query': STAR}))


def threads(pid):
    res = {}
    for t in os.listdir(f'/proc/{pid}/task'):
        try:
            name = Path(f'/proc/{pid}/task/{t}/comm').read_text().strip()
            run, wait, slices = map(int, Path(f'/proc/{pid}/task/{t}/schedstat').read_text().split())
        except (FileNotFoundError, ProcessLookupError, ValueError):
            continue
        res[t] = (name, run, wait, slices)
    return res


def kind(name):
    if name.startswith('query-'):
        return 'rayon'
    if name.startswith('tokio'):
        return 'tokio'
    return 'other'


def sched_delta(before, after):
    acc = {}
    for t, (name, run, wait, sl) in after.items():
        b = before.get(t, (name, 0, 0, 0))
        k = kind(name)
        a = acc.setdefault(k, {'run_ms': 0.0, 'wait_ms': 0.0, 'slices': 0, 'threads': 0})
        a['run_ms'] += (run - b[1]) / 1e6
        a['wait_ms'] += (wait - b[2]) / 1e6
        a['slices'] += sl - b[3]
        a['threads'] += 1
    return {k: {kk: round(vv, 1) if isinstance(vv, float) else vv for kk, vv in v.items()} for k, v in acc.items()}


def cpu_times():
    res = {}
    for line in Path('/proc/stat').read_text().splitlines():
        if line.startswith('cpu') and line[3].isdigit():
            f = line.split()
            vals = list(map(int, f[1:]))
            idle = vals[3] + vals[4]
            res[int(f[0][3:])] = (sum(vals[:8]), idle)
    return res


def busy(before, after, cpus):
    tot = sum(after[c][0] - before[c][0] for c in cpus)
    idle = sum(after[c][1] - before[c][1] for c in cpus)
    return round(1 - idle / tot, 3) if tot else None


def run(binname, cfg, idx):
    sargs, env = CONFIGS[cfg]
    d = out / f'run-{idx:03d}-{binname}-{cfg}'
    d.mkdir(parents=True, exist_ok=True)
    store = L / 'scratch'
    subprocess.run(['rm', '-rf', str(store)], check=True)
    store.mkdir()
    subprocess.run(['cp', '-a', str(BASE), str(store / 'sparkles.db')], check=True)
    subprocess.run(['sync'], check=True)
    e = dict(os.environ)
    e.update(env)
    log = open(d / 'server.log', 'wb')
    srv = subprocess.Popen([str(bins[binname]), '--result-cache-mb', '0', 'serve', '--data', str(store / 'server'),
                            '--loc', f'bench={store / "sparkles.db"}', '--port', str(PORT), '--timeout', '315', *sargs],
                           stdout=log, stderr=subprocess.STDOUT, env=e)
    try:
        for _ in range(300):
            try:
                urllib.request.urlopen(f'http://localhost:{PORT}/$/ready', timeout=2).read()
                break
            except Exception:
                time.sleep(0.1)
        else:
            raise RuntimeError('server not ready')
        time.sleep(2)
        t0, c0 = threads(srv.pid), cpu_times()
        w = subprocess.Popen(['python3', str(SRC / 'scripts/bench-writes.py'), 'loop', f'localhost:{PORT}/bench/update', str(SECONDS),
                              '--rate', '0'], stdout=open(d / 'writer.json', 'w'))
        subprocess.run([OHA, '-z', f'{SECONDS}s', '-c', str(CONC), '--disable-keepalive', '--disable-compression', '-m', 'POST',
                        '-T', 'application/x-www-form-urlencoded', '-D', str(form), '-A', 'text/tab-separated-values', '-t', '300s',
                        '--no-tui', '--output-format', 'json', '-o', str(d / 'readers.json'), f'http://localhost:{PORT}/bench/sparql'],
                       check=True, stdout=subprocess.DEVNULL)
        if w.wait() != 0:
            raise RuntimeError('writer failed')
        t1, c1 = threads(srv.pid), cpu_times()
    finally:
        srv.send_signal(signal.SIGTERM)
        try:
            srv.wait(timeout=60)
        except subprocess.TimeoutExpired:
            srv.kill()
            srv.wait()
    o = json.load(open(d / 'readers.json'))
    wj = json.load(open(d / 'writer.json'))
    s = o['summary']
    codes = o.get('statusCodeDistribution', {})
    ok = sum(v for k, v in codes.items() if k.startswith('2'))
    errs = sum(codes.values()) - ok + sum(v for k, v in o.get('errorDistribution', {}).items() if 'deadline' not in k)
    p = o.get('latencyPercentiles', {})
    ms = lambda v: round(v * 1000, 2) if v is not None else None
    rec = {'bin': binname, 'config': cfg, 'run': idx, 'read_qps': round(ok / s['total'], 1), 'read_p50_ms': ms(p.get('p50')),
           'read_p99_ms': ms(p.get('p99')), 'read_errors': errs, 'writes_per_s': wj['per_s'], 'write_p50_ms': wj['p50_ms'],
           'write_p99_ms': wj['p99_ms'], 'write_errors': wj['errors'], 'sched': sched_delta(t0, t1),
           'busy_p': busy(c0, c1, P_CPUS), 'busy_e': busy(c0, c1, E_CPUS),
           'loadavg': Path('/proc/loadavg').read_text().split()[:3]}
    (d / 'record.json').write_text(json.dumps(rec, indent=1))
    return rec


pairs = [(b, c) for b in bins for c in configs]
recs = []
idx = 0
for r in range(rounds):
    order = pairs[r % len(pairs):] + pairs[:r % len(pairs)]
    if r % 2:
        order = order[::-1]
    for b, c in order:
        idx += 1
        rec = run(b, c, idx)
        recs.append(rec)
        sc = rec['sched']
        print(f"r{r} {b:>10} {c:>12} reads/s {rec['read_qps']:7.1f} writes/s {rec['writes_per_s']:6.1f} "
              f"wp50 {rec['write_p50_ms']} busyP {rec['busy_p']} busyE {rec['busy_e']} "
              + ' '.join(f"{k}:run={v['run_ms']:.0f},wait={v['wait_ms']:.0f},n={v['threads']}" for k, v in sorted(sc.items())),
              flush=True)
        (out / 'records.json').write_text(json.dumps(recs, indent=1))
subprocess.run(['rm', '-rf', str(L / 'scratch')])
print('MIXABDONE', flush=True)
