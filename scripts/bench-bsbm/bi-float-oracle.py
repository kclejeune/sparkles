#!/usr/bin/env python3
"""Check engines' BSBM BI q4 and q5 answers against exact averages computed from the data.

    bi-float-oracle.py DATASET.nt QUERIES_DIR RAW_DIR [--json OUT]

Both queries average `xsd:float(xsd:string(?price))`. SPARQL 1.1 (section 18.5.1.4)
defines AVG as Sum(S) / Count(S), and Sum adds the values with op:numeric-add in an order
the specification leaves open. Each addition of two xsd:float values is rounded to xsd:float
(XPath 3.1 section 4.2), so engines that add in different orders, or that keep a wider
accumulator, legitimately return different floats. Their answers then differ beyond the 12
significant digits that scripts/bench-answers.py compares, and q4 can even rank rows
differently when two ratios are within that rounding.

This script parses the N-Triples once, computes for every instance of q4 and q5 the exact
rational mean of the float-rounded prices, and compares each engine's raw SPARQL JSON
answer (`<query>.<engine>.json`, as bench-answers.py writes it with ANSWERS_RAW_DIR) with
it. A value agrees when it is within the error bound of a float summation in any order,
n * 2^-24 of the sum of magnitudes plus one rounding of the quotient, which the bound
follows from the standard analysis of recursive summation. Integers (q5's review counts),
IRIs and the row set must match exactly, except that q4's top ten may differ at a rank
whose exact ratio is within the bound of the ratio that would replace it.
"""
import json
import re
import struct
import sys
from fractions import Fraction

TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
V = "http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/vocabulary/"
FEATURE, PRODUCT, PRICE = f"<{V}productFeature>", f"<{V}product>", f"<{V}price>"
REVIEW_FOR, COUNTRY = f"<{V}reviewFor>", f"<{V}country>"
REVIEWER = "<http://purl.org/stuff/rev#reviewer>"
EPS = Fraction(1, 2 ** 24)  # unit roundoff of binary32
LINE = re.compile(r'^(<[^>]*>|_:\S+) (<[^>]*>) (<[^>]*>|_:\S+|"(?:[^"\\]|\\.)*"(?:\^\^<[^>]*>|@\S+)?) \.$')


def f32(text):
    """xsd:float of a decimal lexical form: the nearest binary32, as an exact fraction."""
    return Fraction(struct.unpack("<f", struct.pack("<f", float(text)))[0])


def load(path, types):
    """The triples the two queries read, for the product types they name."""
    d = {"type": {t: set() for t in types}, "feature": {}, "offer_product": {}, "price": {},
         "review_for": {}, "reviewer": {}, "country": {}}
    want = {f"<{t}>": t for t in types}
    with open(path, encoding="utf-8") as f:
        for line in f:
            m = LINE.match(line.rstrip("\n"))
            if not m:
                continue
            s, p, o = m.group(1), m.group(2), m.group(3)
            if p == TYPE:
                if o in want:
                    d["type"][want[o]].add(s)
            elif p == FEATURE:
                d["feature"].setdefault(s, set()).add(o)
            elif p == PRODUCT:
                d["offer_product"][s] = o
            elif p == PRICE:
                d["price"][s] = f32(o[1:o.index('"', 1)])
            elif p == REVIEW_FOR:
                d["review_for"][s] = o
            elif p == REVIEWER:
                d["reviewer"][s] = o
            elif p == COUNTRY:
                d["country"][s] = o
    return d


class Mean:
    """The exact mean of float values and the bound of any float summation of them."""

    def __init__(self):
        self.total, self.mag, self.n = Fraction(0), Fraction(0), 0

    def add(self, x):
        self.total += x
        self.mag += abs(x)
        self.n += 1

    def value(self):
        return self.total / self.n

    def bound(self):
        # (n - 1) roundings of partial sums, one of the division by n
        return (self.n * EPS * self.mag) / self.n + EPS * abs(self.value())


def q4(d, ptype):
    products = d["type"][ptype]
    offers = {}
    for offer, product in d["offer_product"].items():
        if product in products and offer in d["price"]:
            offers.setdefault(product, []).append(d["price"][offer])
    features = set().union(*(d["feature"].get(p, set()) for p in products)) if products else set()
    rows = {}
    for feat in features:
        w, wo = Mean(), Mean()
        for product, prices in offers.items():
            has = feat in d["feature"].get(product, set())
            for x in prices:
                (w if has else wo).add(x)
        if w.n and wo.n:
            r = w.value() / wo.value()
            # relative bounds of numerator and denominator add, plus the rounding of the quotient
            rel = w.bound() / w.value() + wo.bound() / wo.value() + EPS
            rows[feat] = (r, r * rel)
    order = sorted(rows, key=lambda f: (-rows[f][0], f))
    return rows, order


def q5(d, ptype):
    products = d["type"][ptype]
    reviews = {}
    for review, product in d["review_for"].items():
        if product in products:
            country = d["country"].get(d["reviewer"].get(review))
            if country:
                reviews[(country, product)] = reviews.get((country, product), 0) + 1
    best = {}
    for (country, _), n in reviews.items():
        best[country] = max(best.get(country, 0), n)
    prices = {}
    for offer, product in d["offer_product"].items():
        if product in products and offer in d["price"]:
            prices.setdefault(product, Mean()).add(d["price"][offer])
    rows = {}
    for (country, product), n in reviews.items():
        if n == best[country] and product in prices:
            m = prices[product]
            rows[(country, product)] = (n, m.value(), m.bound())
    return rows


def term(b):
    if b is None:
        return None
    return f"<{b['value']}>" if b["type"] == "uri" else b["value"]


def check_q4(rows, order, answer, limit=10):
    got = [(term(b.get("feature")), Fraction(b["priceRatio"]["value"])) for b in answer["results"]["bindings"]]
    problems = []
    if len(got) != min(limit, len(order)):
        problems.append(f"{len(got)} rows, expected {min(limit, len(order))}")
    cutoff = rows[order[limit - 1]] if len(order) >= limit else None
    for feat, val in got:
        if feat not in rows:
            problems.append(f"{feat} is not a feature with both averages")
            continue
        exact, bound = rows[feat]
        if abs(val - exact) > bound:
            problems.append(f"{feat}: {float(val)!r} is {float(abs(val - exact) / bound):.1f} bounds from {float(exact)!r}")
        elif feat not in order[:limit] and not (cutoff and exact + bound >= cutoff[0] - cutoff[1]):
            problems.append(f"{feat} is outside the top {limit} by more than the rounding")
    return problems


def check_q5(rows, answer):
    problems, seen = [], set()
    for b in answer["results"]["bindings"]:
        key = (term(b.get("country")), term(b.get("product")))
        seen.add(key)
        if key not in rows:
            problems.append(f"{key} is not a country's most reviewed product")
            continue
        n, exact, bound = rows[key]
        if int(b["nrOfReviews"]["value"]) != n:
            problems.append(f"{key}: {b['nrOfReviews']['value']} reviews, expected {n}")
        val = Fraction(b["avgPrice"]["value"])
        if abs(val - exact) > bound:
            problems.append(f"{key}: {float(val)!r} is {float(abs(val - exact) / bound):.1f} bounds from {float(exact)!r}")
    for key in set(rows) - seen:
        problems.append(f"{key} is missing")
    return problems


def ptype_of(path):
    m = re.search(r"instances/(ProductType\d+)>", open(path).read())
    return "http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/" + m.group(1)


def main():
    data, qdir, raw = sys.argv[1:4]
    out = sys.argv[sys.argv.index("--json") + 1] if "--json" in sys.argv else None
    import glob
    import os
    names = sorted(os.path.basename(p)[:-3] for p in glob.glob(f"{qdir}/bi-q[45]-*.rq"))
    types = {n: ptype_of(f"{qdir}/{n}.rq") for n in names}
    d = load(data, set(types.values()))
    report = {}
    for n in names:
        if n.startswith("bi-q4"):
            rows, order = q4(d, types[n])
        else:
            rows = q5(d, types[n])
        for path in sorted(glob.glob(f"{raw}/{n}.*.json")):
            engine = os.path.basename(path)[len(n) + 1:-5]
            try:
                answer = json.load(open(path))
            except ValueError as e:
                report.setdefault(n, {})[engine] = {"status": "unreadable", "problems": [str(e)]}
                continue
            problems = check_q4(rows, order, answer) if n.startswith("bi-q4") else check_q5(rows, answer)
            var = "priceRatio" if n.startswith("bi-q4") else "avgPrice"
            # SPARQL gives xsd:float for an average or a quotient of xsd:float values
            dts = sorted({b[var].get("datatype", "") for b in answer["results"]["bindings"] if var in b})
            report.setdefault(n, {})[engine] = {"status": "ok" if not problems else "disagrees",
                                                "datatypes": dts, "problems": problems[:5]}
            print(f"{n} {engine}: {'ok' if not problems else '; '.join(problems[:3])} {types}")
    if out:
        json.dump(report, open(out, "w"), indent=1)
    sys.exit(1 if any(v["status"] != "ok" for r in report.values() for v in r.values()) else 0)


if __name__ == "__main__":
    main()
