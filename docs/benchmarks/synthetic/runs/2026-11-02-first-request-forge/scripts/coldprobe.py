#!/usr/bin/env python3
"""Cold first-request probe for X07 item 4.

Each sample evicts the store from the page cache, starts a fresh `sparkles serve`, polls
`/$/ready` with curl as bench.sh does, waits --delay-ms, then times the query with curl's
time_total (the published cold column), and then times the same text again and an unseen
text (a trailing comment) on the warm process. Variants are given as
LABEL=BINARY[,ENV=VAL...] and run interleaved (A B B A ...).
"""
import argparse, json, os, re, signal, socket, statistics, subprocess, sys, tempfile, time

p = argparse.ArgumentParser()
p.add_argument('--db', required=True)
p.add_argument('--query', required=True)
p.add_argument('--rounds', type=int, default=10)
p.add_argument('--delay-ms', type=float, default=0)
p.add_argument('--port', type=int, default=43870)
p.add_argument('--cpus', default='')
p.add_argument('--perf', default='', help='perf events to count over the first request')
p.add_argument('--out', required=True)
p.add_argument('--perf-on', default='first')
p.add_argument('--record', default='', help='perf record arguments over the measured request, e.g. "-e minor-faults -c 1 -g"')
p.add_argument('--serve-arg', action='append', default=[])
p.add_argument('variants', nargs='+')
a = p.parse_args()

query = open(a.query).read()
cpus = [int(c) for c in a.cpus.split(',')] if a.cpus else None
work = tempfile.mkdtemp(prefix='coldprobe-', dir=os.path.dirname(os.path.abspath(a.out)))

def evict():
    for root, _, files in os.walk(a.db):
        for n in files:
            fd = os.open(os.path.join(root, n), os.O_RDONLY)
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(fd)

def curl(url, data=None):
    cmd = ['curl', '-sf', '-o', '/dev/null', '-w', '%{time_total} %{time_connect} %{time_starttransfer}']
    if data is not None:
        cmd += ['-H', 'Accept: application/sparql-results+json', '--data-urlencode', 'query=' + data]
    r = subprocess.run(cmd + [url], capture_output=True, text=True)
    if r.returncode:
        return None
    return [float(x) * 1000 for x in r.stdout.split()]

def snap(pid):
    run = wait = 0
    for t in os.listdir(f'/proc/{pid}/task'):
        try:
            r, w, _ = open(f'/proc/{pid}/task/{t}/schedstat').read().split()
            run += int(r); wait += int(w)
        except OSError:
            pass
    f = open(f'/proc/{pid}/stat').read().rsplit(')', 1)[1].split()
    return run, wait, int(f[7]), int(f[9])

def sample(label, binary, env):
    evict()
    e = dict(os.environ, **{k: v for k, v in env.items() if k not in ('DELAY_MS', 'WARM', 'WARM_DELAY_MS')})
    logpath = os.path.join(work, f'{label}.log')
    off = os.path.getsize(logpath) if os.path.exists(logpath) else 0
    log = open(logpath, 'ab')
    proc = subprocess.Popen([binary, '--result-cache-mb', '0', 'serve', '--data', os.path.join(work, 'server'),
                             '--loc', f'bench={a.db}', '--port', str(a.port), '--timeout', '60', *a.serve_arg],
                            stdout=log, stderr=log, env=e,
                            preexec_fn=(lambda: os.sched_setaffinity(0, cpus)) if cpus else None)
    t0 = time.perf_counter()
    while True:
        if proc.poll() is not None:
            raise SystemExit(f'server exited, see {work}/{label}.log')
        if curl(f'http://127.0.0.1:{a.port}/$/ready') is not None:
            break
        time.sleep(0.05)
    ready = (time.perf_counter() - t0) * 1000
    if env.get('WARM'):
        curl(f'http://127.0.0.1:{a.port}/bench/sparql', env['WARM'])
        time.sleep(float(env.get('WARM_DELAY_MS', 0)) / 1000)
    d = float(env.get('DELAY_MS', a.delay_ms))
    if d:
        time.sleep(d / 1000)
    perf = None
    url = f'http://127.0.0.1:{a.port}/bench/sparql'
    if a.perf_on == 'second':
        first = curl(url, query)
    if a.perf:
        pf = os.path.join(work, 'perf.txt')
        cmd = ['perf', 'stat', '-x,', '-e', a.perf, '-p', str(proc.pid), '-o', pf]
        if a.record:
            cmd = ['perf', 'record', *a.record.split(), '-p', str(proc.pid), '-o', os.path.join(work, f'rec-{label}-{len(res[label])}.data')]
        perf = subprocess.Popen(cmd,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.3)
    s0 = snap(proc.pid)
    if a.perf_on == 'second':
        second = curl(url, query)
    else:
        first = curl(url, query)
    s1 = snap(proc.pid)
    sched = {'cpu_ms': (s1[0] - s0[0]) / 1e6, 'runq_ms': (s1[1] - s0[1]) / 1e6, 'minflt': s1[2] - s0[2], 'majflt': s1[3] - s0[3]}
    s0 = snap(proc.pid)
    s2 = curl(url, query + '\n# again')
    s1 = snap(proc.pid)
    sched2 = {'cpu_ms': (s1[0] - s0[0]) / 1e6, 'runq_ms': (s1[1] - s0[1]) / 1e6, 'minflt': s1[2] - s0[2], 'majflt': s1[3] - s0[3]}
    counts = {}
    if perf:
        perf.send_signal(signal.SIGINT)
        perf.wait()
        for line in (open(pf) if not a.record else []):
            f = line.strip().split(',')
            if len(f) > 2 and f[0] and not line.startswith('#'):
                try:
                    counts[f[2]] = float(f[0])
                except ValueError:
                    pass
    if a.perf_on != 'second':
        second = curl(url, query)
    unseen = curl(url, query + '\n# unseen ' + str(time.time_ns()))
    proc.terminate()
    proc.wait()
    log.close()
    acc = []
    try:
        with open(logpath, errors='replace') as fh:
            fh.seek(off)
            new = fh.read()
        for line in re.sub(r'\x1b\[[0-9;]*m', '', new).splitlines():
            if 'sparkles::access' in line and 'operation="query"' in line:
                acc.append({k: float(v) for k, v in re.findall(r'(parse_ms|plan_ms|exec_ms|serialize_ms|total_ms)=([0-9.]+)', line)})
    except OSError:
        pass
    if env.get('WARM') and acc:
        acc = acc[1:]
    return {'ready': ready, 'first': first, 'second': second, 'unseen': unseen, 'perf': counts, 'sched': sched, 'sched_unseen2': sched2, 'access': acc}

vs = []
for v in a.variants:
    label, rest = v.split('=', 1)
    parts = rest.split(',')
    env = dict(x.split('=', 1) for x in parts[1:])
    vs.append((label, parts[0], env))

res = {l: [] for l, _, _ in vs}
order = []
for r in range(a.rounds):
    seq = vs if r % 2 == 0 else list(reversed(vs))
    for label, b, env in seq:
        s = sample(label, b, env)
        res[label].append(s)
        order.append(label)
        print(label, r, json.dumps({k: s[k] for k in ('first', 'sched', 'sched_unseen2')}), json.dumps(s['access'][:1]), flush=True)
json.dump({'args': vars(a), 'order': order, 'results': res}, open(a.out, 'w'), indent=1)

def med(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else float('nan')
print(f"{'variant':12} {'first':>8} {'second':>8} {'unseen':>8} {'ready':>8}")
for label, _, _ in vs:
    rs = res[label]
    print(f"{label:12} {med([s['first'][0] for s in rs if s['first']]):8.2f} "
          f"{med([s['second'][0] for s in rs if s['second']]):8.2f} "
          f"{med([s['unseen'][0] for s in rs if s['unseen']]):8.2f} {med([s['ready'] for s in rs]):8.1f}")
    for k in ('cpu_ms', 'runq_ms', 'minflt', 'majflt'):
        print(f"   sched first {k:12} {med([s['sched'][k] for s in rs]):10.3f}   unseen2 {med([s['sched_unseen2'][k] for s in rs]):10.3f}")
    for k in ('parse_ms', 'plan_ms', 'exec_ms', 'serialize_ms', 'total_ms'):
        print(f"   server first {k:12} {med([s['access'][0].get(k) for s in rs if s['access']]):8.3f}")
    keys = sorted({k for s in rs for k in s['perf']})
    for k in keys:
        print(f"   {k:30} {med([s['perf'].get(k) for s in rs]):12.0f}")
