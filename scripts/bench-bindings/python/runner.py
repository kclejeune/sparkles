"""One process of the binding comparison (scripts/bench-bindings/bench.py) for Python.

    runner.py CONFIG.json

Engines:

* `sparkles`: the native `sparkles` API (`Dataset.memory()`);
* `pyoxigraph`: pyoxigraph's in-memory `Store`, the closest analogue;
* `rdflib-sparkles`: an rdflib `Graph` over `sparkles.rdflib.SparklesStore`;
* `rdflib-memory`: an rdflib `Graph` over rdflib's default `Memory` store.

The configuration and the JSON lines written to standard output are those of the JVM
runner (jvm/sparkles-jena/src/test/kotlin/io/github/kclejeune/sparkles/jena/bench/BindingsBench.kt).
Every row and term of a result is consumed. Sparkles' result cache is cleared before each
sample, outside the timed region, since the Python API has no per-query switch.
"""

from __future__ import annotations

import gc
import json
import signal
import sys
import time

RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
FOAF_NAME = "http://xmlns.com/foaf/0.1/name"
EX = "http://example.org/"
XSD_STRING = "http://www.w3.org/2001/XMLSchema#string"
XSD_BOOLEAN = "http://www.w3.org/2001/XMLSchema#boolean"
TRUE = {"type": "literal", "value": "true", "datatype": XSD_BOOLEAN}
FALSE = {"type": "literal", "value": "false", "datatype": XSD_BOOLEAN}


def emit(**kv):
    sys.stdout.write(json.dumps(kv) + "\n")
    sys.stdout.flush()


class Timeout(Exception):
    pass


def _alarm(_signum, _frame):
    raise Timeout()


# ---------------------------------------------------------------------- term encodings


def native_term(t):
    """A sparkles or pyoxigraph term as a SPARQL JSON results term (the APIs match)."""
    if t is None:
        return None
    name = type(t).__name__
    if name == "NamedNode":
        return {"type": "uri", "value": t.value}
    if name == "BlankNode":
        return {"type": "bnode", "value": t.value}
    if name == "Literal":
        if t.language:
            return {"type": "literal", "value": t.value, "xml:lang": t.language}
        return {"type": "literal", "value": t.value, "datatype": t.datatype.value}
    return {"type": "other", "value": str(t)}


def native_touch(t):
    if t is None:
        return 0
    if type(t).__name__ == "Literal":
        return len(t.value) + len(t.datatype.value) + len(t.language or "")
    return len(t.value)


def rdflib_term(t):
    import rdflib

    if t is None:
        return None
    if isinstance(t, rdflib.URIRef):
        return {"type": "uri", "value": str(t)}
    if isinstance(t, rdflib.BNode):
        return {"type": "bnode", "value": str(t)}
    if isinstance(t, rdflib.Literal):
        if t.language:
            return {"type": "literal", "value": str(t), "xml:lang": t.language}
        return {"type": "literal", "value": str(t), "datatype": str(t.datatype) if t.datatype else XSD_STRING}
    return {"type": "other", "value": str(t)}


def rdflib_touch(t):
    if t is None:
        return 0
    n = len(t)
    dt = getattr(t, "datatype", None)
    if dt is not None:
        n += len(dt)
    return n


class Sink:
    """Receives the rows of a case: sums the terms' lengths when timing, writes them as
    SPARQL JSON terms when checking answers."""

    def __init__(self, out, encode, touch):
        self.out, self.encode, self.touch = out, encode, touch
        self.rows = 0
        self.acc = 0

    def row(self, terms):
        self.rows += 1
        if self.out is None:
            for t in terms:
                self.acc += self.touch(t)
        else:
            self.out.write(json.dumps([self.encode(t) for t in terms]) + "\n")

    def flag(self, b):
        self.rows += 1
        if self.out is not None:
            self.out.write(json.dumps([TRUE if b else FALSE]) + "\n")


# ------------------------------------------------------------------------------ engines


class Native:
    """The native APIs: sparkles and pyoxigraph, which share the shape of pyoxigraph's."""

    encode = staticmethod(native_term)
    touch = staticmethod(native_touch)

    def __init__(self, cfg):
        self.cfg = cfg
        self.engine = cfg["engine"]
        if self.engine == "sparkles":
            import sparkles as m

            self.m = m
        else:
            import pyoxigraph as m

            self.m = m
        self.store = None

    def load(self):
        if self.engine == "sparkles":
            self.store = self.m.Dataset.memory()
            self.store.load_files([self.cfg["data"]])
        else:
            self.store = self.m.Store()
            self.store.load(path=self.cfg["data"], format=self.m.RdfFormat.N_TRIPLES)

    def size(self):
        return len(self.store)

    def before_sample(self):
        if self.engine == "sparkles":
            self.store.clear_cache()

    def iri(self, s):
        return self.m.NamedNode(s)

    def query(self, text, vars_, sink, deadline):
        if self.engine == "sparkles":
            res = self.store.query(text, timeout=max(0.001, deadline - time.monotonic()))
        else:
            res = self.store.query(text)
        if isinstance(res, bool):
            sink.flag(res)
            return
        for i, sol in enumerate(res):
            sink.row([_get(sol, v) for v in vars_])
            if i & 1023 == 0 and time.monotonic() > deadline:
                raise Timeout()

    def quads(self, s=None, p=None, o=None):
        return self.store.quads_for_pattern(s, p, o, None)

    def all(self):
        return iter(self.store)

    def contains(self, s, p, o):
        if self.engine == "sparkles":
            return self.m.Quad(s, p, o) in self.store
        return self.m.Quad(s, p, o, self.m.DefaultGraph()) in self.store

    def value(self, s, p):
        for q in self.store.quads_for_pattern(s, p, None, None):
            return q.object
        return None

    def adds(self, triples):
        m = self.m
        if self.engine == "sparkles":
            with self.store.transaction() as tx:
                for s, p, o in triples:
                    tx.add(m.Quad(m.NamedNode(s), m.NamedNode(p), m.Literal(o)))
        else:
            # pyoxigraph has no explicit transaction in Python; `extend` adds the quads
            # in one transaction
            self.store.extend(m.Quad(m.NamedNode(s), m.NamedNode(p), m.Literal(o), m.DefaultGraph()) for s, p, o in triples)

    def adds_bulk(self, triples):
        m = self.m
        if self.engine == "sparkles":
            self.store.extend(m.Quad(m.NamedNode(s), m.NamedNode(p), m.Literal(o)) for s, p, o in triples)
        else:
            self.store.bulk_extend(m.Quad(m.NamedNode(s), m.NamedNode(p), m.Literal(o), m.DefaultGraph()) for s, p, o in triples)


def _get(sol, v):
    try:
        return sol[v]
    except (KeyError, IndexError):
        return None


class Rdflib:
    """rdflib's Graph over the Sparkles store plugin or over rdflib's Memory store."""

    encode = staticmethod(rdflib_term)
    touch = staticmethod(rdflib_touch)

    def __init__(self, cfg):
        import rdflib

        self.rdflib = rdflib
        self.cfg = cfg
        self.engine = cfg["engine"]
        self.g = None

    def load(self):
        rdflib = self.rdflib
        if self.engine == "rdflib-sparkles":
            from sparkles.rdflib import SparklesStore

            self.g = rdflib.Graph(store=SparklesStore())
        else:
            self.g = rdflib.Graph()
        self.g.parse(self.cfg["data"], format="nt")

    def size(self):
        return len(self.g)

    def before_sample(self):
        if self.engine == "rdflib-sparkles":
            self.g.store.dataset.clear_cache()

    def iri(self, s):
        return self.rdflib.URIRef(s)

    def query(self, text, vars_, sink, deadline):
        res = self.g.query(text)
        if res.type == "ASK":
            sink.flag(bool(res.askAnswer))
            return
        for i, row in enumerate(res):
            d = row.asdict()
            sink.row([d.get(v) for v in vars_])
            if i & 1023 == 0 and time.monotonic() > deadline:
                raise Timeout()

    def quads(self, s=None, p=None, o=None):
        return self.g.triples((s, p, o))

    def all(self):
        return iter(self.g)

    def contains(self, s, p, o):
        return (s, p, o) in self.g

    def value(self, s, p):
        return self.g.value(s, p)

    def adds(self, triples):
        rdflib = self.rdflib
        g = self.g
        for s, p, o in triples:
            g.add((rdflib.URIRef(s), rdflib.URIRef(p), rdflib.Literal(o)))
        g.commit()

    def adds_bulk(self, triples):
        rdflib = self.rdflib
        self.g.addN((rdflib.URIRef(s), rdflib.URIRef(p), rdflib.Literal(o), self.g) for s, p, o in triples)
        self.g.commit()


def _triple_terms(q):
    # a sparkles or pyoxigraph Quad, or an rdflib triple
    if isinstance(q, tuple):
        return q[:3]
    return (q.subject, q.predicate, q.object)


# --------------------------------------------------------------------------------- cases


class Runner:
    def __init__(self, cfg, eng):
        self.cfg, self.eng = cfg, eng
        self.subjects = [eng.iri(s) for s in cfg["subjects"]]
        self.probes = [tuple(eng.iri(x) for x in p) for p in cfg["probes"]]
        self.name = eng.iri(FOAF_NAME)
        self.add_round = 0

    def run(self, case, sink, deadline):
        """One sample of a case; returns the number of operations it made."""
        eng, kind = self.eng, case["kind"]
        if kind == "query":
            eng.query(case["query"], case["vars"], sink, deadline)
            return 1
        if kind == "iter-all":
            for i, q in enumerate(eng.all()):
                sink.row(_triple_terms(q))
                if i & 65535 == 0 and time.monotonic() > deadline:
                    raise Timeout()
            return 1
        if kind == "pattern-s":
            for s in self.subjects:
                for q in eng.quads(s):
                    sink.row(_triple_terms(q))
            return len(self.subjects)
        if kind == "pattern-po":
            for q in eng.quads(None, eng.iri(RDF_TYPE), eng.iri(EX + "Researcher")):
                sink.row(_triple_terms(q))
            return 1
        if kind == "contains":
            for s, p, o in self.probes:
                sink.flag(eng.contains(s, p, o))
            return len(self.probes)
        if kind == "value":
            for s in self.subjects:
                sink.row([eng.value(s, self.name)])
            return len(self.subjects)
        if kind in ("adds", "adds-bulk"):
            r = self.add_round
            self.add_round += 1
            n = self.cfg["add_count"]
            triples = [(f"{EX}bench/add/{self.cfg['engine']}/{kind}/{r}/{i}", EX + "bench/p", f"v{i}") for i in range(n)]
            (eng.adds if kind == "adds" else eng.adds_bulk)(triples)
            sink.rows += n
            return n
        raise ValueError(f"unknown case kind {kind}")


def bounded(cfg, f):
    """Runs `f(deadline)` with the case timeout: an alarm interrupts Python code, and the
    loops check the deadline. A native call that does not return is left to the
    driver's watchdog, which resumes after the case."""
    timeout = float(cfg.get("timeout_s", 300))
    signal.setitimer(signal.ITIMER_REAL, timeout + 1)
    try:
        return f(time.monotonic() + timeout)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


def answers(cfg, runner):
    import os

    os.makedirs(cfg["answers_dir"], exist_ok=True)
    for case in cfg["cases"]:
        if case["name"] in cfg.get("skip", []) or case["kind"] in ("adds", "adds-bulk"):
            continue
        emit(event="start", case=case["name"])
        rec = {"event": "case", "case": case["name"]}
        path = os.path.join(cfg["answers_dir"], case["name"].replace(":", "_") + ".jsonl")
        try:
            with open(path, "w") as out:
                out.write(json.dumps({"vars": case.get("vars", [])}) + "\n")
                sink = Sink(out, runner.eng.encode, runner.eng.touch)
                runner.eng.before_sample()
                t = time.perf_counter()
                bounded(cfg, lambda d: runner.run(case, sink, d))
                rec.update(status="ok", rows=sink.rows, ms=(time.perf_counter() - t) * 1e3)
        except Timeout:
            rec.update(status="timeout")
        except Exception as e:  # noqa: BLE001 - a failure is recorded, not raised
            rec.update(status="error", error=repr(e)[:300])
        emit(**rec)


def timing(cfg, runner):
    warmup, runs = int(cfg.get("warmup", 2)), int(cfg.get("runs", 10))
    budget = float(cfg.get("budget_s", 120))
    for case in cfg["cases"]:
        if case["name"] in cfg.get("skip", []):
            continue
        emit(event="start", case=case["name"])
        rec = {"event": "case", "case": case["name"]}
        warm, samples, rows, ops = [], [], -1, 1
        begin = time.monotonic()
        try:
            i = 0
            while i < warmup + runs:
                sink = Sink(None, runner.eng.encode, runner.eng.touch)
                runner.eng.before_sample()
                gc.collect()
                t = time.perf_counter()
                ops = bounded(cfg, lambda d, s=sink: runner.run(case, s, d))
                ms = (time.perf_counter() - t) * 1e3
                rows = sink.rows
                (warm if i < warmup else samples).append(ms)
                emit(event="tick", case=case["name"])
                i += 1
                # past its budget, a case skips the rest of its warm-up and stops after
                # its first measured sample
                if time.monotonic() - begin > budget:
                    if samples:
                        break
                    i = max(i, warmup)
            rec["status"] = "ok"
        except Timeout:
            rec["status"] = "timeout"
        except Exception as e:  # noqa: BLE001
            rec.update(status="error", error=repr(e)[:300])
        rec.update(rows=rows, ops=ops, warmup_ms=warm, samples_ms=samples)
        emit(**rec)


def versions(engine):
    import platform

    v = {"python": platform.python_version(), "implementation": platform.python_implementation()}
    if engine in ("sparkles", "rdflib-sparkles"):
        import sparkles

        v["sparkles"] = getattr(sparkles, "__version__", "?")
    if engine == "pyoxigraph":
        import pyoxigraph

        v["pyoxigraph"] = pyoxigraph.__version__
    if engine.startswith("rdflib"):
        import rdflib

        v["rdflib"] = rdflib.__version__
    return v


def main():
    cfg = json.load(open(sys.argv[1]))
    signal.signal(signal.SIGALRM, _alarm)
    engine = cfg["engine"]
    emit(event="versions", versions=versions(engine))
    eng = Native(cfg) if engine in ("sparkles", "pyoxigraph") else Rdflib(cfg)
    try:
        t = time.perf_counter()
        eng.load()
        ms = (time.perf_counter() - t) * 1e3
        if cfg["mode"] == "load":
            emit(event="case", case="load", status="ok", rows=eng.size(), samples_ms=[ms])
        else:
            emit(event="ready", ms=ms, quads=eng.size())
            runner = Runner(cfg, eng)
            {"answers": answers, "time": timing}[cfg["mode"]](cfg, runner)
    except Exception as e:  # noqa: BLE001
        emit(event="fatal", error=repr(e)[:500])
        raise
    emit(event="done")


if __name__ == "__main__":
    main()
