#!/usr/bin/env python3
"""Compare the OWL 2 EL classifier's taxonomy with a reference reasoner's.

The reference taxonomy is the functional-style file that ELK's command line writes with
`-c -o FILE`: EquivalentClasses axioms for the nodes of several classes, with
owl:Thing and owl:Nothing in theirs, and SubClassOf axioms for the direct subsumptions.
The classifier's taxonomy is the file `el_classify --taxonomy` writes, with lines
`EQ c1 c2 ...` and `SUB c d`.

Both are reduced to the same form, a partition of the classes into nodes and the direct
edges between the nodes, and compared over the classes both name. The answer is a JSON
object with the counts and the first differences, and the exit status is 1 when they
differ.

    scripts/el-oracle.py REFERENCE.ofn OURS.txt

With `--asserted`, the first file is the ontology itself in functional-style syntax, and
the taxonomy is compared with its asserted SubClassOf axioms between named classes. Some
ontologies, such as the Gene Ontology, publish the direct hierarchy that ELK inferred as
asserted axioms. The answer counts the asserted subsumptions the taxonomy does not
entail and the direct edges of the taxonomy that are not asserted, up to equivalence.
Both are zero when the asserted hierarchy is the taxonomy's direct hierarchy.

    scripts/el-oracle.py --asserted ONTOLOGY.ofn OURS.txt
"""

import json
import re
import sys

THING = "http://www.w3.org/2002/07/owl#Thing"
NOTHING = "http://www.w3.org/2002/07/owl#Nothing"

TOKEN = re.compile(r"<([^>]*)>|([A-Za-z][\w.-]*)?:([\w.-]*)")


def terms(body, prefixes):
    out = []
    for m in TOKEN.finditer(body):
        if m.group(1) is not None:
            out.append(m.group(1))
        else:
            out.append(prefixes.get(m.group(2) or "", "") + m.group(3))
    return out


def read_reference(path):
    prefixes = {"owl": "http://www.w3.org/2002/07/owl#"}
    eqs, subs = [], []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            m = re.match(r"Prefix\(([\w.-]*):=<([^>]*)>\)", line)
            if m:
                prefixes[m.group(1)] = m.group(2)
                continue
            if line.startswith("EquivalentClasses("):
                eqs.append(terms(line[len("EquivalentClasses(") : -1], prefixes))
            elif line.startswith("SubClassOf("):
                t = terms(line[len("SubClassOf(") : -1], prefixes)
                if len(t) == 2:
                    subs.append((t[0], t[1]))
            elif line.startswith("Declaration(Class("):
                t = terms(line[len("Declaration(Class(") : -2], prefixes)
                if t:
                    eqs.append([t[0]])
    return eqs, subs


def read_ours(path):
    eqs, subs = [], []
    with open(path, encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            if parts[0] == "EQ":
                eqs.append(parts[1:])
            elif parts[0] == "SUB":
                subs.append((parts[1], parts[2]))
    return eqs, subs


def normalize(eqs, subs):
    """The nodes as frozensets and the edges between them, by node."""
    parent = {}

    def find(x):
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    for e in eqs:
        for c in e:
            find(c)
        for c in e[1:]:
            union(e[0], c)
    for a, b in subs:
        find(a)
        find(b)
    find(THING)
    find(NOTHING)
    groups = {}
    for c in list(parent):
        groups.setdefault(find(c), set()).add(c)
    node = {c: frozenset(groups[find(c)]) for c in parent}
    edges = set()
    for a, b in subs:
        na, nb = node[a], node[b]
        if na != nb and NOTHING not in na:
            edges.add((na, nb))
    return node, edges


def restrict(node, edges, keep):
    node = {c: frozenset(n & keep) for c, n in node.items() if c in keep}
    out = set()
    for a, b in edges:
        a2, b2 = frozenset(a & keep), frozenset(b & keep)
        if a2 and b2:
            out.add((a2, b2))
    return node, out


def read_asserted(path):
    """The SubClassOf axioms between two named classes of a functional-style file."""
    prefixes = {"owl": "http://www.w3.org/2002/07/owl#"}
    named = re.compile(r"^SubClassOf\((<[^>]+>|[\w.-]*:[\w.-]*) (<[^>]+>|[\w.-]*:[\w.-]*)\)$")
    subs = set()
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            m = re.match(r"Prefix\(([\w.-]*):=<([^>]*)>\)", line)
            if m:
                prefixes[m.group(1)] = m.group(2)
                continue
            m = named.match(line)
            if m:
                t = terms(m.group(1) + " " + m.group(2), prefixes)
                if len(t) == 2:
                    subs.add((t[0], t[1]))
    return subs


def compare_asserted(ontology, ours):
    asserted = read_asserted(ontology)
    eqs, subs = read_ours(ours)
    rep = {}
    for e in eqs:
        for c in e:
            rep[c] = e[0]
    r = lambda c: rep.get(c, c)  # noqa: E731
    parents = {}
    for a, b in subs:
        parents.setdefault(r(a), set()).add(r(b))
    memo = {}

    def ancestors(c):
        c = r(c)
        if c in memo:
            return memo[c]
        seen, stack = set(), list(parents.get(c, ()))
        while stack:
            x = stack.pop()
            if x not in seen:
                seen.add(x)
                stack.extend(parents.get(x, ()))
        memo[c] = seen
        return seen

    missing = sorted(
        (a, b) for a, b in asserted if b != THING and r(a) != r(b) and r(b) not in ancestors(a)
    )
    direct = {(r(a), r(b)) for a, b in subs if r(b) != THING}
    asserted_rep = {(r(a), r(b)) for a, b in asserted}
    extra = sorted(direct - asserted_rep)
    report = {
        "asserted": len(asserted),
        "direct": len(direct),
        "assertedNotEntailed": len(missing),
        "directNotAsserted": len(extra),
        "examples": {"missing": missing[:5], "extra": extra[:5]},
    }
    report["agree"] = not (missing or extra)
    print(json.dumps(report))
    sys.exit(0 if report["agree"] else 1)


def main():
    if sys.argv[1] == "--asserted":
        compare_asserted(sys.argv[2], sys.argv[3])
    ref_node, ref_edges = normalize(*read_reference(sys.argv[1]))
    our_node, our_edges = normalize(*read_ours(sys.argv[2]))
    common = set(ref_node) & set(our_node)
    only_ref = sorted(set(ref_node) - common)
    only_ours = sorted(set(our_node) - common)
    ref_node, ref_edges = restrict(ref_node, ref_edges, common)
    our_node, our_edges = restrict(our_node, our_edges, common)
    node_diff = sorted(c for c in common if ref_node[c] != our_node[c])
    unsat_ref = sorted(c for c in common if NOTHING in ref_node[c] and c != NOTHING)
    unsat_ours = sorted(c for c in common if NOTHING in our_node[c] and c != NOTHING)
    missing = ref_edges - our_edges
    extra = our_edges - ref_edges

    def show(e):
        return [sorted(e[0])[:3], sorted(e[1])[:3]]

    report = {
        "classes": len(common),
        "onlyReference": len(only_ref),
        "onlyOurs": len(only_ours),
        "nodes": len(set(ref_node.values())),
        "edges": len(ref_edges),
        "unsatisfiable": len(unsat_ref),
        "nodeDifferences": len(node_diff),
        "unsatisfiableDifferences": len(set(unsat_ref) ^ set(unsat_ours)),
        "missingEdges": len(missing),
        "extraEdges": len(extra),
        "examples": {
            "onlyReference": only_ref[:5],
            "onlyOurs": only_ours[:5],
            "nodes": [
                {"class": c, "reference": sorted(ref_node[c])[:5], "ours": sorted(our_node[c])[:5]}
                for c in node_diff[:5]
            ],
            "missing": [show(e) for e in sorted(missing, key=show)[:5]],
            "extra": [show(e) for e in sorted(extra, key=show)[:5]],
        },
    }
    report["agree"] = not (node_diff or missing or extra)
    print(json.dumps(report))
    sys.exit(0 if report["agree"] else 1)


if __name__ == "__main__":
    main()
