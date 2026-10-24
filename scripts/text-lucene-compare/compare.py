#!/usr/bin/env python3
"""Runs the queries of the analyzer comparison against Fuseki and one or more Sparkles
datasets and reports where their hits differ.

    compare.py QUERIES.tsv FUSEKI_URL NAME=SPARKLES_URL... > report.md

Each query runs as text:query (rdfs:label "QUERY" "lang:LANG" 10000) and its hits are
compared as sets of subjects. A query that both engines refuse agrees. The report gives
the agreement per language and kind for each Sparkles dataset and lists every query on
which the first dataset differs from Fuseki.
"""

import collections
import json
import sys
import urllib.error
import urllib.parse
import urllib.request

PREFIX = (
    "PREFIX text: <http://jena.apache.org/text#> "
    "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> "
)


def run(url, q, lang):
    lit = q.replace("\\", "\\\\").replace('"', '\\"')
    sparql = (
        PREFIX + f'SELECT ?s WHERE {{ ?s text:query (rdfs:label "{lit}" "lang:{lang}" 10000) }}'
    )
    body = urllib.parse.urlencode({"query": sparql}).encode()
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "Accept": "application/sparql-results+json",
            "Content-Type": "application/x-www-form-urlencoded",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            res = json.load(r)
    except urllib.error.HTTPError as e:
        return ("error", e.code)
    return frozenset(b["s"]["value"].rsplit("/", 1)[1] for b in res["results"]["bindings"])


def show(r):
    if isinstance(r, tuple):
        return f"error {r[1]}"
    return f"{len(r)} hits"


def main():
    queries = [l.rstrip("\n").split("\t") for l in open(sys.argv[1], encoding="utf-8")]
    fuseki = sys.argv[2]
    sparkles = [a.split("=", 1) for a in sys.argv[3:]]
    agree = {name: collections.Counter() for name, _ in sparkles}
    total = collections.Counter()
    diffs = []
    for lang, kind, q in queries:
        f = run(fuseki, q, lang)
        total[lang] += 1
        total[kind] += 1
        for i, (name, url) in enumerate(sparkles):
            s = run(url, q, lang)
            same = s == f or (isinstance(s, tuple) and isinstance(f, tuple))
            if same:
                agree[name][lang] += 1
                agree[name][kind] += 1
                agree[name]["all"] += 1
            elif i == 0:
                detail = ""
                if not isinstance(s, tuple) and not isinstance(f, tuple):
                    only_f = sorted(f - s, key=int)[:5]
                    only_s = sorted(s - f, key=int)[:5]
                    detail = f"only Fuseki {only_f}, only Sparkles {only_s}"
                diffs.append((lang, kind, q, show(f), show(s), detail))
    n = len(queries)
    print("# Full-text analyzer comparison with Fuseki\n")
    print(f"{n} queries over {sys.argv[1]}.\n")
    keys = [k for k in ["en", "fr", "es", "it", "pt", "de", "nl", "ru", "sv"] if total[k]]
    kinds = sorted(k for k in total if k not in keys)
    print("| | " + " | ".join(name for name, _ in sparkles) + " |")
    print("|---|" + "---|" * len(sparkles))
    print("| all | " + " | ".join(f"{agree[name]['all']} of {n}" for name, _ in sparkles) + " |")
    for k in keys + kinds:
        print(
            f"| {k} | "
            + " | ".join(f"{agree[name][k]} of {total[k]}" for name, _ in sparkles)
            + " |"
        )
    print(f"\n## Differences of {sparkles[0][0]}\n")
    print("| lang | kind | query | Fuseki | Sparkles | detail |")
    print("|---|---|---|---|---|---|")
    for d in diffs:
        print("| " + " | ".join(str(x).replace("|", "\\|") for x in d) + " |")


if __name__ == "__main__":
    main()
