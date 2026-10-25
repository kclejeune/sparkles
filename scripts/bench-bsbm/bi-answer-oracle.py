#!/usr/bin/env python3
"""Check BI q6 score intervals and q7 legal LIMIT ties directly from N-Triples.

The verdict is numeric equivalence, not identical RDF datatypes, for q6. For q7 it
accepts every result obtainable by choosing a legal tied inner LIMIT boundary.
Fingerprints only bind the independently computed verdict to a captured response.
"""
import argparse
from collections import Counter
from fractions import Fraction
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import struct

V = b"<http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/vocabulary/"
TYPE = b"<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
PRODUCT, VENDOR, COUNTRY, PRODUCER, REVIEW_FOR = [V + x + b">" for x in
                                               [b"product", b"vendor", b"country", b"producer", b"reviewFor"]]
REVIEWER = b"<http://purl.org/stuff/rev#reviewer>"
RATINGS = {V + f"rating{i}>".encode() for i in range(1, 5)}
EPS = Fraction(1, 2**24)
NUMERIC = {"http://www.w3.org/2001/XMLSchema#" + x for x in ["float", "double", "decimal"]}


def binary32(value):
    return Fraction(struct.unpack("<f", struct.pack("<f", float(value)))[0])


QUERY_SHAPES = {
    "q6": "639a549bbbc6331c416b9211c516955396a3173085a9167dfe0cec5662962f4a",
    "q7": "122c3e1a98f65df60bd604da8b10126d4729db4cf10ee6aeb7e2bcc87bf721e6",
}


def query_shape(text):
    # Pin the generated template, allowing only its documented parameters to vary.
    text = re.sub(r"<http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/dataFromProducer\d+/Producer\d+>|"
                  r"<http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/ProductType\d+>|"
                  r"<http://downlode.org/rdf/iso-3166/countries#[^>]+>", "<PARAM>", text)
    text = re.sub(r"(?i)\bLIMIT\s+\d+", "limit PARAM", text)
    return hashlib.sha256(" ".join(text.lower().split()).encode()).hexdigest()


def parameters(text, kind, *, check_shape=True):
    if check_shape and query_shape(text) != QUERY_SHAPES[kind]:
        raise ValueError(f"unsupported BI {kind} query shape")
    def one(pattern):
        values = set(re.findall(pattern, text))
        if len(values) != 1:
            raise ValueError(f"expected one {pattern!r}, found {len(values)}")
        return values.pop()
    if kind == "q6":
        return {"producer": one(r"<http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/dataFromProducer\d+/Producer\d+>").encode()}
    return {"type": one(r"<http://www4.wiwiss.fu-berlin.de/bizer/bsbm/v01/instances/ProductType\d+>").encode(),
            "country": one(r"<http://downlode.org/rdf/iso-3166/countries#[^>]+>").encode(),
            "limit": int(one(r"(?i)\bLIMIT\s+(\d+)"))}


def triples(path, digest=None):
    with open(path, "rb") as source:
        for line in source:
            if digest is not None:
                digest.update(line)
            if not line.strip() or line.startswith(b"#"):
                continue
            if not line.rstrip().endswith(b" ."):
                raise ValueError("expected one N-Triples statement per line")
            yield line.rstrip().removesuffix(b" .").split(b" ", 2)


def inputs(path, specs):
    """Three streaming passes, retaining offers and only the selected reviews' scores."""
    producers = {p["producer"] for k, p in specs.values() if k == "q6"}
    classes = {p["type"] for k, p in specs.values() if k == "q7"}
    countries = {p["country"] for k, p in specs.values() if k == "q7"}
    products = {p: set() for p in producers}
    members = {t: set() for t in classes}
    vendors = {c: set() for c in countries}
    offers = {}
    digest = hashlib.sha256()
    for s, p, o in triples(path, digest):
        if p == TYPE and o in members:
            members[o].add(s)
        elif p == PRODUCER and o in products:
            products[o].add(s)
        elif p == COUNTRY and o in vendors:
            vendors[o].add(s)
        elif p == PRODUCT and classes:
            previous = offers.setdefault(s, o)
            if previous != o:
                raise ValueError("BSBM offer has more than one product")
    product_producers = {}
    for producer, items in products.items():
        for product in items:
            product_producers.setdefault(product, set()).add(producer)
    reviews = {}
    excluded = {c: set() for c in countries}
    for s, p, o in triples(path):
        if p == REVIEW_FOR and o in product_producers:
            reviews.setdefault(s, set()).add(o)
        elif p == VENDOR and s in offers:
            for country, values in vendors.items():
                if o in values:
                    excluded[country].add(offers[s])
    scores, reviewers = {}, {}
    if producers:
        for s, p, o in triples(path):
            if s in reviews:
                if p in RATINGS:
                    scores.setdefault(s, set()).add((p, o))
                elif p == REVIEWER:
                    reviewers.setdefault(s, set()).add(o)
    return {"source_sha256": digest.hexdigest(), "members": members,
            "counts": Counter(offers.values()), "excluded": excluded,
            "products": product_producers, "reviews": reviews,
            "scores": scores, "reviewers": reviewers}


class Mean:
    def __init__(self):
        self.total, self.mag, self.n = Fraction(0), Fraction(0), 0
        self.integral = True

    def add(self, value):
        self.total += value
        self.mag += abs(value)
        self.n += 1
        self.integral &= value.denominator == 1

    def interval(self):
        value = self.total / self.n
        # The usual forward bound for recursive binary32 summation, plus division.
        rounds = (self.n - 1) * EPS
        if rounds >= 1:
            raise ValueError("too many scores for the binary32 summation bound")
        error = rounds / (1 - rounds) * self.mag / self.n
        # Integer ratings whose partial sums fit binary32 have no sum rounding.
        if self.integral and self.mag <= 2**24 and self.n <= 2**24:
            error = Fraction(0)
        error += EPS * (abs(value) + error)
        return value - error, value + error


def q6_rows(data, producer, proof=None):
    overall = Mean()
    people = {}
    for review, products in data["reviews"].items():
        copies = sum(producer in data["products"][product] for product in products)
        if not copies:
            continue
        for _, literal in data["scores"].get(review, set()):
            match = re.fullmatch(rb'"([^"\\]*)"(?:\^\^<[^>]+>)?', literal)
            if not match:
                raise ValueError("unsupported BSBM rating literal")
            score = binary32(match[1].decode())
            for _ in range(copies):
                overall.add(score)
                for person in data["reviewers"].get(review, set()):
                    people.setdefault(person, Mean()).add(score)
    if not overall.n:
        return {}, set()
    lo, hi = overall.interval()
    threshold = (lo * Fraction(3, 2), hi * Fraction(3, 2))
    threshold = (threshold[0] - EPS * abs(threshold[0]), threshold[1] + EPS * abs(threshold[1]))
    rows, mandatory = {}, set()
    if proof is not None:
        proof.update({"producer": producer.decode(), "overall_sum": str(overall.total),
                      "overall_count": overall.n, "overall_mean": str(overall.total / overall.n),
                      "having_threshold_interval": [str(x) for x in threshold], "reviewers": {}})
    for person, mean in people.items():
        low, high = mean.interval()
        if high > threshold[0]:
            rows[person] = (low, high)
        if low > threshold[1]:
            mandatory.add(person)
        if proof is not None:
            proof["reviewers"][person.decode()] = {"sum": str(mean.total), "count": mean.n,
                "exact_mean": str(mean.total / mean.n), "interval": [str(low), str(high)],
                "qualification": "mandatory" if person in mandatory else "possible" if person in rows else "excluded"}
    return rows, mandatory


def check_q6(expected, answer):
    rows, mandatory = expected
    seen, problems = set(), []
    if sorted(answer.get("head", {}).get("vars", [])) != ["reviewer", "reviewerAvgScore"]:
        problems.append("unexpected projected variables")
    for binding in answer["results"]["bindings"]:
        person, score = binding.get("reviewer", {}), binding.get("reviewerAvgScore", {})
        key = ("<" + person.get("value", "") + ">").encode()
        if person.get("type") != "uri" or key not in rows:
            problems.append("unexpected reviewer or impossible HAVING result")
            continue
        if key in seen:
            problems.append("repeated reviewer")
        seen.add(key)
        if score.get("type") not in ("literal", "typed-literal") or score.get("datatype") not in NUMERIC:
            problems.append("score is not a numeric float, double or decimal")
            continue
        try:
            value = Fraction(score["value"])
        except (ValueError, KeyError, ZeroDivisionError):
            problems.append("score is not a finite number")
            continue
        low, high = rows[key]
        if not low <= value <= high:
            problems.append("score is outside the data-derived float summation interval")
    if mandatory - seen:
        problems.append("missing reviewer whose HAVING condition always holds")
    return problems


def q7_rows(data, spec):
    counts = data["counts"]
    eligible = {p for p in data["members"][spec["type"]] if counts[p]}
    if spec["limit"] == 0:
        above, boundary, slots, cutoff = set(), set(), 0, None
    elif len(eligible) <= spec["limit"]:
        above, boundary, slots, cutoff = eligible, set(), 0, None
    else:
        cutoff = sorted((counts[p] for p in eligible), reverse=True)[spec["limit"] - 1]
        above = {p for p in eligible if counts[p] > cutoff}
        boundary = {p for p in eligible if counts[p] == cutoff}
        slots = spec["limit"] - len(above)
    hidden = data["excluded"][spec["country"]]
    mandatory, possible = above - hidden, boundary - hidden
    low, high = max(0, slots - len(boundary & hidden)), min(slots, len(possible))
    return {"mandatory": mandatory, "boundary": possible, "low": low, "high": high,
            "cutoff_offer_count": cutoff, "above_cutoff": len(above), "boundary_products": len(boundary),
            "boundary_slots": slots, "hidden_boundary": len(boundary & hidden)}


def check_q7(expected, answer):
    bindings = answer["results"]["bindings"]
    products = set()
    problems = []
    if answer.get("head", {}).get("vars", []) != ["product"]:
        problems.append("unexpected projected variables")
    for row in bindings:
        product = row.get("product", {})
        if product.get("type") != "uri":
            problems.append("product is not an IRI")
        products.add(("<" + product.get("value", "") + ">").encode())
    if len(products) != len(bindings):
        problems.append("repeated product")
    if expected["mandatory"] - products:
        problems.append("omitted mandatory above-cutoff product")
    if products - (expected["mandatory"] | expected["boundary"]):
        problems.append("ineligible, below-cutoff or excluded-country product")
    if not expected["low"] <= len(products & expected["boundary"]) <= expected["high"]:
        problems.append("no legal cutoff selection produces this answer")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("data")
    parser.add_argument("queries")
    parser.add_argument("raw")
    parser.add_argument("--json", required=True)
    parser.add_argument("--names", nargs="*")
    parser.add_argument("--engines", nargs="*")
    args = parser.parse_args()
    paths = sorted(Path(args.queries).glob("bi-q[67]-*.rq"))
    if args.names is not None:
        paths = [p for p in paths if p.stem in args.names]
    specs = {p.stem: ("q6" if p.stem.startswith("bi-q6-") else "q7",
                      parameters(p.read_text(), "q6" if p.stem.startswith("bi-q6-") else "q7")) for p in paths}
    if not specs:
        Path(args.json).write_text(json.dumps({"version": 1, "queries": {}}) + "\n")
        return
    data = inputs(args.data, specs)
    module = importlib.util.spec_from_file_location("fingerprints", Path(__file__).parents[1] / "bench-answers.py")
    fingerprints = importlib.util.module_from_spec(module)
    module.loader.exec_module(fingerprints)
    report = {"version": 1, "source_sha256": data["source_sha256"], "queries": {}, "expected": {}}
    for path in paths:
        name, text = path.stem, path.read_text()
        kind, spec = specs[name]
        proof = {}
        expected = q6_rows(data, spec["producer"], proof) if kind == "q6" else q7_rows(data, spec)
        report["expected"][name] = proof if kind == "q6" else {
            **{k: v for k, v in expected.items() if k not in ("mandatory", "boundary")},
            "mandatory": sorted(p.decode() for p in expected["mandatory"]),
            "boundary": sorted(p.decode() for p in expected["boundary"]),
            "excluded_country": spec["country"].decode()}
        engines = {}
        for raw in sorted(Path(args.raw).glob(name + ".*.json")):
            engine = raw.name[len(name) + 1:-5]
            if args.engines is not None and engine not in args.engines:
                continue
            answer = json.loads(raw.read_text())
            problems = check_q6(expected, answer) if kind == "q6" else check_q7(expected, answer)
            engines[engine] = {"status": "ok" if not problems else "disagrees", "problems": problems,
                "policy": "binary32-summation-equivalence" if kind == "q6" else "legal-inner-limit-ties",
                "query_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "raw_sha256": hashlib.sha256(raw.read_bytes()).hexdigest(),
                "fingerprint": fingerprints.select_record(text, answer, False)}
            if kind == "q6":
                engines[engine]["datatypes"] = sorted({b.get("reviewerAvgScore", {}).get("datatype", "")
                    for b in answer["results"]["bindings"]})
        report["queries"][name] = engines
    Path(args.json).write_text(json.dumps(report, indent=1) + "\n")
    if not any(report["queries"].values()):
        parser.error("no captured answers were checked")


if __name__ == "__main__":
    main()
