#!/usr/bin/env python3
"""Classify an ontology with Konclude and write its taxonomy in el-oracle's reference form.

    konclude.py run --konclude BIN [--threads N] [--repeat N] [--taxonomy FILE]
        [--raw FILE] INPUT
    konclude.py convert [--input ONTOLOGY] [--reduce] RAW TAXONOMY

`run` starts `BIN classification -w N -v -i INPUT -o RAW` N times, one process each, and
prints one JSON line in the manner of ElkClassify's. wallMs is each process's wall time,
from the start of the process to its exit, and so covers the parse, the classification and
the written output. parseMs and totalMs are the parse time and the total processing time
that Konclude's own log reports, and classifyMs is their difference, the time from the
parsed ontology to the written hierarchy. peakRssKib is each process's peak resident set.
The class hierarchy of the last run stays in RAW, which defaults to TAXONOMY with the
suffix .konclude.owx, and `run` converts it into TAXONOMY.

`convert` reads Konclude's class hierarchy, in OWL/XML or functional-style syntax, and
writes the form that scripts/el-oracle.py reads and ElkClassify writes:
EquivalentClasses for each node of several classes, with the unsatisfiable classes in
the node of owl:Nothing, Declaration(Class) for each class of a node of its own and
SubClassOf from each node's first class to the first class of each of its direct
superclass nodes. Konclude may write an equivalence as subclass axioms both ways,
SubClassOf(owl:Thing A) for a class equivalent to the top class, SubClassOf(A
owl:Nothing) for an unsatisfiable class and SubClassOf(owl:Thing owl:Nothing) alone for an
inconsistent ontology, so the classes are grouped by the cycles of the subclass graph. A
class without a named superclass gets owl:Thing as its direct superclass. An inconsistent
ontology gets one node of every class, owl:Thing and owl:Nothing, with the classes that
INPUT declares when it is given, as the classifier of sparkles-owl writes it. --reduce
removes edges that other edges imply, for an output that is not the direct hierarchy.
"""

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

THING = "http://www.w3.org/2002/07/owl#Thing"
NOTHING = "http://www.w3.org/2002/07/owl#Nothing"
OWL_NS = "http://www.w3.org/2002/07/owl#"
TOKEN = re.compile(r"<([^>]*)>|([A-Za-z][\w.-]*)?:([\w.-]*)")


def expand(term, prefixes):
    m = TOKEN.fullmatch(term)
    if not m:
        return term
    if m.group(1) is not None:
        return m.group(1)
    return prefixes.get(m.group(2) or "", "") + m.group(3)


def read_owx(path):
    """Declared classes, subclass pairs and equivalence groups of an OWL/XML file."""
    declared, subs, eqs = set(), [], []
    prefixes = {"owl": OWL_NS}

    def local(tag):
        return tag.rsplit("}", 1)[-1]

    def iri(el):
        if "IRI" in el.attrib:
            return el.attrib["IRI"]
        if "abbreviatedIRI" in el.attrib:
            p, _, n = el.attrib["abbreviatedIRI"].partition(":")
            return prefixes.get(p, "") + n
        return None

    def classes(el):
        out = []
        for c in el:
            if local(c.tag) != "Class":
                return None
            i = iri(c)
            if i is None:
                return None
            out.append(i)
        return out

    for _, el in ET.iterparse(path, events=("end",)):
        tag = local(el.tag)
        if tag == "Prefix":
            prefixes[el.attrib.get("name", "")] = el.attrib.get("IRI", "")
        elif tag == "Declaration":
            for c in el:
                if local(c.tag) == "Class" and iri(c) is not None:
                    declared.add(iri(c))
            el.clear()
        elif tag == "SubClassOf":
            cs = classes(el)
            if cs and len(cs) == 2:
                subs.append((cs[0], cs[1]))
            el.clear()
        elif tag == "EquivalentClasses":
            cs = classes(el)
            if cs:
                eqs.append(cs)
            el.clear()
    return declared, subs, eqs


def read_ofn(path):
    """Declared classes, subclass pairs and equivalence groups of a functional-style file."""
    declared, subs, eqs = set(), [], []
    prefixes = {"owl": OWL_NS}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            m = re.match(r"Prefix\(([\w.-]*):=<([^>]*)>\)", line)
            if m:
                prefixes[m.group(1)] = m.group(2)
                continue
            m = re.match(r"(Declaration\(Class|SubClassOf|EquivalentClasses)\((.*)\)$", line)
            if not m:
                continue
            body = m.group(2)
            if m.group(1) == "Declaration(Class":
                body = body[:-1] if body.endswith(")") else body
            if "(" in body:
                continue
            ts = [expand(t.group(0), prefixes) for t in TOKEN.finditer(body)]
            if m.group(1) == "Declaration(Class" and len(ts) == 1:
                declared.add(ts[0])
            elif m.group(1) == "SubClassOf" and len(ts) == 2:
                subs.append((ts[0], ts[1]))
            elif m.group(1) == "EquivalentClasses" and ts:
                eqs.append(ts)
    return declared, subs, eqs


def read_hierarchy(path):
    with open(path, "rb") as f:
        head = f.read(512).lstrip()
    return read_owx(path) if head.startswith(b"<") else read_ofn(path)


def declared_classes(path):
    """The classes an ontology file declares, by the reader of its syntax."""
    return read_hierarchy(path)[0]


def sccs(graph):
    """The strongly connected components of a graph of adjacency sets, iteratively."""
    index, low, on, stack, out = {}, {}, set(), [], []
    counter = 0
    for root in graph:
        if root in index:
            continue
        work = [(root, iter(graph[root]))]
        index[root] = low[root] = counter
        counter += 1
        stack.append(root)
        on.add(root)
        while work:
            v, it = work[-1]
            advanced = False
            for w in it:
                if w not in index:
                    index[w] = low[w] = counter
                    counter += 1
                    stack.append(w)
                    on.add(w)
                    work.append((w, iter(graph[w])))
                    advanced = True
                    break
                if w in on:
                    low[v] = min(low[v], index[w])
            if advanced:
                continue
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[v])
            if low[v] == index[v]:
                comp = []
                while True:
                    w = stack.pop()
                    on.discard(w)
                    comp.append(w)
                    if w == v:
                        break
                out.append(comp)
    return out


def taxonomy(declared, subs, eqs, reduce=False, extra_classes=()):
    """(consistent, lines) of the taxonomy in el-oracle's reference form."""
    graph = {THING: set(), NOTHING: set()}

    def node(c):
        graph.setdefault(c, set())

    for c in declared:
        node(c)
    for a, b in subs:
        node(a)
        node(b)
        graph[a].add(b)
        if a == THING:
            # only a class equivalent to the top class is a superclass of it
            graph[b].add(a)
        if b == NOTHING:
            graph[b].add(a)
    for e in eqs:
        for c in e:
            node(c)
        for x, y in zip(e, e[1:] + e[:1]):
            graph[x].add(y)
    comps = sccs(graph)
    of = {}
    for i, comp in enumerate(comps):
        for c in comp:
            of[c] = i
    if of[THING] == of[NOTHING]:
        every = set(graph) | set(extra_classes) | {THING, NOTHING}
        return False, [equivalent(sorted(every))]
    parents = {}
    for v, ws in graph.items():
        for w in ws:
            a, b = of[v], of[w]
            if a != b:
                parents.setdefault(a, set()).add(b)
    first = [min(comp) for comp in comps]
    bottom, top = of[NOTHING], of[THING]
    if reduce:
        parents = reduced(parents)
    lines = []
    for i, comp in enumerate(comps):
        members = sorted(comp)
        lines.append(equivalent(members) if len(members) > 1 else f"Declaration(Class(<{members[0]}>))")
        if i in (bottom, top):
            continue
        ps = parents.get(i, set()) - {bottom}
        if len(ps) > 1:
            ps.discard(top)
        if not ps:
            ps = {top}
        for p in ps:
            lines.append(f"SubClassOf(<{first[i]}> <{first[p]}>)")
    return True, sorted(set(lines))


def reduced(parents):
    """The parents of each node without those that another parent's ancestors hold."""
    memo = {}

    def ancestors(n):
        if n in memo:
            return memo[n]
        out, stack = set(), list(parents.get(n, ()))
        while stack:
            x = stack.pop()
            if x not in out:
                out.add(x)
                stack.extend(parents.get(x, ()))
        memo[n] = out
        return out

    return {
        n: {p for p in ps if not any(p in ancestors(q) for q in ps if q != p)}
        for n, ps in parents.items()
    }


def equivalent(members):
    return "EquivalentClasses(" + " ".join(f"<{m}>" for m in members) + ")"


def convert(raw, out, input_path=None, reduce=False):
    declared, subs, eqs = read_hierarchy(raw)
    extra = declared_classes(input_path) if input_path else ()
    consistent, lines = taxonomy(declared, subs, eqs, reduce=reduce, extra_classes=extra)
    with open(out, "w", encoding="utf-8") as f:
        for line in lines:
            f.write(line + "\n")
    unsat = 0
    classes = set()
    for line in lines:
        names = re.findall(r"<([^>]*)>", line)
        if line.startswith(("EquivalentClasses", "Declaration")):
            classes.update(names)
            if line.startswith("EquivalentClasses") and NOTHING in names:
                unsat = len(names) - 1
    classes -= {THING, NOTHING}
    if not consistent:
        unsat = len(classes) + 1
    return consistent, len(classes), unsat


def log_ms(text, pattern):
    m = re.findall(pattern, text)
    return float(m[-1]) if m else None


def run(args):
    raw = args.raw or ((args.taxonomy or "konclude") + ".konclude.owx")
    wall, parse, total, rss = [], [], [], []
    for _ in range(args.repeat):
        cmd = [args.konclude, "classification", "-w", str(args.threads), "-v", "-i", args.input, "-o", raw]
        t0 = time.monotonic()
        p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log = p.stdout.read().decode("utf-8", "replace")
        _, status, usage = os.wait4(p.pid, 0)
        wall.append((time.monotonic() - t0) * 1000)
        p.returncode = os.waitstatus_to_exitcode(status)
        sys.stderr.write(log)
        if p.returncode != 0:
            print(f"Konclude exited with status {p.returncode}", file=sys.stderr)
            sys.exit(p.returncode if p.returncode > 0 else 1)
        rss.append(usage.ru_maxrss)
        parse.append(log_ms(log, r"[Oo]ntology parsed in (\d+(?:\.\d+)?) ms"))
        total.append(log_ms(log, r"Total processing time: (\d+(?:\.\d+)?) ms"))
    consistent, classes, unsat = convert(raw, args.taxonomy, args.input) if args.taxonomy else (None, None, None)
    classify = [t - p if t is not None and p is not None else None for t, p in zip(total, parse)]

    def med(xs):
        xs = [x for x in xs if x is not None]
        return round(statistics.median(xs), 1) if xs else None

    print(
        json.dumps(
            {
                "input": args.input,
                "reasoner": args.label,
                "threads": args.threads,
                "classes": classes,
                "consistent": consistent,
                "unsatisfiable": unsat,
                "wallMs": [round(x, 1) for x in wall],
                "parseMs": parse,
                "totalMs": total,
                "classifyMs": classify,
                "medianWallMs": med(wall),
                "medianParseMs": med(parse),
                "medianClassifyMs": med(classify),
                "peakRssKib": rss,
                "raw": raw,
            }
        )
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--konclude", required=True)
    r.add_argument("--label", default="Konclude")
    r.add_argument("--threads", type=int, default=os.cpu_count() or 1)
    r.add_argument("--repeat", type=int, default=1)
    r.add_argument("--taxonomy")
    r.add_argument("--raw")
    r.add_argument("input")
    c = sub.add_parser("convert")
    c.add_argument("--input")
    c.add_argument("--reduce", action="store_true")
    c.add_argument("raw")
    c.add_argument("taxonomy")
    args = ap.parse_args()
    if args.cmd == "run":
        if args.threads < 1 or args.repeat < 1:
            ap.error("--threads and --repeat take a positive integer")
        run(args)
    else:
        consistent, classes, unsat = convert(args.raw, args.taxonomy, args.input, args.reduce)
        print(json.dumps({"consistent": consistent, "classes": classes, "unsatisfiable": unsat}))


if __name__ == "__main__":
    main()
