#!/usr/bin/env python3
"""The ORE 2015 DL classification sample of roadmap item 603: what each ontology needs
of a DL reasoner, its stratum, and a development, a held-out and a final set.

The sample is the union of the `dl` and `pure_dl` classification tracks of
`ore2015_sample.zip` (ORE 2015 Reasoner Competition Corpus, Zenodo record 18578), 591
functional-style files. `scan` reads each file from the zip without extracting it and
writes one JSON line per ontology with its axiom counts, the constructors it uses, the
number of non-Horn axioms, a fingerprint of its logical axioms and the stratum that
follows. `select` deals the ontologies of each stratum and size band to the three sets in
turn, in the order of a hash of the file name, so the split is the same on every host,
and keeps the copies of one ontology that the corpus holds under several names, which
have the same fingerprint, in one set. `extract` writes the files of a list to a
directory, for the reasoners.

    ore-dl.py scan ZIP OUT.jsonl [--jobs N]
    ore-dl.py select SCAN.jsonl OUT.tsv
    ore-dl.py extract ZIP LIST DIR

An axiom is Horn when the clause it stands for has at most one disjunct, which the
polarity counts of Krötzsch, Rudolph and Hitzler (*Complexities of Horn Description
Logics*, ACM TOCL 2013) give: a disjunction or an enumeration of several individuals in
a positive position, a complement or a universal restriction in a negative position, a
maximum cardinality above one in a positive position and a minimum above one in a
negative position each add one. The data ranges count the same way, but a finite
datatype's implicit disjunction is not counted. The scan reads a file line by line, as
the OWL API writes one axiom per line. It counts the continuation lines of multi-line
literals it skips, and an axiom it cannot read leaves the file without a stratum.
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
# a constructor's name is written against its parenthesis, which tells it from an
# argument followed by a list, as the class of a key is
TOKEN = re.compile(
    r'[A-Za-z][\w]*\(|\(|\)|<[^>]*>|"(?:[^"\\]|\\.)*"(?:\^\^(?:<[^>]*>|[^\s()]+)|@[A-Za-z0-9-]+)?|[^\s()]+'
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
        if t.endswith("("):
            out = [t[:-1]]
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


class Unknown(ValueError):
    """A constructor or axiom the scan does not know, which it does not guess at."""


THING_ALL = THING | NOTHING
TOP_ROLE = ("owl:topObjectProperty", "<http://www.w3.org/2002/07/owl#topObjectProperty>")


def cap(n):
    return min(n, 2)


def at_most_pos(k, n):
    """The positive count of at most k fillers whose negative count is n: at most k is the
    negation of at least k+1, whose filler keeps its position, so zero is a universal
    restriction on the filler's complement, one adds an equality and more add several."""
    return n if k == 0 else (cap(1 + n) if k == 1 else 2)


def pairs(ns):
    """The most that two of the counts add up to, from the two largest, which is the
    largest clause of a set of pairwise disjointness conditions."""
    a = b = 0
    for n in ns:
        if n > a:
            a, b = n, a
        elif n > b:
            b = n
    return a + b


def cross(ps):
    """The largest clause of an equivalence: the negative count of one operand and the
    positive count of another."""
    best = [(-1, None), (-1, None)]
    for i, (p, _) in enumerate(ps):
        if p > best[0][0]:
            best = [(p, i), best[0]]
        elif p > best[1][0]:
            best[1] = (p, i)
    return max(n + (best[0][0] if best[0][1] != i else best[1][0]) for i, (_, n) in enumerate(ps))


def anonymous(tree):
    """Whether an axiom names an anonymous individual, which OWL 2 EL does not allow."""
    if isinstance(tree, list):
        return any(anonymous(x) for x in tree)
    return tree.startswith("_:")


# a literal's datatype, after the closing quote of its text
TYPED = re.compile(r'^"(?:[^"\\]|\\.)*"\^\^(.+)$')


def literal(t, out):
    """Records the datatype of a literal outside OWL 2 EL."""
    m = TYPED.match(t)
    if m and short(m.group(1)) not in EL_TYPES:
        out["f"].add("datatype_outside_el")


class Scan:
    """What one ontology's axioms use, and which of them are Horn.

    `pl` gives the polarity counts of Krötzsch, Rudolph and Hitzler: the number of
    disjuncts an expression adds to its axiom as a clause in a positive position and in a
    negative one, capped at 2. An axiom is Horn when its clause has at most one."""

    def __init__(self):
        self.f = Counter()  # feature counts
        self.nonhorn = 0
        self.nonel = 0
        self.nonel_classes = set()
        self.classes = set()

    def pl(self, e, out):
        """(positive, negative) counts of a class expression, with its features."""
        if not isinstance(e, list):
            if e in THING_ALL:
                return 0, 0
            self.classes.add(e)
            out["classes"].add(e)
            return 1, 0
        op, args = e[0], e[1:]
        if op == "ObjectIntersectionOf":
            ps = [self.pl(a, out) for a in args]
            return max(p for p, _ in ps), cap(sum(n for _, n in ps))
        if op == "ObjectUnionOf":
            out["f"].add("union")
            ps = [self.pl(a, out) for a in args]
            return cap(sum(p for p, _ in ps)), max(n for _, n in ps)
        if op == "ObjectComplementOf":
            out["f"].add("complement")
            p, n = self.pl(args[0], out)
            return n, p
        if op == "ObjectOneOf":
            out["f"].add("nominal")
            if len(args) > 1:
                out["f"].add("oneof_many")
            return cap(len(args)), 0
        if op == "ObjectSomeValuesFrom":
            self.role(args[0], out)
            p, n = self.pl(args[1], out)
            return max(1, p), n
        if op == "ObjectAllValuesFrom":
            self.role(args[0], out)
            out["f"].add("forall")
            p, n = self.pl(args[1], out)
            return p, max(1, n)
        if op == "ObjectHasValue":
            self.role(args[0], out)
            out["f"].add("hasvalue")
            return 1, 0
        if op == "ObjectHasSelf":
            self.role(args[0], out)
            out["f"].add("self")
            return 1, 0
        if op in ("ObjectMinCardinality", "ObjectMaxCardinality", "ObjectExactCardinality"):
            k = int(args[0])
            self.role(args[1], out)
            filler = args[2] if len(args) > 2 else "owl:Thing"
            out["f"].add("number" if isinstance(filler, str) and filler in THING else "qualified")
            p, n = self.pl(filler, out)
            return self.count(op, k, p, n, out, "")
        if op in ("DataSomeValuesFrom", "DataAllValuesFrom", "DataHasValue",
                  "DataMinCardinality", "DataMaxCardinality", "DataExactCardinality"):
            out["f"].add("data")
            if op == "DataHasValue":
                literal(args[1], out)
                return 1, 0
            card = op.startswith(("DataMin", "DataMax", "DataExact"))
            rest = args[2:] if card else args[1:]
            p, n = self.data(rest[-1], out) if rest else (1, 0)
            if op == "DataSomeValuesFrom":
                return max(1, p), n
            if op == "DataAllValuesFrom":
                out["f"].add("data_forall")
                return p, max(1, n)
            return self.count(op, int(args[0]), p, n, out, "data_")
        raise Unknown(op)

    def count(self, op, k, p, n, out, prefix):
        """The counts of a number restriction with at least or at most k fillers whose
        counts are p and n."""
        lo = op.endswith(("MinCardinality", "ExactCardinality"))
        hi = op.endswith(("MaxCardinality", "ExactCardinality"))
        pos, neg = 0, 0
        if lo:
            if prefix or k > 1:
                out["f"].add(prefix + ("card" if prefix else "min_many"))
            pos = max(pos, max(1, p) if k > 0 else 0)
            # at least k in a negative position is at most k-1 in a positive one
            neg += at_most_pos(k - 1, n) if k > 0 else 0
        if hi:
            out["f"].add(prefix + ("card" if prefix else "max"))
            pos = max(pos, at_most_pos(k, n))
            neg += max(1, p)
        return pos, cap(neg)

    def data(self, e, out):
        """(positive, negative) counts of a data range."""
        if not isinstance(e, list):
            if e.startswith('"'):
                literal(e, out)
                return 1, 0
            if short(e) not in EL_TYPES:
                out["f"].add("datatype_outside_el")
            return 1, 0
        op, args = e[0], e[1:]
        if op == "DataUnionOf":
            out["f"].add("data_union")
            ps = [self.data(a, out) for a in args]
            return cap(sum(p for p, _ in ps)), max(n for _, n in ps)
        if op == "DataIntersectionOf":
            ps = [self.data(a, out) for a in args]
            return max(p for p, _ in ps), cap(sum(n for _, n in ps))
        if op == "DataComplementOf":
            out["f"].add("data_complement")
            p, n = self.data(args[0], out)
            return n, p
        if op == "DataOneOf":
            for a in args:
                literal(a, out)
            if len(args) > 1:
                out["f"].add("data_oneof_many")
            return cap(len(args)), 0
        if op == "DatatypeRestriction":
            out["f"].add("facets")
            self.data(args[0], out)
            for a in args[1:]:
                if isinstance(a, str) and a.startswith('"'):
                    literal(a, out)
            return 1, 0
        raise Unknown(op)

    def role(self, r, out):
        if isinstance(r, list) and r and r[0] == "ObjectInverseOf":
            out["f"].add("inverse")
        elif isinstance(r, list):
            raise Unknown(str(r[0]))
        elif r in TOP_ROLE:
            out["f"].add("top_role")

    def fact(self, ax):
        """The features of an assertion between individuals, which is Horn."""
        f = set()
        if "ObjectInverseOf" in repr(ax):
            f.add("inverse")
        if anonymous(ax[1:]):
            f.add("anonymous")
        if ax[0] in ("DataPropertyAssertion", "NegativeDataPropertyAssertion"):
            literal(ax[-1], {"f": f})
        for x in f:
            self.f[x] += 1
        if f & NON_EL:
            self.nonel += 1

    def axiom(self, ax):
        out = {"f": set(), "classes": set()}
        op, args = ax[0], ax[1:]
        # each clause the axiom stands for, as the counts it adds
        clauses = []
        if op == "SubClassOf":
            _, n = self.pl(args[0], out)
            p, _ = self.pl(args[1], out)
            clauses.append(n + p)
        elif op in ("EquivalentClasses", "DisjointUnion"):
            parts = [self.pl(a, out) for a in args]
            if op == "DisjointUnion":
                out["f"].add("disjoint_union")
                out["f"].add("union")
                rest = parts[1:]
                union = (cap(sum(p for p, _ in rest)), max(n for _, n in rest))
                parts = [parts[0], union]
                clauses.append(pairs(n for _, n in rest))
            clauses.append(cross(parts))
        elif op == "DisjointClasses":
            out["f"].add("disjoint")
            parts = [self.pl(a, out) for a in args]
            clauses.append(pairs(n for _, n in parts))
        elif op == "ClassAssertion":
            out["f"].add("abox")
            clauses.append(self.pl(args[0], out)[0])
        elif op in ("ObjectPropertyDomain", "ObjectPropertyRange"):
            self.role(args[0], out)
            clauses.append(self.pl(args[1], out)[0])
        elif op == "DataPropertyDomain":
            clauses.append(self.pl(args[1], out)[0])
        elif op == "DataPropertyRange":
            out["f"].add("data")
            clauses.append(self.data(args[1], out)[0])
        elif op == "DatatypeDefinition":
            out["f"].add("data")
            p, n = self.data(args[1], out)
            clauses += [p, n + 1]
        elif op == "InverseObjectProperties":
            out["f"].add("inverse")
        elif op == "SymmetricObjectProperty":
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
            self.role(args[0], out)
        elif op in ("SubObjectPropertyOf", "EquivalentObjectProperties"):
            for a in args:
                if isinstance(a, list) and a[0] == "ObjectPropertyChain":
                    out["f"].add("chain")
                    for b in a[1:]:
                        self.role(b, out)
                else:
                    self.role(a, out)
            out["f"].add("role_hierarchy")
        elif op in ("FunctionalDataProperty", "SubDataPropertyOf", "EquivalentDataProperties"):
            out["f"].add("data")
        elif op == "DisjointDataProperties":
            out["f"].add("data")
            out["f"].add("role_disjoint")
        elif op == "HasKey":
            out["f"].add("key")
            # C(x) and C(y) with the key's values shared give x = y
            clauses.append(1 + 2 * self.pl(args[0], out)[1])
            if len(args) > 1 and isinstance(args[1], list):
                for r in args[1][1:]:
                    self.role(r, out)
        elif op == "DLSafeRule":
            out["f"].add("rule")
        else:
            raise Unknown(op)
        if anonymous(ax):
            out["f"].add("anonymous")
        for x in out["f"]:
            self.f[x] += 1
        if any(c > 1 for c in clauses):
            self.nonhorn += 1
        if out["f"] & NON_EL:
            self.nonel += 1
            self.nonel_classes |= out["classes"]


# the features that put an axiom outside OWL 2 EL
NON_EL = {
    "union", "complement", "oneof_many", "forall", "qualified", "number", "max",
    "min_many", "inverse", "symmetric", "functional", "role_disjoint", "disjoint_union",
    "data_forall", "data_card", "data_union", "data_complement", "data_oneof_many",
    "facets", "datatype_outside_el", "rule", "anonymous",
}
# the heads of the axioms of functional-style syntax, for telling an axiom the scan
# cannot read from the continuation line of a multi-line literal
AXIOMS = (
    "SubClassOf(", "EquivalentClasses(", "DisjointClasses(", "DisjointUnion(",
    "ClassAssertion(", "ObjectPropertyDomain(", "ObjectPropertyRange(",
    "DataPropertyDomain(", "DataPropertyRange(", "DatatypeDefinition(",
    "InverseObjectProperties(", "SymmetricObjectProperty(", "FunctionalObjectProperty(",
    "InverseFunctionalObjectProperty(", "AsymmetricObjectProperty(",
    "IrreflexiveObjectProperty(", "DisjointObjectProperties(", "TransitiveObjectProperty(",
    "ReflexiveObjectProperty(", "SubObjectPropertyOf(", "EquivalentObjectProperties(",
    "FunctionalDataProperty(", "SubDataPropertyOf(", "EquivalentDataProperties(",
    "DisjointDataProperties(", "HasKey(", "DLSafeRule(",
)


def stratum(f, nonhorn, unparsed):
    """The calculus an ontology needs, from the cheapest: OWL 2 EL, Horn, and then the
    non-Horn strata by the constructors whose interaction with case analysis needs rules
    of its own: inverses (I), counting (Q, with functionality) and nominals (O). An
    ontology with an axiom the scan could not read gets no stratum."""
    if unparsed:
        return "unread"
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


def line_hash(text):
    return int.from_bytes(hashlib.blake2b(text.encode(), digest_size=8).digest(), "little")


def scan_one(job):
    path, name = job
    z = zipfile.ZipFile(path)
    s = Scan()
    logical = facts = skipped = unparsed = 0
    first_skipped = first_unparsed = None
    # the logical content as a multiset of axioms, for finding copies of one ontology
    fingerprint = 0
    with z.open(name) as raw:
        for line in io.TextIOWrapper(raw, encoding="utf-8", errors="replace"):
            line = line.strip()
            if not line or line == ")" or line.startswith(SKIP):
                continue
            if line.startswith(FACTS):
                logical += 1
                facts += 1
                # the fingerprint of a fact is that of its tree without annotations, as
                # for the other axioms, and that of its first line when a literal goes on
                # over several lines
                try:
                    ax = parse(line)
                except (ValueError, IndexError):
                    fingerprint = (fingerprint + line_hash(line)) % (1 << 64)
                    continue
                fingerprint = (fingerprint + line_hash(repr(ax))) % (1 << 64)
                s.fact(ax)
                continue
            try:
                ax = parse(line)
                s.axiom(ax)
            except (ValueError, IndexError, TypeError):
                if line.startswith(AXIOMS):
                    unparsed += 1
                    first_unparsed = first_unparsed or line[:200]
                else:
                    skipped += 1
                    first_skipped = first_skipped or line[:200]
                continue
            logical += 1
            fingerprint = (fingerprint + line_hash(repr(ax))) % (1 << 64)
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
        "stratum": stratum(f, s.nonhorn > 0, unparsed),
        "band": size_band(logical),
        "fingerprint": f"{fingerprint:016x}",
        "skipped_lines": skipped,
        "first_skipped": first_skipped,
        "unparsed_axioms": unparsed,
        "first_unparsed": first_unparsed,
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


SETS = ("dev", "heldout", "final")


def cmd_select(scan, out):
    with open(scan) as f:
        rows = [json.loads(line) for line in f]
    # copies of one ontology, with the same logical axioms, go to the same set, and the
    # stratum of the first decides
    twins = {}
    for r in sorted(rows, key=lambda r: r["file"]):
        twins.setdefault((r["fingerprint"], r["logical"]), []).append(r)
    strata = {}
    for group in twins.values():
        strata.setdefault((group[0]["stratum"], group[0]["band"]), []).append(group)
    digest = lambda group: hashlib.sha256(("603:" + group[0]["file"]).encode()).hexdigest()
    with open(out, "w") as w:
        w.write("file\tset\tstratum\tband\ttracks\tlogical\tclasses\tnonel_axioms\tnonel_classes\tnonhorn_axioms\texpressivity\tbytes\ttwin_of\n")
        for key in sorted(strata):
            groups = sorted(strata[key], key=digest)
            # the groups of each stratum and band go to the three sets in turn, so each
            # set has about a third and the development set gets the first
            for i, group in enumerate(groups):
                for r in group:
                    w.write("\t".join(str(x) for x in (
                        r["file"], SETS[i % 3], r["stratum"], r["band"],
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
