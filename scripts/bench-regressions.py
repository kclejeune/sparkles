#!/usr/bin/env python3
"""Compare warm/cold HTTP queries on an isolated store copy; disable result caching.

Cold mode evicts only the supplied store's files and restarts the server per sample.
Run one engine at a time. For billion-scale tests, apply the host's memory cap and
pass --server-cpus 0,1,2,3,4,5,6,7,8,9,10,11. Timing excludes server startup and
client process startup, so do not splice these numbers into hyperfine/curl tables.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import statistics
import subprocess
import time
import urllib.request

p = argparse.ArgumentParser()
p.add_argument('--binary', required=True)
p.add_argument('--db', required=True)
p.add_argument('--queries', required=True)
p.add_argument('--names', nargs='+', required=True)
p.add_argument('--out', required=True)
p.add_argument('--runs', type=int, default=7)
p.add_argument('--warmup', type=int, default=2)
p.add_argument('--cold', action='store_true')
p.add_argument('--port', type=int, default=5660)
p.add_argument('--format', default='text/tab-separated-values')
p.add_argument('--server-cpus')
a = p.parse_args()
if a.runs < 1 or a.warmup < 0:
    p.error('--runs must be positive and --warmup must be nonnegative')
if not Path(a.db).is_dir():
    p.error('--db must name an existing isolated store copy')
server_cpus = range(os.cpu_count())
if a.server_cpus:
    server_cpus = [int(cpu) for cpu in a.server_cpus.split(',')]
url = f'http://127.0.0.1:{a.port}/bench/sparql'
work = Path(a.out).parent
work.mkdir(parents=True, exist_ok=True)

def start():
    try:
        busy = socket.create_connection(('127.0.0.1', a.port), timeout=.25)
    except OSError:
        pass
    else:
        busy.close()
        raise RuntimeError(f'port {a.port} is already in use')
    log = open(work / 'server.log', 'ab')
    proc = subprocess.Popen([a.binary, '--result-cache-mb', '0', 'serve',
        '--data', str(work / 'server'), '--loc', f'bench={a.db}',
        '--port', str(a.port), '--timeout', '600', '--read-only'],
        stdout=log, stderr=log,
        preexec_fn=lambda: os.sched_setaffinity(0, server_cpus))
    log.close()
    for _ in range(600):
        if proc.poll() is not None:
            raise RuntimeError(f'server exited: {work / "server.log"}')
        try:
            urllib.request.urlopen(f'http://127.0.0.1:{a.port}/$/ping', timeout=1).close()
            return proc
        except Exception:
            time.sleep(.05)
    stop(proc)
    raise RuntimeError('server did not start')

def stop(proc):
    proc.terminate()
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()

def evict():
    for root, _, files in os.walk(a.db):
        for name in files:
            fd = os.open(os.path.join(root, name), os.O_RDONLY)
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(fd)

result = {}
proc = None
try:
    if not a.cold:
        proc = start()
    for name in a.names:
        query = (Path(a.queries) / f'{name}.rq').read_bytes()
        req = urllib.request.Request(url, data=query,
            headers={'Content-Type': 'application/sparql-query',
                     'Accept': a.format})
        times, digests, sizes = [], [], []
        for i in range(a.runs + (0 if a.cold else a.warmup)):
            if a.cold:
                evict()
                proc = start()
            before = time.perf_counter()
            with urllib.request.urlopen(req, timeout=600) as response:
                body = response.read()
            elapsed = (time.perf_counter() - before) * 1000
            if a.format == 'application/x-sparkles+json':
                print('timing', name, json.loads(body).get('meta'), flush=True)
            if a.cold:
                stop(proc)
                proc = None
            if a.cold or i >= a.warmup:
                times.append(elapsed)
                # Compare multisets, not ORDER BY sequences. An unordered LIMIT may
                # legitimately select another subset, so also record its row count.
                lines = body.splitlines()
                digests.append(hashlib.sha256(b'\n'.join(sorted(lines))).hexdigest())
                sizes.append(len(lines) - 1)
        result[name] = {'ms': times, 'median': statistics.median(times),
                        'mean': statistics.mean(times), 'stdev': statistics.stdev(times) if len(times) > 1 else 0,
                        'hashes': sorted(set(digests)), 'rows': sorted(set(sizes))}
        print(name, result[name], flush=True)
        Path(a.out).write_text(json.dumps(result, indent=2))
finally:
    if proc is not None:
        stop(proc)
