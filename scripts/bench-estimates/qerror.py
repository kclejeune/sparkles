#!/usr/bin/env python3
"""Record the planner's estimated rows against the rows each operator produced.

    qerror.py run --sparkles BIN --loc DB --queries DIR --out FILE.jsonl [--port P]
                  [--disable NAMES] [--runs N] [--timeout S] [--threads N]
    qerror.py compare BASE.jsonl CAND.jsonl [--md OUT.md]
    qerror.py summary FILE.jsonl ...

`run` starts `sparkles serve` on the database DB, with SPARKLES_DISABLE_OPTIMIZATIONS set
to NAMES when given, and sends every query of DIR (*.rq, sorted by name) N times (default
2) with the result cache off. The plan of the last run, which `application/x-sparkles+json`
returns in `plan`, is kept: the first run reads the blocks that the planner's key
probes and samples look at only when the block cache holds them. Each line of the output
holds a query's name, its execution time, its row count, a digest of its sorted answer
and every plan node with its operator, description, estimated rows and actual rows.

The q-error of a node is max(est, act) / min(est, act), with both raised to at least one
row. Nodes that did not run to completion (a skipped right side, an input stopped early by
LIMIT, an operator rerun with a larger input) and nodes without an estimate are left out.
`compare` reports, for the join nodes and for all nodes, the distribution of the q-error
under both files, each query's largest join q-error and the geometric mean of its join
q-errors, and every query whose answers differ. It lists every query whose largest join
q-error rose, and, for queries whose plans have the same shape in both files, every join
node whose q-error rose. `summary` prints the distributions of each file.

Answer format 1 compares SELECT multisets, ASK booleans and complete RDF graph sets.
Blank-node relabeling is exact for at most eight nodes and one million renamed row
visits. Larger searches and unsupported term or response kinds fail explicitly.
Legacy unversioned files remain readable for plan statistics, but their answer checks
are unverified. Unknown answer-format versions are refused. A failed run retains its
error records and exits unsuccessfully after stopping its own server.
"""

import argparse
import hashlib
import itertools
import json
import math
import os
import pathlib
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


def post(url, query, timeout):
    body = urllib.parse.urlencode({"query": query}).encode()
    req = urllib.request.Request(
        url + "?nocache=true",
        data=body,
        headers={
            "Accept": "application/x-sparkles+json",
            "Content-Type": "application/x-www-form-urlencoded",
        },
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def flags(node, outer=None):
    """What makes a node's actual rows incomparable with its estimate. `outer` is the
    cursor node around a streamed operator, whose completion flag counts too."""
    fid = node.get("fidelity") or {}
    out = []
    for k in ("skipped", "stoppedEarly", "incomplete"):
        if node.get(k) or fid.get(k):
            out.append(k)
    if node.get("runs") or fid.get("runs") or fid.get("reruns"):
        out.append("reruns")
    if node.get("complete") is False or (outer is not None and outer.get("complete") is False):
        if "incomplete" not in out:
            out.append("incomplete")
    if node.get("cached"):
        out.append("cached")
    return out


def walk(node, path="0", depth=0, out=None):
    """The nodes of an eager plan or of a streamed one, whose cursor nodes hold the
    operator's description, estimate and counts in `operator`."""
    if out is None:
        out = []
    op = node.get("operator")
    if isinstance(op, dict):
        name = op.get("operator") or op.get("name") or ""
        info, outer = op, node
    else:
        name = op or ""
        info, outer = node, None
    out.append(
        {
            "id": node.get("id", info.get("id", path)),
            "depth": depth,
            "op": name,
            "desc": (info.get("description") or "")[:300],
            "est": info.get("estimatedRows"),
            "act": info.get("actualRows"),
            "flags": flags(info, outer),
        }
    )
    for i, c in enumerate(node.get("children") or []):
        walk(c, f"{path}.{i}", depth + 1, out)
    return out


DIGEST_FORMAT = 1
QUERY_KINDS = {"SELECT", "ASK", "CONSTRUCT", "DESCRIBE"}
XSD_STRING = "http://www.w3.org/2001/XMLSchema#string"
RDF_LANG_STRING = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"


def _term(value, position=None):
    if value is None and position is None:
        return ("unbound",)
    if not isinstance(value, dict) or not isinstance(value.get("value"), str):
        raise ValueError("malformed RDF term")
    kind, text = value.get("type"), value["value"]
    allowed = {"uri", "bnode", "literal"}
    if position == "predicate":
        allowed = {"uri"}
    elif position in {"subject", "graph"}:
        allowed = {"uri", "bnode"}
    if not isinstance(kind, str) or kind not in allowed:
        raise ValueError(f"unsupported RDF term kind {kind!r} at {position or 'SELECT'}")
    if kind != "literal":
        if not text or "datatype" in value or "xml:lang" in value:
            raise ValueError("malformed IRI or blank-node term")
        return kind, text
    datatype = value.get("datatype", XSD_STRING)
    if not isinstance(datatype, str) or not datatype:
        raise ValueError("malformed literal datatype")
    language = value.get("xml:lang", "")
    if "xml:lang" in value:
        if not isinstance(language, str) or not re.fullmatch(r"[A-Za-z]+(?:-[A-Za-z0-9]+)*", language):
            raise ValueError("malformed literal language tag")
        if "datatype" in value and datatype != RDF_LANG_STRING:
            raise ValueError("language literal has a conflicting datatype")
        datatype = RDF_LANG_STRING
    return "literal", text, datatype, language.lower()


def _canonical(rows):
    labels = sorted({term[1] for row in rows for term in row if term[0] == "bnode"})
    if not labels:
        return tuple(sorted(rows))
    if len(labels) > 8 or math.factorial(len(labels)) * len(rows) > 1_000_000:
        raise ValueError("blank-node canonicalization exceeds the supported search budget")
    best = None
    for permutation in itertools.permutations(range(len(labels))):
        names = {label: str(n) for label, n in zip(labels, permutation)}
        candidate = tuple(sorted(
            tuple(("bnode", names[term[1]]) if term[0] == "bnode" else term for term in row)
            for row in rows
        ))
        if best is None or candidate < best:
            best = candidate
    return best


def _complete(resp, count):
    meta = resp.get("meta")
    if not isinstance(meta, dict) or meta.get("status", "complete") != "complete":
        raise ValueError("native answer is incomplete or lacks completion metadata")
    for record in (resp, meta):
        if "describeTruncated" in record:
            if type(record["describeTruncated"]) is not bool or record["describeTruncated"]:
                raise ValueError("native graph answer was truncated")
    for name in ("sentRows", "totalRows"):
        if type(meta.get(name)) is not int or meta[name] != count:
            raise ValueError(f"incomplete native answer: {name}")


def digest(resp):
    if not isinstance(resp, dict) or "error" in resp:
        raise ValueError("native answer is an error or not an object")
    kind = resp.get("queryType")
    if not isinstance(kind, str) or kind not in QUERY_KINDS:
        raise ValueError(f"unsupported native query kind: {kind!r}")
    schema = []
    if kind == "SELECT":
        schema, values = resp.get("vars"), resp.get("rows")
        if not isinstance(schema, list) or any(not isinstance(v, str) for v in schema) or len(set(schema)) != len(schema):
            raise ValueError("invalid SELECT schema")
        if not isinstance(values, list) or any(not isinstance(row, list) or len(row) != len(schema) for row in values):
            raise ValueError("invalid SELECT rows")
        _complete(resp, len(values))
        order = sorted(range(len(schema)), key=lambda i: schema[i])
        rows = [tuple(_term(row[i]) for i in order) for row in values]
        schema = sorted(schema)
    elif kind == "ASK":
        if type(resp.get("boolean")) is not bool:
            raise ValueError("ASK answer requires a boolean")
        _complete(resp, 1)
        rows = [(("boolean", str(resp["boolean"])),)]
    else:
        if "triples" not in resp and "quads" not in resp:
            raise ValueError("native graph answer lacks triples or quads")
        triples, quads = resp.get("triples", []), resp.get("quads", [])
        if not isinstance(triples, list) or not isinstance(quads, list):
            raise ValueError("invalid graph arrays")
        _complete(resp, len(triples) + len(quads))
        rows = set()
        for width, values in ((3, triples), (4, quads)):
            for row in values:
                if not isinstance(row, list) or len(row) != width:
                    raise ValueError("invalid graph row width")
                triple = tuple(_term(term, position) for term, position in zip(row[:3], ("subject", "predicate", "object")))
                graph = ("default",) if width == 3 or row[3] is None else _term(row[3], "graph")
                rows.add(triple + (graph,))
    canonical = _canonical(rows)
    payload = json.dumps([kind, schema, canonical], ensure_ascii=True, separators=(",", ":"))
    return hashlib.sha256(payload.encode()).hexdigest(), len(rows)


def run(a):
    if a.runs < 1:
        raise ValueError("--runs must be positive")
    env = dict(os.environ)
    if a.disable:
        env["SPARKLES_DISABLE_OPTIMIZATIONS"] = a.disable
    if a.threads:
        env["RAYON_NUM_THREADS"] = str(a.threads)
    data = pathlib.Path(a.out).with_suffix(".srv")
    subprocess.run(["rm", "-rf", str(data)], check=True)
    log = open(pathlib.Path(a.out).with_suffix(".log"), "w")
    srv = subprocess.Popen(
        [a.sparkles, "--result-cache-mb", "0", "serve", "--data", str(data),
         "--loc", f"bench={a.loc}", "--port", str(a.port), "--timeout", str(a.timeout),
         "--no-access-log"],
        env=env, stdout=log, stderr=subprocess.STDOUT,
    )
    try:
        base = f"http://localhost:{a.port}"
        for _ in range(3000):
            try:
                urllib.request.urlopen(base + "/$/ready", timeout=2)
                break
            except Exception:
                if srv.poll() is not None:
                    sys.exit(f"server exited, see {log.name}")
                time.sleep(0.1)
        url = base + "/bench/sparql"
        names = sorted(pathlib.Path(a.queries).glob("*.rq"))
        if a.only:
            keep = set(a.only.split())
            names = [n for n in names if n.stem in keep]
        if not names:
            raise ValueError("no query files selected")
        errors = 0
        with open(a.out, "w") as out:
            for q in names:
                text = q.read_text()
                rec = {"query": q.stem}
                try:
                    for _ in range(a.runs):
                        t0 = time.perf_counter()
                        resp = post(url, text, a.timeout)
                        wall = time.perf_counter() - t0
                    meta = resp.get("meta") or {}
                    plan = resp.get("plan") or meta.get("plan")
                    timing = resp.get("timing") or meta.get("timing") or {}
                    rec["execMs"] = timing.get("execMs")
                    rec["planMs"] = timing.get("planMs")
                    rec["wall"] = wall
                    rec["nodes"] = walk(plan) if plan else []
                    rec["digest"], rec["rows"] = digest(resp)
                    rec["digestFormat"] = DIGEST_FORMAT
                    rec["queryType"] = resp["queryType"]
                except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
                    errors += 1
                    rec["error"] = str(e)[:300]
                out.write(json.dumps(rec) + "\n")
                out.flush()
                print(q.stem, rec.get("execMs"), rec.get("error", ""), file=sys.stderr)
    finally:
        srv.send_signal(signal.SIGTERM)
        try:
            srv.wait(timeout=60)
        except subprocess.TimeoutExpired:
            srv.kill()
            srv.wait()
        log.close()
        subprocess.run(["rm", "-rf", str(data)], check=False)
    if errors:
        sys.exit(f"{errors} queries failed execution or answer validation; see {a.out}")


def qerr(n):
    e, x = n.get("est"), n.get("act")
    if e is None or x is None or e < 0 or x < 0 or n.get("flags"):
        return None
    return max(e, 1.0, x) / max(min(max(e, 1.0), max(x, 1.0)), 1.0)


def is_join(n):
    op = n["op"]
    return "Join" in op or op in ("Product", "CrossProduct")


def load(f):
    with open(f) as source:
        records = {r["query"]: r for r in map(json.loads, source) if r}
    for record in records.values():
        version = record.get("digestFormat")
        if version is not None and (type(version) is not int or version != DIGEST_FORMAT):
            raise ValueError(f"{f}: unsupported answer fingerprint format {record['digestFormat']!r}")
    return records


def answers_equal(base, candidate):
    for record in (base, candidate):
        if type(record.get("digestFormat")) is not int or record["digestFormat"] != DIGEST_FORMAT or "error" in record:
            return None
        if not isinstance(record.get("queryType"), str) or record["queryType"] not in QUERY_KINDS or type(record.get("rows")) is not int or record["rows"] < 0:
            return None
        if not isinstance(record.get("digest"), str) or not re.fullmatch(r"[0-9a-f]{64}", record["digest"]):
            return None
    return all(base.get(key) == candidate.get(key) for key in ("queryType", "rows", "digest"))


def dist(vals):
    if not vals:
        return "n=0"
    v = sorted(vals)
    pick = lambda p: v[min(len(v) - 1, int(p * len(v)))]
    gm = math.exp(sum(math.log(x) for x in v) / len(v))
    return (f"n={len(v)} geomean={gm:.2f} median={pick(0.5):.2f} p90={pick(0.9):.2f} "
            f"p95={pick(0.95):.2f} max={v[-1]:.3g}")


def per_query(r, joins):
    qs = [qerr(n) for n in r.get("nodes", []) if (not joins or is_join(n))]
    qs = [q for q in qs if q is not None]
    if not qs:
        return None, None
    return max(qs), math.exp(sum(math.log(q) for q in qs) / len(qs))


def summary(a):
    for f in a.files:
        recs = load(f)
        for joins in (True, False):
            vals = [q for r in recs.values() for n in r.get("nodes", [])
                    if (not joins or is_join(n)) and (q := qerr(n)) is not None]
            print(f"{f} {'joins' if joins else 'all'}: {dist(vals)}")


def compare(a):
    b, c = load(a.base), load(a.cand)
    lines = []
    w = lines.append
    for joins in (True, False):
        kind = "join nodes" if joins else "all nodes"
        for name, recs in (("base", b), ("candidate", c)):
            vals = [q for r in recs.values() for n in r.get("nodes", [])
                    if (not joins or is_join(n)) and (q := qerr(n)) is not None]
            w(f"* {name}, {kind}: {dist(vals)}")
    w("")
    w("| query | base max | cand max | base gm | cand gm | base ms | cand ms | answers |")
    w("|---|---|---|---|---|---|---|---|")
    worse = []
    for q in sorted(set(b) | set(c)):
        rb, rc = b.get(q, {}), c.get(q, {})
        mb, gb = per_query(rb, True)
        mc, gc = per_query(rc, True)
        if mb is None:
            mb, gb = per_query(rb, False)
            mc, gc = per_query(rc, False)
        equal = answers_equal(rb, rc)
        same = "unverified (legacy or missing fingerprint)" if equal is None else "same" if equal else "DIFFER"
        if "error" in rb or "error" in rc:
            same = f"error {rb.get('error', '')[:40]} / {rc.get('error', '')[:40]}"
        fmt = lambda x: "-" if x is None else f"{x:.2f}"
        w(f"| {q} | {fmt(mb)} | {fmt(mc)} | {fmt(gb)} | {fmt(gc)} | "
          f"{fmt(rb.get('execMs'))} | {fmt(rc.get('execMs'))} | {same} |")
        if mb is not None and mc is not None and mc > mb * 1.0001:
            worse.append(q)
    w("")
    w(f"queries whose largest join q-error rose: {' '.join(worse) or 'none'}")
    # node by node where the plans have the same shape
    rose = []
    for q in sorted(set(b) & set(c)):
        nb, nc = b[q].get("nodes", []), c[q].get("nodes", [])
        if [(n["depth"], n["op"]) for n in nb] != [(n["depth"], n["op"]) for n in nc]:
            continue
        for x, y in zip(nb, nc):
            if not is_join(x):
                continue
            qx, qy = qerr(x), qerr(y)
            if qx is not None and qy is not None and qy > qx * 1.0001:
                rose.append(f"{q}:{x['id']} {qx:.2f}->{qy:.2f}")
    w(f"join nodes of unchanged plan shapes whose q-error rose: {', '.join(rose) or 'none'}")
    # the plan without the run-time notes in brackets and with the generated names of
    # aggregate variables, which differ from run to run, left out
    hidden = re.compile(r"\?[0-9a-f]{24,32}\b")
    sig = lambda r: [(n["depth"], n["op"], hidden.sub("?_", n["desc"].split(" [")[0]))
                     for n in r.get("nodes", [])]
    changed = [q for q in sorted(set(b) & set(c)) if sig(b[q]) != sig(c[q])]
    w(f"queries whose plans changed: {' '.join(changed) or 'none'}")
    text = "\n".join(lines)
    print(text)
    if a.md:
        pathlib.Path(a.md).write_text(text + "\n")


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--sparkles", required=True)
    r.add_argument("--loc", required=True)
    r.add_argument("--queries", required=True)
    r.add_argument("--out", required=True)
    r.add_argument("--port", type=int, default=29454)
    r.add_argument("--disable", default="")
    r.add_argument("--runs", type=int, default=2)
    r.add_argument("--timeout", type=int, default=300)
    r.add_argument("--threads", type=int, default=0)
    r.add_argument("--only", default="")
    s = sub.add_parser("summary")
    s.add_argument("files", nargs="+")
    c = sub.add_parser("compare")
    c.add_argument("base")
    c.add_argument("cand")
    c.add_argument("--md")
    a = p.parse_args()
    try:
        {"run": run, "summary": summary, "compare": compare}[a.cmd](a)
    except ValueError as error:
        p.error(str(error))


if __name__ == "__main__":
    main()
