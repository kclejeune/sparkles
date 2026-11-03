#!/usr/bin/env python3
"""Judge one run of the classifier against a reference taxonomy (roadmap item 603).

The classifier's run is `el_classify --taxonomy OURS.txt`, with its exit status, its JSON
report and its standard error. The reference is HermiT's taxonomy, or Konclude's where
HermiT wrote none, in the functional-style form of `scripts/elk-compare`. The answer is
one JSON line with the outcome:

* `refused`: the reader refused the file, which is not a classification answer
* `timeout` and `failed`: the run did not end with an answer
* `no-reference`: neither reasoner wrote a taxonomy
* `equal`: the run left nothing out and its subsumptions are the reference's
* `differs`: the run left nothing out and its subsumptions are not the reference's
* `sound`: the run left axioms out or used some in part, and every subsumption it
  answers is entailed
* `unsound`: some subsumption the run answers is not entailed

    ours-compare.py --status S --name N --hermit H --konclude K --report R --stderr E \
        --taxonomy T
"""

import argparse
import json
import os
import re
import sys

THING = "http://www.w3.org/2002/07/owl#Thing"
NOTHING = "http://www.w3.org/2002/07/owl#Nothing"
IRI = re.compile(r"<([^>]*)>")


def nonempty(path):
    return path and os.path.exists(path) and os.path.getsize(path) > 0


def reference(path):
    """The told edges of a reference taxonomy, as a map from a class to its parents."""
    up = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            ns = IRI.findall(line)
            if line.startswith("SubClassOf(") and len(ns) == 2:
                up.setdefault(ns[0], set()).add(ns[1])
            elif line.startswith("EquivalentClasses("):
                for a in ns:
                    up.setdefault(a, set()).update(x for x in ns if x != a)
            elif line.startswith("Declaration(Class(") and ns:
                up.setdefault(ns[0], set())
    return up


def ours(path):
    up = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            p = line.split()
            if not p:
                continue
            if p[0] == "SUB" and len(p) == 3:
                up.setdefault(p[1], set()).add(p[2])
                up.setdefault(p[2], set())
            elif p[0] == "EQ":
                for a in p[1:]:
                    up.setdefault(a, set()).update(x for x in p[1:] if x != a)
    return up


class Closure:
    def __init__(self, up):
        self.up = up
        self.memo = {}
        self.inconsistent = NOTHING in self.above(THING)

    def above(self, c):
        if c in self.memo:
            return self.memo[c]
        # owl:Thing's superclasses and equivalents hold of every class
        seen = {c, THING}
        stack = [c, THING]
        while stack:
            x = stack.pop()
            for y in self.up.get(x, ()):
                if y not in seen:
                    seen.add(y)
                    stack.append(y)
        self.memo[c] = seen
        return seen

    def entails(self, a, b):
        if self.inconsistent or b == THING or a == NOTHING:
            return True
        s = self.above(a)
        return b in s or NOTHING in s


def unentailed(have, want):
    """The told edges of `have` that `want` does not entail; the closure of the told
    edges is entailed when each is."""
    out = []
    for a, bs in have.up.items():
        for b in bs:
            if not want.entails(a, b):
                out.append((a, b))
    return out


def main():
    p = argparse.ArgumentParser()
    for a in ["name", "hermit", "konclude", "report", "stderr", "taxonomy"]:
        p.add_argument("--" + a)
    p.add_argument("--status", type=int)
    a = p.parse_args()
    out = {"name": a.name, "status": a.status}
    if a.status != 0:
        err = open(a.stderr, encoding="utf-8", errors="replace").read() if nonempty(a.stderr) else ""
        if a.status == 124:
            out["outcome"] = "timeout"
        elif re.search(r"line \d+, column \d+: ", err):
            out["outcome"] = "refused"
            out["why"] = err.strip().splitlines()[-1][-300:] if err.strip() else ""
        else:
            out["outcome"] = "failed"
            out["why"] = err.strip()[-300:]
        print(json.dumps(out))
        return
    report = json.load(open(a.report))
    for k in ["outside", "rules", "partial", "tier", "outsideTaken", "conjunctions", "classifyMedianMs", "classes"]:
        if k in report:
            out[k] = report[k]
    ref = a.hermit if nonempty(a.hermit) else a.konclude if nonempty(a.konclude) else None
    if ref is None:
        out["outcome"] = "no-reference"
        print(json.dumps(out))
        return
    out["reference"] = "hermit" if ref == a.hermit else "konclude"
    want = Closure(reference(ref))
    have = Closure(ours(a.taxonomy))
    extra = unentailed(have, want)
    out["unentailed"] = len(extra)
    out["unentailedFirst"] = extra[:5]
    if extra:
        out["outcome"] = "unsound"
    elif report.get("outside", 0) or report.get("rules", 0) or report.get("partial"):
        out["outcome"] = "sound"
    else:
        missing = unentailed(want, have)
        out["missing"] = len(missing)
        out["missingFirst"] = missing[:5]
        out["outcome"] = "differs" if missing else "equal"
    print(json.dumps(out))


if __name__ == "__main__":
    sys.exit(main())
