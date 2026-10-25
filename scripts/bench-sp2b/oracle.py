#!/usr/bin/env python3
"""Reconstruct SP2Bench q4 and q6 from source triples, independently of a SPARQL engine.

The generator's names are xsd:string and years are xsd:integer. Refuse other terms
rather than extending those query-specific assumptions silently. q4 keeps only one
name's partner set in memory. The complete row hashes spill to disk before sorting.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import subprocess
import tempfile
from collections import defaultdict
from decimal import Decimal

HERE = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("benchmark_terms", HERE.parent / "bench-answers.py")
TERMS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TERMS)
XSD = "http://www.w3.org/2001/XMLSchema#"
RDF_TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
SUBCLASS = "<http://www.w3.org/2000/01/rdf-schema#subClassOf>"
DOCUMENT = "<http://xmlns.com/foaf/0.1/Document>"
ARTICLE = "<http://localhost/vocabulary/bench/Article>"
NAME = "<http://xmlns.com/foaf/0.1/name>"
CREATOR = "<http://purl.org/dc/elements/1.1/creator>"
ISSUED = "<http://purl.org/dc/terms/issued>"
JOURNAL = "<http://swrc.ontoware.org/ontology#journal>"


def digest_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def source_tables(source):
    tables = {key: defaultdict(set) for key in (RDF_TYPE, SUBCLASS, NAME, CREATOR, ISSUED, JOURNAL)}
    with open(source) as stream:
        for line in stream:
            fields = line.split(None, 2)
            if len(fields) < 3 or fields[1] not in tables:
                continue
            match = TERMS.NT_LINE.fullmatch(line.strip())
            if not match:
                raise ValueError("invalid source triple")
            subject, predicate, obj = match.groups()
            tables[predicate][subject].add(obj)
    names = {}
    for author, values in tables[NAME].items():
        parsed = [TERMS.term(TERMS.nt_term(value), False) for value in values]
        if any(value[0] != "lit" or value[2] != XSD + "string" for value in parsed):
            raise ValueError("the name oracle requires xsd:string")
        names[author] = {value[1] for value in parsed}
    years = {}
    for document, values in tables[ISSUED].items():
        parsed = [TERMS.term(TERMS.nt_term(value), False) for value in values]
        if any(value[0] != "lit" or value[2] != XSD + "integer" for value in parsed):
            raise ValueError("the year oracle requires xsd:integer")
        years[document] = parsed
    return tables, names, years


def q4_rows(tables, names):
    journals = defaultdict(set)
    for article, types in tables[RDF_TYPE].items():
        if ARTICLE not in types:
            continue
        people = {name for author in tables[CREATOR].get(article, ()) for name in names.get(author, ())}
        for journal in tables[JOURNAL].get(article, ()):
            journals[journal].update(people)
    memberships = defaultdict(list)
    for people in journals.values():
        for name in people:
            memberships[name].append(people)
    for first in sorted(memberships):
        partners = {second for people in memberships[first] for second in people if first < second}
        for second in partners:
            yield (("lit", first, XSD + "string"), ("lit", second, XSD + "string"))


def q6_rows(tables, names, years):
    classes = {cls for cls, bases in tables[SUBCLASS].items() if DOCUMENT in bases}
    documents = {doc: len(types & classes) for doc, types in tables[RDF_TYPE].items() if types & classes}
    earliest = {}
    for document in documents:
        for year in years.get(document, ()):
            value = int(year[1])
            for author in tables[CREATOR].get(document, ()):
                earliest[author] = min(value, earliest.get(author, value))
    for document, multiplicity in documents.items():
        doc_term = TERMS.term(TERMS.nt_term(document), False)
        if doc_term[0] != "uri":
            raise ValueError("the streamed q6 oracle requires IRI documents")
        for year in years.get(document, ()):
            for author in tables[CREATOR].get(document, ()):
                if int(year[1]) != earliest[author]:
                    continue
                for name in names.get(author, ()):
                    for _ in range(multiplicity):
                        yield (doc_term, ("lit", name, XSD + "string"), year)


def fingerprint(rows, directory):
    path = pathlib.Path(directory) / "rows"
    count = 0
    with open(path, "wb") as out:
        for row in rows:
            out.write(hashlib.sha256(repr(row).encode()).hexdigest().encode() + b"\n")
            count += 1
    h = hashlib.sha256()
    with subprocess.Popen(["sort", "--parallel=1", "-S", "64M", "-T", directory, str(path)],
                          stdout=subprocess.PIPE, env=dict(os.environ, LC_ALL="C")) as proc:
        for chunk in iter(lambda: proc.stdout.read(1 << 20), b""):
            h.update(chunk)
        proc.stdout.close()
        if proc.wait():
            raise ValueError("oracle row sorting failed")
    path.unlink()
    return {"rows": count, "exact": "row-sha256-v1:" + h.hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path)
    parser.add_argument("queries", type=pathlib.Path)
    parser.add_argument("out", type=pathlib.Path)
    args = parser.parse_args()
    tables, names, years = source_tables(args.source)
    record = {"version": 1, "method": "source-triple-reconstruction", "source_sha256": digest_file(args.source), "queries": {}}
    with tempfile.TemporaryDirectory(prefix="sp2b-oracle-") as directory:
        for query, rows in (("q4", q4_rows(tables, names)), ("q6", q6_rows(tables, names, years))):
            rec = fingerprint(rows, directory)
            # Require the canonical integer lexical forms used by the generator.
            if query == "q6" and any(str(int(year[1])) != year[1] for values in years.values() for year in values):
                raise ValueError("the generated years must have canonical integer lexical forms")
            # Numeric value normalization changes integer terms to ('num', value).
            # q6 therefore needs its own value pass rather than copying the exact digest.
            if query == "q6":
                value_rows = ((doc, name, ("num", str(Decimal(year[1]).normalize()))) for doc, name, year in q6_rows(tables, names, years))
                rec["value"] = fingerprint(value_rows, directory)["exact"]
            else:
                rec["value"] = rec["exact"]
            rec["query_sha256"] = digest_file(args.queries / (query + ".rq"))
            record["queries"][query] = rec
            print(query, rec, flush=True)
    args.out.write_text(json.dumps(record, indent=2) + "\n")


if __name__ == "__main__":
    main()
