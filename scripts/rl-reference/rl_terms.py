#!/usr/bin/env python3
"""Term encoding and closure comparison for the OWL 2 RL reference (owl2rl.rls).

encode IN.nt DIR
    Writes DIR/base.nt, the triples of IN.nt with each literal replaced by an IRI
    <urn:x-lit:N> and each blank node by an IRI <urn:x-bnode:N>, DIR/literals.nt, which
    marks each literal as one and gives the datatypes of OWL 2 RL whose value space holds each literal's value (the
    rule dt-type2) and marks the literals whose value is the number 1, and DIR/terms.tsv,
    which maps each encoded IRI back to the original term. Nemo normalizes literal values
    and renames blank nodes, so a closure compared term for term needs the encoding.

decode DIR DERIVED.nt OUT.nt
    Writes the triples of DERIVED.nt with the encoded IRIs of DIR/terms.tsv replaced by
    their original terms, sorted and without duplicates.

split QUADS.nq BASE.nt DERIVED.nt GRAPH
    Splits one N-Quads dump into the triples of the default graph and those of GRAPH,
    so that both come from one dump and share its blank node labels.

delta IN.nt EVERY DELTA.nt REST.nt INSERT.ru DELETE.ru
    Takes every EVERY-th triple of IN.nt whose property is outside the RDF, RDFS and OWL
    vocabularies and that has no blank node as the change of the incremental
    measurements of bench.sh. DELTA.nt gets those triples, REST.nt the others, and
    INSERT.ru and DELETE.ru the SPARQL updates that insert and delete them.

diff SPARKLES.nt REFERENCE.nt REPORT.json [--known KNOWN.tsv] [--subset]
    Compares two derived graphs triple for triple. Every difference must match a line of
    KNOWN.tsv, whose rows name a side (sparkles or reference), a regular expression over
    the N-Triples line, and the explanation. The command fails when a difference matches
    no row, and the report counts the differences by side, predicate and explanation.
    With --subset only the reference's triples that Sparkles lacks must be explained.
    Both graphs are sorted on disk beside REPORT.json and then merged, so the memory
    use stays near the sort buffer of GNU sort whatever the size of the graphs.

The N-Triples reader takes one triple per line, as `sparkles dump` and Nemo write them.
"""

import json
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter
from decimal import Decimal, InvalidOperation

XSD = "http://www.w3.org/2001/XMLSchema#"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
LIT = "urn:x-lit:"
BNODE = "urn:x-bnode:"
RL = "urn:x-rl:"

# N-Triples BLANK_NODE_LABEL: '_:' (PN_CHARS_U | [0-9]) ((PN_CHARS | '.')* PN_CHARS)?
PN_CHARS_BASE = (
    "A-Za-z\u00c0-\u00d6\u00d8-\u00f6\u00f8-\u02ff\u0370-\u037d\u037f-\u1fff\u200c-\u200d"
    "\u2070-\u218f\u2c00-\u2fef\u3001-\ud7ff\uf900-\ufdcf\ufdf0-\ufffd\U00010000-\U000effff"
)
PN_CHARS_U = PN_CHARS_BASE + "_:"
PN_CHARS = PN_CHARS_U + "\\-0-9\u00b7\u0300-\u036f\u203f-\u2040"
BLANK = rf"_:[{PN_CHARS_U}0-9](?:[{PN_CHARS}.]*[{PN_CHARS}])?"
TERM = re.compile(
    rf'\s*(<[^>]*>|{BLANK}|"(?:[^"\\]|\\.)*"(?:@[A-Za-z0-9-]+|\^\^<[^>]*>)?)(?=[\s.<"#_]|$)'
)
QUAD_END = re.compile(r"\s*\.\s*(#.*)?$")


def terms(line, n):
    """The first n terms of an N-Triples or N-Quads line."""
    out, pos = [], 0
    for _ in range(n):
        m = TERM.match(line, pos)
        if not m:
            raise ValueError(f"cannot read a term at column {pos}: {line!r}")
        out.append(m.group(1))
        pos = m.end()
    return out, line[pos:]


def triples(path):
    with open(path, encoding="utf-8") as f:
        for line in f:
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            (s, p, o), rest = terms(line, 3)
            if not QUAD_END.match(rest):
                raise ValueError(f"not a triple: {line!r}")
            yield s, p, o


ESC = {"t": "\t", "b": "\b", "n": "\n", "r": "\r", "f": "\f", '"': '"', "'": "'", "\\": "\\"}


def unescape(s):
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        e = s[i + 1]
        if e in ESC:
            out.append(ESC[e])
            i += 2
        elif e == "u":
            out.append(chr(int(s[i + 2 : i + 6], 16)))
            i += 6
        elif e == "U":
            out.append(chr(int(s[i + 2 : i + 10], 16)))
            i += 10
        else:
            raise ValueError(f"bad escape in {s!r}")
    return "".join(out)


def literal(term):
    """(lexical form, language or None, datatype IRI) of an N-Triples literal."""
    m = re.fullmatch(r'"((?:[^"\\]|\\.)*)"(?:@([A-Za-z0-9-]+)|\^\^<([^>]*)>)?', term)
    lex, lang, dt = unescape(m.group(1)), m.group(2), m.group(3)
    if lang:
        return lex, lang.lower(), RDF + "langString"
    return lex, None, dt or XSD + "string"


# ------------------------------------------------------------- value spaces (dt-type2)

INTEGER_RANGES = {
    "integer": (None, None),
    "nonNegativeInteger": (0, None),
    "nonPositiveInteger": (None, 0),
    "positiveInteger": (1, None),
    "negativeInteger": (None, -1),
    "long": (-(2**63), 2**63 - 1),
    "int": (-(2**31), 2**31 - 1),
    "short": (-(2**15), 2**15 - 1),
    "byte": (-(2**7), 2**7 - 1),
    "unsignedLong": (0, 2**64 - 1),
    "unsignedInt": (0, 2**32 - 1),
    "unsignedShort": (0, 2**16 - 1),
    "unsignedByte": (0, 2**8 - 1),
}
STRING_TYPES = ["normalizedString", "token", "language", "Name", "NCName", "NMTOKEN"]
OTHER_TYPES = ["hexBinary", "base64Binary", "anyURI", "boolean", "dateTime", "dateTimeStamp"]

NAME_START = r":A-Z_a-zÀ-ÖØ-öø-˿Ͱ-ͽͿ-῿‌-‍⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�\U00010000-\U000EFFFF"
NAME_CHAR = NAME_START + r"\-.0-9·̀-ͯ‿-⁀"
NAME = re.compile(f"[{NAME_START}][{NAME_CHAR}]*")
NMTOKEN = re.compile(f"[{NAME_CHAR}]+")
LANGUAGE = re.compile(r"[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*")
DECIMAL = re.compile(r"[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)")
INTEGER = re.compile(r"[+-]?[0-9]+")
FLOAT = re.compile(r"[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?|[+-]?INF|NaN")
HEX = re.compile(r"([0-9a-fA-F]{2})*")
# XSD 1.1 base64Binary: groups of four, the last of which may end in = or ==, with single
# spaces allowed between characters
B64 = r"[A-Za-z0-9+/] ?"
BASE64 = re.compile(
    rf"(({B64}){{4}})*((({B64}){{3}}[A-Za-z0-9+/])|(({B64}){{2}}[AEIMQUYcgkosw048] ?=)|({B64}[AQgw] ?= ?=))?"
)
DATETIME = re.compile(
    r"(-?(?:[1-9][0-9]{3,}|0[0-9]{3}))-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2}(\.[0-9]+)?)"
    r"(Z|[+-][0-9]{2}:[0-9]{2})?"
)


def collapse_ws(s):
    return re.sub(r"[ \t\n\r]+", " ", s).strip(" ")


def value(lex, lang, dt):
    """The literal's value as (space, value), or None when it is ill-typed or the
    datatype is not one of OWL 2 RL's."""
    if lang is not None:
        return ("langstring", (lex, lang))
    if dt == RDF + "PlainLiteral":
        at = lex.rfind("@")
        if at < 0:
            return None
        return ("string", lex[:at]) if at == len(lex) - 1 else ("langstring", (lex[:at], lex[at + 1 :].lower()))
    if not dt.startswith(XSD):
        return ("other", (dt, lex)) if dt == RDF + "XMLLiteral" else None
    local = dt[len(XSD) :]
    if local in INTEGER_RANGES:
        c = collapse_ws(lex)
        if not INTEGER.fullmatch(c):
            return None
        v = int(c)
        lo, hi = INTEGER_RANGES[local]
        if (lo is not None and v < lo) or (hi is not None and v > hi):
            return None
        return ("decimal", Decimal(v))
    if local == "decimal":
        c = collapse_ws(lex)
        if not DECIMAL.fullmatch(c):
            return None
        return ("decimal", Decimal(c))
    if local in ("float", "double"):
        c = collapse_ws(lex)
        if not FLOAT.fullmatch(c):
            return None
        return (local, c if c in ("INF", "+INF", "-INF", "NaN") else repr(float(c)))
    if local == "string":
        return ("string", lex)
    if local in STRING_TYPES:
        v = lex.replace("\t", " ").replace("\n", " ").replace("\r", " ")
        if local != "normalizedString":
            v = collapse_ws(v)
        if local in STRING_TYPES and string_in(v, local):
            return ("string", v)
        return None
    if local == "boolean":
        c = collapse_ws(lex)
        return ("boolean", c in ("true", "1")) if c in ("true", "false", "1", "0") else None
    if local in ("dateTime", "dateTimeStamp"):
        c = collapse_ws(lex)
        m = DATETIME.fullmatch(c)
        if not m or not valid_datetime(m) or (local == "dateTimeStamp" and not m.group(8)):
            return None
        return ("dateTime", (c, bool(m.group(8))))
    if local == "hexBinary":
        c = collapse_ws(lex)
        return (local, c.upper()) if HEX.fullmatch(c) else None
    if local == "base64Binary":
        c = collapse_ws(lex)
        return (local, c.replace(" ", "")) if BASE64.fullmatch(c) else None
    if local == "anyURI":
        return (local, collapse_ws(lex))
    return None


def valid_datetime(m):
    """The fields of a dateTime match within their ranges, with 24:00:00 for midnight. The
    seconds are compared as exact decimals."""
    year, month, day, hour, minute = (int(m.group(i)) for i in range(1, 6))
    second, tz = Decimal(m.group(6)), m.group(8)
    if not 1 <= month <= 12 or minute > 59 or second >= 60:
        return False
    leap = year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
    days = [31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1]
    if not 1 <= day <= days:
        return False
    if hour > 24 or (hour == 24 and (minute or second)):
        return False
    if tz and tz != "Z":
        th, tm = int(tz[1:3]), int(tz[4:6])
        if tm > 59 or th * 60 + tm > 14 * 60:
            return False
    return True


def string_in(s, local):
    if local == "normalizedString":
        return not any(c in s for c in "\t\n\r")
    if local == "token":
        return string_in(s, "normalizedString") and s == collapse_ws(s)
    if local == "language":
        return string_in(s, "token") and bool(LANGUAGE.fullmatch(s))
    if local == "NMTOKEN":
        return string_in(s, "token") and bool(NMTOKEN.fullmatch(s))
    if local == "Name":
        return string_in(s, "token") and bool(NAME.fullmatch(s))
    if local == "NCName":
        return string_in(s, "Name") and ":" not in s
    raise KeyError(local)


def datatypes(lex, lang, dt):
    """The datatypes of OWL 2 RL whose value space holds the literal's value, and
    whether that value is the number 1."""
    v = value(lex, lang, dt)
    if v is None:
        # a literal of a datatype that OWL 2 RL does not know is a member of rdfs:Literal
        known = dt.startswith(XSD) or dt in (RDF + "PlainLiteral", RDF + "XMLLiteral", RDF + "langString")
        return ([] if known else [RDFS + "Literal"]), False
    space, x = v
    out = [RDFS + "Literal"]
    one = False
    if space == "decimal":
        out.append(XSD + "decimal")
        if x == x.to_integral_value():
            n = int(x)
            for local, (lo, hi) in INTEGER_RANGES.items():
                if (lo is None or n >= lo) and (hi is None or n <= hi):
                    out.append(XSD + local)
        one = x == 1
    elif space in ("float", "double"):
        out.append(XSD + space)
    elif space == "string":
        out += [XSD + "string", RDF + "PlainLiteral"]
        out += [XSD + t for t in STRING_TYPES if string_in(x, t)]
    elif space == "langstring":
        out.append(RDF + "PlainLiteral")
    elif space == "boolean":
        out.append(XSD + "boolean")
    elif space == "dateTime":
        out.append(XSD + "dateTime")
        if x[1]:
            out.append(XSD + "dateTimeStamp")
    elif space == "other":
        out.append(x[0])
    else:
        out.append(XSD + space)
    return out, one


# ------------------------------------------------------------- commands


def encode(src, out):
    ids = {}
    lits = []

    def enc(term):
        if term[0] == "<":
            if unescape(term).startswith(("<" + LIT, "<" + BNODE)):
                raise ValueError(f"{src} holds the IRI {term}, which the encoding reserves")
            return term
        e = ids.get(term)
        if e is None:
            if term[0] == '"':
                e = f"<{LIT}{len(ids)}>"
                lits.append((e, term))
            else:
                e = f"<{BNODE}{len(ids)}>"
            ids[term] = e
        return e

    n = 0
    with open(f"{out}/base.nt", "w", encoding="utf-8") as b:
        for s, p, o in triples(src):
            b.write(f"{enc(s)} {enc(p)} {enc(o)} .\n")
            n += 1
    with open(f"{out}/literals.nt", "w", encoding="utf-8") as f:
        for term, e in ids.items():
            if term[0] == "_":
                f.write(f"{e} <{RL}kind> <{RL}bnode> .\n")
        for e, term in lits:
            dts, one = datatypes(*literal(term))
            f.write(f"{e} <{RL}kind> <{RL}literal> .\n")
            for dt in dts:
                f.write(f"{e} <{RL}inDatatype> <{dt}> .\n")
            if one:
                f.write(f"{e} <{RL}value> <{RL}one> .\n")
    with open(f"{out}/terms.tsv", "w", encoding="utf-8") as f:
        for term, e in ids.items():
            f.write(f"{e}\t{term}\n")
    print(f"encoded {n} triples, {len(lits)} literals, {len(ids) - len(lits)} blank nodes", file=sys.stderr)


def load_terms(d):
    m = {}
    with open(f"{d}/terms.tsv", encoding="utf-8") as f:
        for line in f:
            e, term = line.rstrip("\n").split("\t", 1)
            m[e] = term
    return m


def decode(d, src, dst):
    m = load_terms(d)

    def lines():
        for s, p, o in triples(src):
            for t in (s, p, o):
                if t.startswith(("<" + LIT, "<" + BNODE)) and t not in m:
                    raise ValueError(f"{src} has the encoded term {t}, which {d}/terms.tsv does not name")
            yield f"{m.get(s, s)} {m.get(p, p)} {m.get(o, o)} ."

    n = sort_unique(lines(), dst)
    print(f"decoded {n} triples", file=sys.stderr)


def split(src, base, derived, graph):
    g = f"<{graph}>"
    nb = nd = 0
    with open(src, encoding="utf-8") as f, open(base, "w", encoding="utf-8") as b, open(
        derived, "w", encoding="utf-8"
    ) as d:
        for line in f:
            if not line.strip():
                continue
            (s, p, o), rest = terms(line, 3)
            rest = rest.strip()
            if rest == ".":
                b.write(f"{s} {p} {o} .\n")
                nb += 1
                continue
            (gt,), rest2 = terms(rest, 1)
            if gt == g:
                d.write(f"{s} {p} {o} .\n")
                nd += 1
            else:
                raise ValueError(f"a quad in the graph {gt}, which is neither the default graph nor {g}")
    print(f"split {nb} base and {nd} derived triples", file=sys.stderr)


def delta(src, every, delta_out, rest_out, insert_ru, delete_ru):
    """Every EVERY-th triple of SRC whose property is outside the RDF, RDFS and OWL
    vocabularies and that has no blank node, as the change of the incremental
    measurements, with the rest of SRC and the SPARQL updates that insert and delete it."""
    every = int(every)
    vocab = ("<" + RDF, "<" + RDFS, "<http://www.w3.org/2002/07/owl#")
    n = 0
    chosen = []
    with open(rest_out, "w", encoding="utf-8") as rest:
        for s, p, o in triples(src):
            line = f"{s} {p} {o} ."
            if not p.startswith(vocab) and not s.startswith("_:") and not o.startswith("_:"):
                n += 1
                if n % every == 0:
                    chosen.append(line)
                    continue
            rest.write(line + "\n")
    with open(delta_out, "w", encoding="utf-8") as f:
        f.writelines(line + "\n" for line in chosen)
    for path, op in ((insert_ru, "INSERT"), (delete_ru, "DELETE")):
        with open(path, "w", encoding="utf-8") as f:
            f.write(f"{op} DATA {{\n")
            f.writelines(line + "\n" for line in chosen)
            f.write("}\n")
    print(f"chose {len(chosen)} of {n} eligible triples", file=sys.stderr)


# The buffer of each external sort. GNU sort spills to temporary files beside its output
# beyond it, so a comparison of two closures of tens of millions of triples needs about
# this much memory and no more.
SORT_BUFFER = "1G"


def sort_unique(lines, dst):
    """Writes the lines, each without its newline, to DST sorted by code point and without
    duplicates, and returns their number. DST appears only when the sort succeeds. GNU
    sort in the C locale orders UTF-8 bytewise, which is code point order, so the order is
    that of Python's sorted()."""
    part = dst + ".part"
    cmd = ["sort", "-u", "-S", SORT_BUFFER, "-T", os.path.dirname(os.path.abspath(dst)), "-o", part]
    try:
        with subprocess.Popen(cmd, stdin=subprocess.PIPE, env=dict(os.environ, LC_ALL="C")) as proc:
            with open(proc.stdin.fileno(), "w", encoding="utf-8", newline="\n", closefd=False) as w:
                for line in lines:
                    w.write(line + "\n")
            proc.stdin.close()
        if proc.returncode != 0:
            raise RuntimeError(f"sort ended with status {proc.returncode} while writing {dst}")
        os.replace(part, dst)
    finally:
        if os.path.exists(part):
            os.remove(part)
    n = 0
    with open(dst, "rb") as f:
        for _ in f:
            n += 1
    return n


def sorted_lines(path):
    with open(path, encoding="utf-8", newline="\n") as f:
        for line in f:
            yield line[:-1] if line.endswith("\n") else line


def merge(a, b):
    """Walks two sorted iterators of distinct lines and yields (side, line) for each line
    of only one of them, side 0 for A and 1 for B, and (None, line) for a line of both."""
    end = object()
    x, y = next(a, end), next(b, end)
    while x is not end or y is not end:
        if y is end or (x is not end and x < y):
            yield 0, x
            x = next(a, end)
        elif x is end or y < x:
            yield 1, y
            y = next(b, end)
        else:
            yield None, x
            x, y = next(a, end), next(b, end)


def diff(sparkles, reference, report, known_path=None, subset=False):
    known = []
    if known_path:
        with open(known_path, encoding="utf-8") as f:
            for line in f:
                if not line.strip() or line.startswith("#"):
                    continue
                side, pattern, why = line.rstrip("\n").split("\t")
                known.append((side, re.compile(pattern), why))

    # Each graph is sorted without duplicates into a file of its own, and the two files
    # are merged, so memory stays bounded by the sort buffer whatever the graphs' size.
    with tempfile.TemporaryDirectory(dir=os.path.dirname(os.path.abspath(report))) as tmp:
        sorted_paths = []
        for i, path in enumerate((sparkles, reference)):
            out = os.path.join(tmp, f"{i}.nt")
            sort_unique((f"{s} {p} {o} ." for s, p, o in triples(path)), out)
            sorted_paths.append(out)
        sides = ("sparkles", "reference")
        totals = {side: 0 for side in sides}
        common = 0
        by_why = {side: Counter() for side in sides}
        by_pred = {side: Counter() for side in sides}
        examples = {side: {} for side in sides}
        unexplained = {side: 0 for side in sides}
        shown = {side: [] for side in sides}
        for which, line in merge(*(sorted_lines(p) for p in sorted_paths)):
            if which is None:
                common += 1
                continue
            side = sides[which]
            totals[side] += 1
            pred = terms(line, 2)[0][1]
            by_pred[side][pred] += 1
            why = next((w for s, r, w in known if s == side and r.search(line)), None)
            if why is None:
                why = "unexplained"
                if not (subset and side == "sparkles"):
                    unexplained[side] += 1
                    if len(shown[side]) < 20:
                        shown[side].append(f"{side} only: {line}")
            by_why[side][why] += 1
            if len(examples[side].setdefault(why, [])) < 5:
                examples[side][why].append(line)
    na, nb = totals["sparkles"] + common, totals["reference"] + common
    result = {"sparkles": na, "reference": nb, "common": common, "subset": subset, "differences": {}}
    for side in sides:
        result["differences"][side] = {
            "total": totals[side],
            "by_explanation": dict(by_why[side].most_common()),
            "by_predicate": dict(by_pred[side].most_common()),
            "examples": examples[side],
        }
    n_unexplained = sum(unexplained.values())
    result["unexplained"] = n_unexplained
    with open(report, "w", encoding="utf-8") as f:
        json.dump(result, f, indent=1)
    print(
        f"sparkles {na}, reference {nb}, common {common}, "
        f"sparkles only {totals['sparkles']}, reference only {totals['reference']}, unexplained {n_unexplained}",
        file=sys.stderr,
    )
    for line in (shown["sparkles"] + shown["reference"])[:20]:
        print(line, file=sys.stderr)
    return 1 if n_unexplained else 0


def main(argv):
    cmd, args = argv[1], argv[2:]
    if cmd == "encode":
        encode(*args)
    elif cmd == "decode":
        decode(*args)
    elif cmd == "split":
        split(*args)
    elif cmd == "delta":
        delta(*args)
    elif cmd == "diff":
        known = None
        subset = "--subset" in args
        args = [a for a in args if a != "--subset"]
        if "--known" in args:
            i = args.index("--known")
            known = args[i + 1]
            args = args[:i] + args[i + 2 :]
        return diff(*args, known_path=known, subset=subset)
    else:
        raise SystemExit(__doc__)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
