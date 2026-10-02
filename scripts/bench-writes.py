#!/usr/bin/env python3
"""Write workloads for scripts/bench.sh: the churn of MODE=updates and the writer of MODE=mixed.

    bench-writes.py gen <data.nt> <commits> <out.ru>
    bench-writes.py apply <update-url> <churn.ru> [--field k=v]...
    bench-writes.py loop <update-url> <seconds> [--rate R] [--people N] [--field k=v]...

`gen` writes one SPARQL Update per line, the same for every engine. About 70% are
INSERT DATA of a triple the data does not hold: a new `foaf:knows` edge between two
existing people (half of the inserts), a new `rdf:type` of an existing person (a quarter)
or a new `ex:tag` literal on an existing person or document (a quarter). The other 30% are
DELETE DATA of a `foaf:knows`, `foaf:name` or `rdf:type` triple of the data. Every choice
comes from a fixed seed and the data, so the file depends only on the data and the count.
All triples are in the default graph.

`apply` sends each line as its own request (an HTML form with `update=`, on a new
connection) and waits for the answer before the next. `loop` sends
single-triple INSERT DATA requests the same way for <seconds>, as fast as the engine
answers or at --rate per second. Each new triple gives an existing person an `ex:mixedTag`
literal that no query reads. Both print a JSON object: the commits that succeeded,
the errors, the seconds, commits per second, and the latency per commit (p50, p99, mean,
max in ms). --field adds a form field to every request, for example QLever's access token.
"""

import http.client
import json
import random
import re
import sys
import time
import urllib.parse

SEED = 20261001
EX = "http://example.org/"
FOAF = "http://xmlns.com/foaf/0.1/"
RDF_TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
KNOWS = f"<{FOAF}knows>"
NAME = f"<{FOAF}name>"
PERSON = re.compile(r"^<http://example\.org/person/(\d+)> ")
KINDS = ["Researcher", "Manager", "Student", "Employee"]


def triples(path):
    """(line without the final ' .', predicate) for every triple of an N-Triples file"""
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip()
            if not line or line.startswith("#"):
                continue
            if line.endswith(" ."):
                line = line[:-2]
            parts = line.split(" ", 2)
            if len(parts) == 3:
                yield line, parts[1]


def gen(data, n, out):
    rnd = random.Random(SEED)
    n_del = round(n * 0.3)
    n_ins = n - n_del
    del_kinds = rnd.choices([KNOWS, NAME, RDF_TYPE], k=n_del)
    want = {p: del_kinds.count(p) for p in (KNOWS, NAME, RDF_TYPE)}
    # pass 1: the number of people, the documents, and a reservoir sample per predicate
    # (twice the need, since the data repeats some triples)
    pool = {p: [] for p in want}
    seen = dict.fromkeys(want, 0)
    sample = {p: random.Random(SEED + i) for i, p in enumerate(want)}
    people = docs = 0
    for line, p in triples(data):
        m = PERSON.match(line)
        if m:
            people = max(people, int(m.group(1)) + 1)
        elif line.startswith(f"<{EX}doc/") and p == RDF_TYPE:
            docs += 1
        if p in pool and line.startswith(f"<{EX}"):
            k = 2 * want[p]
            seen[p] += 1
            if len(pool[p]) < k:
                pool[p].append(line)
            else:
                j = sample[p].randrange(seen[p])
                if j < k:
                    pool[p][j] = line
    if people < 2:
        sys.exit("gen: no people in the data")
    deletes = []
    for p, k in want.items():
        uniq = list(dict.fromkeys(sorted(pool[p])))
        rnd.shuffle(uniq)
        deletes += uniq[:k]
    # insert candidates (twice the need), then pass 2 drops those the data holds
    ins_kinds = rnd.choices(["knows", "type", "tag"], weights=[2, 1, 1], k=n_ins)
    cand = {"knows": [], "type": [], "tag": []}
    for i in range(2 * n_ins):
        kind = ins_kinds[i % n_ins]
        if kind == "knows":
            a, b = rnd.randrange(people), rnd.randrange(people)
            if a != b:
                cand[kind].append(f"<{EX}person/{a}> {KNOWS} <{EX}person/{b}>")
        elif kind == "type":
            cand[kind].append(f"<{EX}person/{rnd.randrange(people)}> {RDF_TYPE} <{EX}{rnd.choice(KINDS)}>")
        else:
            s = f"{EX}doc/{rnd.randrange(docs)}" if docs and rnd.random() < 0.5 else f"{EX}person/{rnd.randrange(people)}"
            cand[kind].append(f'<{s}> <{EX}tag> "churn-{i}"')
    every = {c for cs in cand.values() for c in cs}
    present = {line for line, _ in triples(data) if line in every}
    inserts = []
    for kind in ("knows", "type", "tag"):
        ok = [c for c in dict.fromkeys(cand[kind]) if c not in present]
        inserts += ok[: ins_kinds.count(kind)]
    ops = [f"INSERT DATA {{ {t} . }}" for t in inserts] + [f"DELETE DATA {{ {t} . }}" for t in deletes]
    rnd.shuffle(ops)
    with open(out, "w", encoding="utf-8") as f:
        f.write("\n".join(ops) + "\n")
    print(f"churn: {len(inserts)} inserts, {len(deletes)} deletes ({people} people) -> {out}", file=sys.stderr)


class Client:
    """a new connection per request, as curl makes in the rest of the benchmark. On a
    reused connection QLever's answers wait about 40 ms for a delayed TCP ACK."""

    def __init__(self, url, fields):
        u = urllib.parse.urlsplit(url if "://" in url else "http://" + url)
        self.host, self.port, self.path = u.hostname, u.port or 80, u.path or "/"
        self.fields = fields

    def send(self, update):
        body = urllib.parse.urlencode([("update", update)] + self.fields)
        conn = http.client.HTTPConnection(self.host, self.port, timeout=600)
        try:
            conn.request("POST", self.path, body, {"Content-Type": "application/x-www-form-urlencoded", "Connection": "close"})
            r = conn.getresponse()
            r.read()
            return 200 <= r.status < 300
        except (http.client.HTTPException, OSError):
            return False
        finally:
            conn.close()


def stats(lat, errors, secs):
    lat = sorted(lat)

    def pct(q):
        return round(lat[min(len(lat) - 1, int(q * len(lat)))] * 1000, 3) if lat else None

    return {
        "commits": len(lat),
        "errors": errors,
        "seconds": round(secs, 3),
        "per_s": round(len(lat) / secs, 1) if secs > 0 else None,
        "p50_ms": pct(0.5),
        "p99_ms": pct(0.99),
        "mean_ms": round(sum(lat) / len(lat) * 1000, 3) if lat else None,
        "max_ms": round(lat[-1] * 1000, 3) if lat else None,
    }


def run(client, updates, deadline=None, rate=None):
    lat, errors = [], 0
    t0 = time.perf_counter()
    for i, u in enumerate(updates):
        now = time.perf_counter()
        if deadline is not None and now - t0 >= deadline:
            break
        if rate:
            wait = t0 + i / rate - now
            if wait > 0:
                time.sleep(wait)
        t = time.perf_counter()
        if client.send(u):
            lat.append(time.perf_counter() - t)
        else:
            errors += 1
    return stats(lat, errors, time.perf_counter() - t0)


def opts(args):
    fields, kv = [], {}
    it = iter(args)
    for a in it:
        if a == "--field":
            fields.append(tuple(next(it).split("=", 1)))
        elif a.startswith("--"):
            kv[a[2:]] = next(it)
        else:
            sys.exit(f"unexpected argument {a}")
    return fields, kv


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "gen":
        gen(args[0], int(args[1]), args[2])
    elif cmd == "apply":
        fields, _ = opts(args[2:])
        with open(args[1], encoding="utf-8") as f:
            ups = [line.rstrip("\n") for line in f if line.strip()]
        print(json.dumps(run(Client(args[0], fields), ups)))
    elif cmd == "loop":
        fields, kv = opts(args[2:])
        people = int(kv.get("people", "1000"))
        rate = float(kv.get("rate", "0")) or None
        stamp = int(time.time())

        def ups():
            i = 0
            while True:
                yield f'INSERT DATA {{ <{EX}person/{i % people}> <{EX}mixedTag> "mixed-{stamp}-{i}" . }}'
                i += 1

        print(json.dumps(run(Client(args[0], fields), ups(), deadline=float(args[1]), rate=rate)))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
