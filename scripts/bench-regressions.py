#!/usr/bin/env python3
"""Compare warm/cold HTTP queries on an isolated store copy; disable result caching.

Cold mode evicts only the supplied store's files and restarts the server per sample.
With --io, every request asks for `profile=io` in the native JSON format, and the output
keeps each sample's metadata: the server's execution time and the I/O profile of what the
query read and decoded, by file and by phase (X07's breakdown).

Run one engine at a time. For billion-scale tests, apply the host's memory cap and pass
--server-cpus 0,1,2,3,4,5,6,7,8,9,10,11. Timing excludes server startup and
client process startup, so do not splice these numbers into hyperfine/curl tables.

    bench-regressions.py compare OLD.json NEW.json [LATER.json ...] [--engine sparkles] [--no-fail]

compares result records (scripts/bench-record.py, docs/specs/X06 section 7). Results are
paired by suite, scale, engine, query, mode and clients. The tool prints the ratio of
medians and whether the sample ranges overlap. A query of the compared engine (Sparkles by
default) is a regression when its median is more than 10% slower and the ranges do not
overlap, or more than 5% slower in two consecutive comparisons of a chain of three or more
records. Throughput results (`qmph` and `qps`) have no samples, so they are
compared by ratio, higher being better, with the same two thresholds. A changed answer hash is always reported. The exit status is 1 when anything was
reported.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request


def compare_main(argv):
    """The `compare` subcommand: records in, regressions out."""
    ap = argparse.ArgumentParser(prog='bench-regressions.py compare')
    ap.add_argument('records', nargs='+', help='two or more records, oldest first')
    ap.add_argument('--engine', default='sparkles', help='the engine whose regressions are reported')
    ap.add_argument('--threshold', type=float, default=0.10, help='slower than this fraction, with disjoint ranges')
    ap.add_argument('--repeat-threshold', type=float, default=0.05, help='slower than this in two consecutive comparisons')
    ap.add_argument('--no-fail', action='store_true', help='exit with status 0 even when something is reported')
    ap.add_argument('--all', action='store_true', help='also print the pairs that did not change')
    a = ap.parse_args(argv)
    if len(a.records) < 2:
        ap.error('compare needs at least two records')
    recs = []
    for f in a.records:
        with open(f) as fh:
            rec = json.load(fh)
        # the record versions of scripts/bench-record.py that this comparison understands
        if rec.get('record') not in (1, 2):
            print(f'{f}: record version {rec.get("record")!r} is not one this tool knows (1, 2)', file=sys.stderr)
            return 2
        recs.append(rec)

    def key(rec, r):
        return (rec['suite'], rec['scale'], r['engine'], r['query'], r['mode'], r['clients'])

    def index(rec):
        return {key(rec, r): r for r in rec['results']}

    def span(r):
        s = r.get('samples_ms') or []
        return (min(s), max(s)) if s else None

    reported = 0
    slower_before = {}
    for old, new, oname, nname in zip(recs, recs[1:], a.records, a.records[1:]):
        print(f'== {oname} -> {nname}')
        io, inew = index(old), index(new)
        rows, findings = [], []
        for k in sorted(set(io) & set(inew)):
            o, n = io[k], inew[k]
            label = f'{k[2]} {k[3]} {k[4]}' + (f' x{k[5]}' if k[5] != 1 else '')
            oa, na = o.get('answer'), n.get('answer')
            if oa and na and oa.get('hash') != na.get('hash'):
                findings.append(f'ANSWER CHANGED {label}: {oa.get("rows")} rows -> {na.get("rows")} rows')
            if o.get('status') == 'ok' and n.get('status') != 'ok':
                findings.append(f'STATUS {label}: ok -> {n.get("status")}')
            metric = next((m for m in ('qmph', 'qps') if o.get(m) is not None and n.get(m) is not None), None)
            if metric:
                # throughput: higher is better and there are no samples, so only the thresholds apply
                ot, nt = o[metric], n[metric]
                if ot <= 0:
                    continue
                unit = 'QMpH' if metric == 'qmph' else 'qps'
                ratio = nt / ot
                rows.append((label + f' ({unit})', ot, nt, ratio, 'n/a'))
                if k[2] == a.engine:
                    loss = 1 - ratio
                    repeat = loss > a.repeat_threshold and slower_before.get(k, False)
                    if loss > a.threshold or repeat:
                        why = (f'more than {a.threshold:.0%} lower' if loss > a.threshold
                               else f'more than {a.repeat_threshold:.0%} lower in two consecutive comparisons')
                        findings.append(f'REGRESSION {label}: {ot:.1f} -> {nt:.1f} {unit} ({ratio:.2f}x, {why})')
                    slower_before[k] = loss > a.repeat_threshold
                continue
            om, nm = o.get('median_ms'), n.get('median_ms')
            if om is None or nm is None or om <= 0:
                continue
            ratio = nm / om
            so, sn = span(o), span(n)
            overlap = 'n/a' if not so or not sn else ('yes' if sn[0] <= so[1] and so[0] <= sn[1] else 'no')
            rows.append((label, om, nm, ratio, overlap))
            if k[2] != a.engine:
                continue
            slower = ratio - 1
            repeat = slower > a.repeat_threshold and slower_before.get(k, False)
            disjoint = slower > a.threshold and overlap == 'no'
            if disjoint or repeat:
                why = ' and '.join(w for w, on in (('ranges do not overlap', disjoint),
                                                    ('more than 5% slower in two consecutive comparisons', repeat)) if on)
                findings.append(f'REGRESSION {label}: {om:.2f} -> {nm:.2f} ms ({ratio:.2f}x, {why})')
            slower_before[k] = slower > a.repeat_threshold
        for k in set(slower_before) - set(inew):
            del slower_before[k]
        print(f'{"result":<46} {"old":>10} {"new":>10} {"ratio":>7} overlap')
        for label, om, nm, ratio, overlap in rows:
            if a.all or abs(ratio - 1) > 0.02:
                print(f'{label:<46} {om:>10.2f} {nm:>10.2f} {ratio:>7.2f} {overlap}')
        only_old, only_new = set(io) - set(inew), set(inew) - set(io)
        if only_old or only_new:
            print(f'{len(only_old)} results only in the old record, {len(only_new)} only in the new one')
        for f in findings:
            print(f)
        reported += len(findings)
    print(f'{reported} finding(s)')
    return 0 if a.no_fail or not reported else 1


if len(sys.argv) > 1 and sys.argv[1] == 'compare':
    raise SystemExit(compare_main(sys.argv[2:]))

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
p.add_argument('--keep-going', action='store_true',
               help='record a query whose request fails as {"error": ...} and go on to the next query')
p.add_argument('--serve-arg', action='append', default=[],
               help='a flag for `sparkles serve`, repeated for each word, for example --serve-arg=--memory-profile --serve-arg=small')
p.add_argument('--io', action='store_true',
               help='ask for profile=io in the native format and keep each sample\'s io object and server times')
a = p.parse_args()
if a.io:
    a.format = 'application/x-sparkles+json'
if a.runs < 1 or a.warmup < 0:
    p.error('--runs must be positive and --warmup must be nonnegative')
if not Path(a.db).is_dir():
    p.error('--db must name an existing isolated store copy')
server_cpus = range(os.cpu_count())
if a.server_cpus:
    server_cpus = [int(cpu) for cpu in a.server_cpus.split(',')]
url = f'http://127.0.0.1:{a.port}/bench/sparql' + ('?profile=io' if a.io else '')
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
        '--port', str(a.port), '--timeout', '600', '--read-only', *a.serve_arg],
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
        times, digests, sizes, metas = [], [], [], []
        failed = None
        for i in range(a.runs + (0 if a.cold else a.warmup)):
            if a.cold:
                evict()
                proc = start()
            before = time.perf_counter()
            try:
                with urllib.request.urlopen(req, timeout=600) as response:
                    body = response.read()
            except (urllib.error.URLError, TimeoutError) as e:
                if not a.keep_going:
                    raise
                if a.cold:
                    stop(proc)
                    proc = None
                failed = str(getattr(e, 'code', '') or e)
                break
            elapsed = (time.perf_counter() - before) * 1000
            meta = None
            if a.format == 'application/x-sparkles+json':
                doc = json.loads(body)
                meta = doc.pop('meta', None)
                print('timing', name, meta, flush=True)
                # the answer without the timings and counters, for the digest
                body = json.dumps(doc, sort_keys=True).encode()
            if a.cold:
                stop(proc)
                proc = None
            if a.cold or i >= a.warmup:
                times.append(elapsed)
                if a.io:
                    metas.append(meta)
                # Compare multisets, not ORDER BY sequences. An unordered LIMIT may
                # legitimately select another subset, so also record its row count.
                lines = body.splitlines()
                digests.append(hashlib.sha256(b'\n'.join(sorted(lines))).hexdigest())
                sizes.append(len(lines) - 1)
        if failed is not None:
            result[name] = {'error': failed, 'ms': times}
            print(name, result[name], flush=True)
            Path(a.out).write_text(json.dumps(result, indent=2))
            if not a.cold:
                # a failed request may have left the server in any state
                stop(proc)
                proc = start()
            continue
        result[name] = {'ms': times, 'median': statistics.median(times),
                        'mean': statistics.mean(times), 'stdev': statistics.stdev(times) if len(times) > 1 else 0,
                        'hashes': sorted(set(digests)), 'rows': sorted(set(sizes))}
        if a.io:
            # per sample: the server's own times and the I/O profile (docs/API.md, the I/O profile)
            result[name]['meta'] = metas
        print(name, result[name], flush=True)
        Path(a.out).write_text(json.dumps(result, indent=2))
finally:
    if proc is not None:
        stop(proc)
