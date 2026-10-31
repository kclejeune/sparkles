#!/usr/bin/env python3
"""Complete-HTTP JSON timer for births-by-decade on the 50M slice, Sparkles against QLever on one host.

Each round runs the two engines in alternating order. For each engine it does three cold requests,
each after a restart with the store evicted from the page cache (posix_fadvise DONTNEED), and then
one warm process with three untimed and twenty timed requests. A request opens a new connection
and reads the whole JSON body. The timer is pinned to CPU 11 and the servers use every CPU.
Result caches are off (Sparkles: --result-cache-mb 0 and nocache=true; QLever: -e 0B).
"""
import hashlib, http.client, json, os, platform, socket, statistics, subprocess, sys, time, urllib.parse
from decimal import Decimal
from pathlib import Path

B = Path.home() / 'lanes/forge-checks-440-432-20261031'
S50 = B / 'billion/50m'
SPARKLES = B / 'bin/1acdc38a'
QLEVER = Path('/nix/store/xn7f9c965zfza9nks2qzlrnx916xg3h2-qlever-0.5.48/bin/qlever-server')
QFILE = B / 'src/1acdc38a/scripts/bench-billion/queries/births-by-decade.rq'
query = QFILE.read_text()
ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 4
WARM, COLD, UNTIMED = 20, 3, 3
PORTS = {'S': 41910, 'Q': 41911}
out = B / ('births-http-' + time.strftime('%Y%m%dT%H%M%SZ', time.gmtime()))
out.mkdir()
all_cpus = os.sched_getaffinity(0)
os.sched_setaffinity(0, {11})


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def request(label):
    c = http.client.HTTPConnection('127.0.0.1', PORTS[label], timeout=300)
    fields = {'query': query}
    if label == 'S':
        fields.update({'nocache': 'true', 'execution': 'auto'})
        path = '/bench/sparql'
    else:
        fields['send'] = '10000000'
        path = '/'
    try:
        t0 = time.perf_counter()
        c.request('POST', path, urllib.parse.urlencode(fields),
                  {'Content-Type': 'application/x-www-form-urlencoded', 'Accept': 'application/sparql-results+json'})
        r = c.getresponse()
        body = r.read()
        ms = (time.perf_counter() - t0) * 1000
        assert r.status == 200, (r.status, body[:300])
        return ms, body
    finally:
        c.close()


def groups(body):
    rows = json.loads(body)['results']['bindings']
    pairs = sorted((int(Decimal(r['decade']['value'])), int(Decimal(r['n']['value']))) for r in rows)
    return pairs


def evict(path):
    for root, _, files in os.walk(path):
        for f in files:
            fd = os.open(Path(root) / f, os.O_RDONLY)
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(fd)


def start(label, tag):
    log = (out / f'server-{tag}.log').open('ab')
    if label == 'S':
        cmd = [str(SPARKLES), '--result-cache-mb', '0', 'serve', '--data', str(out / f'side-{tag}'),
               '--loc', f'bench={S50 / "sparkles.db"}', '--port', str(PORTS[label]), '--timeout', '300',
               '--query-memory-mb', '12288', '--no-access-log']
        cwd, ready = B, '/$/ready'
    else:
        cmd = [str(QLEVER), '-i', 'dbpedia', '-p', str(PORTS[label]), '-m', '12G', '-c', '2G', '-e', '0B',
               '-s', '300s', '-a', 'bench', '-j', '16']
        cwd, ready = S50 / 'qlever-index', '/?cmd=stats'
    proc = subprocess.Popen(cmd, cwd=cwd, stdout=log, stderr=log, preexec_fn=lambda: os.sched_setaffinity(0, all_cpus))
    log.close()
    for _ in range(1200):
        if proc.poll() is not None:
            raise RuntimeError(f'{label} exited, see {out}/server-{tag}.log')
        c = http.client.HTTPConnection('127.0.0.1', PORTS[label], timeout=.5)
        try:
            c.request('GET', ready)
            r = c.getresponse()
            r.read()
            if r.status == 200:
                return proc
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


store = {'S': S50 / 'sparkles.db', 'Q': S50 / 'qlever-index'}
records, ref = [], None
for rnd in range(1, ROUNDS + 1):
    order = ['S', 'Q'] if rnd % 2 else ['Q', 'S']
    for label in order:
        cold = []
        for i in range(COLD):
            evict(store[label])
            p = start(label, f'r{rnd}-{label}-cold{i}')
            try:
                ms, body = request(label)
            finally:
                stop(p)
            g = groups(body)
            ref = ref or g
            assert g == ref, (label, 'answer differs')
            cold.append(ms)
        p = start(label, f'r{rnd}-{label}-warm')
        try:
            for _ in range(UNTIMED):
                request(label)
            warm = []
            for _ in range(WARM):
                ms, body = request(label)
                assert groups(body) == ref, (label, 'warm answer differs')
                warm.append(ms)
        finally:
            stop(p)
        records.append({'round': rnd, 'engine': label, 'cold_ms': cold, 'warm_ms': warm})
        print(rnd, label, 'cold', [round(x, 1) for x in cold], 'warm median', round(statistics.median(warm), 2), flush=True)
        (out / 'partial.json').write_text(json.dumps(records, indent=1))
summary = {}
for label in 'SQ':
    w = [x for r in records if r['engine'] == label for x in r['warm_ms']]
    c = [x for r in records if r['engine'] == label for x in r['cold_ms']]
    summary[label] = {'warm_median_ms': statistics.median(w), 'cold_median_ms': statistics.median(c), 'n_warm': len(w), 'n_cold': len(c)}
print(json.dumps(summary, indent=1))
rec = {
    'protocol': 'Complete HTTP JSON body timed by a Python http.client timer pinned to CPU 11, new connection per request, servers on all CPUs, one engine alone, result caches off. Each round runs both engines in alternating order. Per engine: three cold requests, each after a restart with posix_fadvise DONTNEED of the store, then one warm process with three untimed and twenty timed requests.',
    'host': {'name': 'forge', 'cpu': '13th Gen Intel(R) Core(TM) i5-13500', 'kernel': platform.release(),
             'governor': Path('/sys/devices/system/cpu/cpu2/cpufreq/scaling_governor').read_text().strip(),
             'epp': Path('/sys/devices/system/cpu/cpu2/cpufreq/energy_performance_preference').read_text().strip()},
    'sparkles': {'commit': '1acdc38a', 'binary_sha256': sha(SPARKLES)},
    'qlever': {'version': '0.5.48', 'path': str(QLEVER), 'binary_sha256': sha(QLEVER)},
    'query_sha256': sha(QFILE),
    'groups': len(ref), 'groups_sha256': hashlib.sha256(json.dumps(ref).encode()).hexdigest()[:16],
    'rounds': ROUNDS, 'records': records, 'summary': summary,
}
(out / 'complete.json').write_text(json.dumps(rec, indent=1))
print('COMPLETE', out)
