#!/usr/bin/env python3
"""The ORE 2015 DL classification sample of roadmap item 603: what each ontology needs
of a DL reasoner, its stratum, and a development and a held-out set.

The sample is the union of the `dl` and `pure_dl` classification tracks of
`ore2015_sample.zip` (ORE 2015 Reasoner Competition Corpus, Zenodo record 18578), 591
functional-style files. `scan` reads each file from the zip without extracting it and
writes one JSON line per ontology with its axiom counts, the constructors it uses, the
positions they take and the stratum that follows. `select` splits each stratum into a
development third and a held-out rest by a hash of the file name, so the split is the
same on every host, and keeps the copies of one ontology that the corpus holds under
several names in the same set. `extract` writes the files of a list to a directory, for the reasoners.

    ore-dl.py scan ZIP OUT.jsonl [--jobs N]
    ore-dl.py select SCAN.jsonl OUT.tsv
    ore-dl.py extract ZIP LIST DIR

The scan follows the polarity of each class expression. An axiom is Horn when no
disjunction is in a positive position and no universal restriction, maximum cardinality
or minimum cardinality above one is in a negative position (Krötzsch, Rudolph and
Hitzler, *Complexities of Horn Description Logics*, ACM TOCL 2013). It reads a file line
by line, as the OWL API writes one axiom per line, and counts the lines it cannot read.
"""

import csv
import hashlib
import io
import json
import os
import re
import sys
import zipfile
from collections import Counter
from concurrent.futures import ProcessPoolExecutor

TRACKS = ("dl", "pure_dl", "el")
TOKEN = re.compile(
    r'\(|\)|<[^>]*>|"(?:[^"\\]|\\.)*"(?:\^\^(?:<[^>]*>|[^\s()]+)|@[A-Za-z0-9-]+)?|[^\s()]+'
)
# axioms that carry no logical content or only assertions of named facts
SKIP = (
    "Declaration(",
    "AnnotationAssertion(",
    "SubAnnotationPropertyOf(",
    "AnnotationPropertyDomain(",
    "AnnotationPropertyRange(",
    "Prefix(",
    "Ontology(",
    "Import(",
)
FACTS = (
    "ObjectPropertyAssertion(",
    "DataPropertyAssertion(",
    "NegativeObjectPropertyAssertion(",
    "NegativeDataPropertyAssertion(",
    "SameIndividual(",
    "DifferentIndividuals(",
)
THING = {"owl:Thing", "<http://www.w3.org/2002/07/owl#Thing>"}
NOTHING = {"owl:Nothing", "<http://www.w3.org/2002/07/owl#Nothing>"}
# the datatypes of OWL 2 EL (Profiles §2.2.1)
EL_TYPES = {
    "rdf:PlainLiteral", "rdf:XMLLiteral", "rdfs:Literal", "owl:real", "owl:rational",
    "xsd:decimal", "xsd:integer", "xsd:nonNegativeInteger", "xsd:string",
    "xsd:normalizedString", "xsd:token", "xsd:Name", "xsd:NCName", "xsd:NMTOKEN",
    "xsd:hexBinary", "xsd:base64Binary", "xsd:anyURI", "xsd:dateTime", "xsd:dateTimeStamp",
}
NS = {
    "http://www.w3.org/2001/XMLSchema#": "xsd:",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#": "rdf:",
    "http://www.w3.org/2000/01/rdf-schema#": "rdfs:",
    "http://www.w3.org/2002/07/owl#": "owl:",
}


def short(t):
    if t.startswith("<"):
        for ns, p in NS.items():
            if t.startswith("<" + ns):
                return p + t[len(ns) + 1:-1]
    return t


def parse(line):
    """The tree of one axiom: a list whose head is the constructor's name."""
    toks = TOKEN.findall(line)
    at = 0

    def node():
        nonlocal at
        t = toks[at]
        at += 1
        if t == "(":
            # a list without a head, such as the properties of a key
            out = [""]
            while toks[at] != ")":
                out.append(node())
            at += 1
            return out
        if at < len(toks) and toks[at] == "(":
            at += 1
            out = [t]
            while toks[at] != ")":
                out.append(node())
            at += 1
            return out
        return t

    tree = node()
    if at != len(toks) or not isinstance(tree, list):
        raise ValueError("incomplete")
    return strip(tree)


def strip(node):
    """Drops the annotations of an axiom and of its parts."""
    if not isinstance(node, list):
        return node
    return [strip(x) for x in node if not (isinstance(x, list) and x and x[0] == "Annotation")]


class Scan:
    def __init__(self):
        self.f = Counter()  # feature counts
        self.nonhorn = 0
        self.nonel = 0
        self.nonel_classes = set()
        self.classes = set()

    # the features of a class expression in a position: +1 positive, -1 negative,
    # 0 both
    def cls(self, e, pol, out):
        if not isinstance(e, list):
            if (e.startswith("<") or ":" in e) and e not in THING and e not in NOTHING:
                self.classes.add(e)
                out["classes"].add(e)
            return
        op, args = e[0], e[1:]
        pos, neg = pol >= 0, pol <= 0
        if op == "ObjectIntersectionOf":
            for a in args:
                self.cls(a, pol, out)
        elif op == "ObjectUnionOf":
            out["f"].add("union")
            if pos and len(args) > 1:
                out["nonhorn"] = True
            for a in args:
                self.cls(a, pol, out)
        elif op == "ObjectComplementOf":
            out["f"].add("complement")
            self.cls(args[0], -pol, out)
        elif op == "ObjectOneOf":
            out["f"].add("nominal")
            if len(args) > 1:
                out["f"].add("oneof_many")
                if pos:
                    out["nonhorn"] = True
        elif op in ("ObjectSomeValuesFrom", "ObjectAllValuesFrom"):
            self.role(args[0], out)
            if op == "ObjectAllValuesFrom":
                out["f"].add("forall")
                if neg:
                    out["nonhorn"] = True
            self.cls(args[1], pol, out)
        elif op == "ObjectHasValue":
            self.role(args[0], out)
            out["f"].add("hasvalue")
        elif op == "ObjectHasSelf":
            self.role(args[0], out)
            out["f"].add("self")
        elif op in ("ObjectMinCardinality", "ObjectMaxCardinality", "ObjectExactCardinality"):
            n = int(args[0])
            self.role(args[1], out)
            qualified = len(args) > 2 and not (isinstance(args[2], str) and args[2] in THING)
            out["f"].add("qualified" if qualified else "number")
            lo = op in ("ObjectMinCardinality", "ObjectExactCardinality")
            hi = op in ("ObjectMaxCardinality", "ObjectExactCardinality")
            if hi:
                out["f"].add("max")
                if neg or (pos and n > 1):
                    out["nonhorn"] = True
            if lo and n > 1:
                out["f"].add("min_many")
                if neg:
                    out["nonhorn"] = True
            if len(args) > 2:
                # the filler of a minimum keeps the position, that of a maximum flips
                if lo:
                    self.cls(args[2], pol, out)
                if hi:
                    self.cls(args[2], -pol, out)
        elif op in ("DataSomeValuesFrom", "DataAllValuesFrom", "DataHasValue",
                    "DataMinCardinality", "DataMaxCardinality", "DataExactCardinality"):
            out["f"].add("data")
            if op == "DataAllValuesFrom":
                out["f"].add("data_forall")
                if neg:
                    out["nonhorn"] = True
            if op in ("DataMaxCardinality", "DataExactCardinality"):
                out["f"].add("data_card")
                if neg or (pos and int(args[0]) > 1):
                    out["nonhorn"] = True
            if op == "DataMinCardinality":
                out["f"].add("data_card")
            for a in args:
                if isinstance(a, list):
                    self.data(a, pol, out)
                elif short(a) in EL_TYPES or a.startswith(("xsd:", "<http://www.w3.org/2001")):
                    self.datatype(a, out)
        else:
            out["f"].add("unknown:" + op)

    def data(self, e, pol, out):
        op, args = e[0], e[1:]
        if op == "DataUnionOf":
            out["f"].add("data_union")
            if pol >= 0:
                out["nonhorn"] = True
        elif op == "DataComplementOf":
            out["f"].add("data_complement")
        elif op == "DataOneOf" and len(args) > 1:
            out["f"].add("data_oneof_many")
            if pol >= 0:
                out["nonhorn"] = True
        elif op == "DatatypeRestriction":
            out["f"].add("facets")
        for a in args:
            if isinstance(a, list):
                self.data(a, pol, out)
            elif not a.startswith('"'):
                self.datatype(a, out)

    def datatype(self, a, out):
        if not a.startswith('"') and short(a) not in EL_TYPES:
            out["f"].add("datatype_outside_el")

    def role(self, r, out):
        if isinstance(r, list) and r and r[0] == "ObjectInverseOf":
            out["f"].add("inverse")
        elif r in ("owl:topObjectProperty", "<http://www.w3.org/2002/07/owl#topObjectProperty>"):
            out["f"].add("top_role")

    def axiom(self, ax):
        out = {"f": set(), "nonhorn": False, "classes": set()}
        op, args = ax[0], ax[1:]
        if op == "SubClassOf":
            self.cls(args[0], -1, out)
            self.cls(args[1], 1, out)
        elif op == "EquivalentClasses":
            for a in args:
                self.cls(a, 0, out)
        elif op == "DisjointClasses":
            out["f"].add("disjoint")
            for a in args:
                self.cls(a, -1, out)
        elif op == "DisjointUnion":
            out["f"].add("disjoint_union")
            self.cls(args[0], 0, out)
            for a in args[1:]:
                self.cls(a, 0, out)
            if len(args) > 2:
                out["nonhorn"] = True
        elif op == "ClassAssertion":
            out["f"].add("abox")
            self.cls(args[0], 1, out)
        elif op in ("ObjectPropertyDomain", "ObjectPropertyRange"):
            self.role(args[0], out)
            self.cls(args[1], 1, out)
        elif op in ("DataPropertyDomain",):
            self.cls(args[1], 1, out)
        elif op == "DataPropertyRange":
            out["f"].add("data")
            if isinstance(args[1], list):
                self.data(args[1], 1, out)
            else:
                self.datatype(args[1], out)
        elif op == "InverseObjectProperties":
            out["f"].add("inverse")
        elif op in ("SymmetricObjectProperty",):
            out["f"].add("inverse")
            out["f"].add("symmetric")
        elif op == "FunctionalObjectProperty":
            out["f"].add("functional")
            self.role(args[0], out)
        elif op == "InverseFunctionalObjectProperty":
            out["f"].add("functional")
            out["f"].add("inverse")
        elif op in ("AsymmetricObjectProperty", "IrreflexiveObjectProperty",
                    "DisjointObjectProperties"):
            out["f"].add("role_disjoint")
            for a in args:
                self.role(a, out)
        elif op == "TransitiveObjectProperty":
            out["f"].add("transitive")
            self.role(args[0], out)
        elif op == "ReflexiveObjectProperty":
            out["f"].add("reflexive")
        elif op in ("SubObjectPropertyOf", "EquivalentObjectProperties"):
            for a in args:
                if isinstance(a, list) and a[0] == "ObjectPropertyChain":
                    out["f"].add("chain")
                    for b in a[1:]:
                        self.role(b, out)
                else:
                    self.role(a, out)
            out["f"].add("role_hierarchy")
        elif op in ("FunctionalDataProperty", "SubDataPropertyOf",
                    "EquivalentDataProperties", "DatatypeDefinition"):
            out["f"].add("data")
        elif op == "DisjointDataProperties":
            out["f"].add("data")
            out["f"].add("role_disjoint")
        elif op == "HasKey":
            out["f"].add("key")
            self.cls(args[0], -1, out)
        elif op == "DLSafeRule":
            out["f"].add("rule")
        else:
            out["f"].add("unknown:" + op)
        for x in out["f"]:
            self.f[x] += 1
        if out["nonhorn"]:
            self.nonhorn += 1
        if out["f"] & NON_EL:
            self.nonel += 1
            self.nonel_classes |= out["classes"]
        return out


# the features that put an axiom outside OWL 2 EL
NON_EL = {
    "union", "complement", "oneof_many", "forall", "qualified", "number", "max",
    "min_many", "inverse", "symmetric", "functional", "role_disjoint", "disjoint_union",
    "data_forall", "data_card", "data_union", "data_complement", "data_oneof_many",
    "facets", "datatype_outside_el", "rule",
}


def stratum(f, nonhorn):
    """The calculus an ontology needs, from the cheapest: OWL 2 EL, Horn, and then the
    non-Horn strata by the constructors whose interaction with case analysis needs rules
    of its own: inverses (I), counting (Q, with functionality) and nominals (O)."""
    if not (set(f) & NON_EL):
        return "el"
    if not nonhorn:
        return "horn"
    has = lambda *ks: any(f.get(k) for k in ks)
    i = has("inverse")
    q = has("qualified", "number", "max", "min_many", "functional", "data_card")
    o = has("nominal", "hasvalue")
    if o and i and q:
        return "dl-oiq"
    if o:
        return "dl-o"
    if q:
        return "dl-q"
    if i:
        return "dl-i"
    return "dl"


def size_band(n):
    for lim, band in ((1000, "s"), (10000, "m"), (100000, "l")):
        if n < lim:
            return band
    return "xl"


def scan_one(job):
    path, name = job
    z = zipfile.ZipFile(path)
    s = Scan()
    logical = facts = bad = 0
    first_bad = None
    with z.open(name) as raw:
        for line in io.TextIOWrapper(raw, encoding="utf-8", errors="replace"):
            line = line.strip()
            if not line or line == ")" or line.startswith(SKIP):
                continue
            if line.startswith(FACTS):
                logical += 1
                facts += 1
                continue
            try:
                s.axiom(parse(line))
            except (ValueError, IndexError, TypeError):
                bad += 1
                first_bad = first_bad or line[:200]
                continue
            logical += 1
    f = dict(s.f)
    return {
        "file": name.split("/")[-1],
        "logical": logical,
        "facts": facts,
        "classes": len(s.classes),
        "nonel_axioms": s.nonel,
        "nonel_classes": len(s.nonel_classes),
        "nonhorn_axioms": s.nonhorn,
        "features": dict(sorted(f.items())),
        "stratum": stratum(f, s.nonhorn > 0),
        "band": size_band(logical),
        "unread_lines": bad,
        "first_unread": first_bad,
    }


def tracks(z):
    out = {}
    for t in TRACKS:
        meta = z.read(f"pool_sample/{t}/classification/metadata.csv").decode()
        for r in csv.DictReader(io.StringIO(meta)):
            e = out.setdefault(r["ore2015_filename"], {"tracks": [], "meta": {}})
            e["tracks"].append(t)
            e["meta"] = {
                k: r[k] for k in ("expressivity", "owl2_el", "owl2_ql", "owl2_rl", "logical_axiom_count", "filesize")
            }
    return out


def cmd_scan(path, out, jobs=6):
    z = zipfile.ZipFile(path)
    meta = tracks(z)
    names = {i.filename.split("/")[-1]: i.filename for i in z.infolist() if "/files/" in i.filename}
    todo = sorted(
        (n for n, m in meta.items() if set(m["tracks"]) & {"dl", "pure_dl"}),
        key=lambda n: -z.getinfo(names[n]).file_size,
    )
    with ProcessPoolExecutor(jobs) as ex, open(out, "w") as w:
        for r in ex.map(scan_one, [(path, names[n]) for n in todo]):
            r.update(meta[r["file"]])
            r["bytes"] = z.getinfo(names[r["file"]]).file_size
            w.write(json.dumps(r, sort_keys=True) + "\n")
            w.flush()
            print(r["file"], r["stratum"], r["band"], file=sys.stderr)


def twin_key(r):
    """Files with the same counts of axioms, classes and constructors, which the corpus
    holds as copies of one ontology under several names."""
    return (r["logical"], r["classes"], r["nonel_axioms"], json.dumps(r["features"], sort_keys=True))


def cmd_select(scan, out):
    with open(scan) as f:
        rows = [json.loads(line) for line in f]
    # copies of one ontology go to the same set, and the stratum of the first decides
    twins = {}
    for r in sorted(rows, key=lambda r: r["file"]):
        twins.setdefault(twin_key(r), []).append(r)
    strata = {}
    for group in twins.values():
        strata.setdefault((group[0]["stratum"], group[0]["band"]), []).append(group)
    digest = lambda group: hashlib.sha256(("603:" + group[0]["file"]).encode()).hexdigest()
    with open(out, "w") as w:
        w.write("file\tset\tstratum\tband\ttracks\tlogical\tclasses\tnonel_axioms\tnonel_classes\tnonhorn_axioms\texpressivity\tbytes\ttwin_of\n")
        for key in sorted(strata):
            groups = sorted(strata[key], key=digest)
            # a third of each stratum's ontologies, and at least one, is for development
            dev = max(1, round(len(groups) / 3))
            for i, group in enumerate(groups):
                for r in group:
                    w.write("\t".join(str(x) for x in (
                        r["file"], "dev" if i < dev else "heldout", r["stratum"], r["band"],
                        ",".join(r["tracks"]), r["logical"], r["classes"], r["nonel_axioms"],
                        r["nonel_classes"], r["nonhorn_axioms"], r["meta"]["expressivity"], r["bytes"],
                        group[0]["file"] if r is not group[0] else "",
                    )) + "\n")


def cmd_extract(path, listing, d):
    z = zipfile.ZipFile(path)
    names = {i.filename.split("/")[-1]: i for i in z.infolist() if "/files/" in i.filename}
    os.makedirs(d, exist_ok=True)
    with open(listing) as f:
        lines = f.readlines()
    for line in lines:
        n = line.split("\t")[0].strip()
        if not n or n == "file":
            continue
        target = os.path.join(d, n)
        if os.path.exists(target) and os.path.getsize(target) == names[n].file_size:
            continue
        with z.open(names[n]) as src, open(target + ".part", "wb") as dst:
            while chunk := src.read(1 << 20):
                dst.write(chunk)
        os.replace(target + ".part", target)


def main(argv):
    if len(argv) >= 3 and argv[0] == "scan":
        jobs = int(argv[argv.index("--jobs") + 1]) if "--jobs" in argv else 6
        cmd_scan(argv[1], argv[2], jobs)
    elif len(argv) == 3 and argv[0] == "select":
        cmd_select(argv[1], argv[2])
    elif len(argv) == 4 and argv[0] == "extract":
        cmd_extract(argv[1], argv[2], argv[3])
    else:
        sys.stderr.write(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
