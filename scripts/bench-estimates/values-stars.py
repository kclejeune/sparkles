#!/usr/bin/env python3
"""Write VALUES stars over the people of scripts/gen-data.py to a query directory.

    values-stars.py N_PEOPLE DIR

Each query joins VALUES of 1%, 3% or 10% of the people, drawn with a fixed seed, with
`foaf:name`, `foaf:age` and `rdf:type` (v-star3-<p>pct.rq). They exercise the join
estimates of a small input probed against the patterns it drives.
"""
import os
import random
import sys

people, out = int(sys.argv[1]), sys.argv[2]
os.makedirs(out, exist_ok=True)
P = ("PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> "
     "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> "
     "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ")
for pct, tag in [(0.01, "1"), (0.03, "3"), (0.1, "10")]:
    rnd = random.Random(7 + int(pct * 1000))
    keys = sorted(rnd.sample(range(people), int(people * pct)))
    vals = " ".join(f"<http://example.org/person/{i}>" for i in keys)
    q = f"SELECT * WHERE {{ VALUES ?s {{ {vals} }} ?s foaf:name ?n ; foaf:age ?a ; a ?t }}"
    with open(os.path.join(out, f"v-star3-{tag}pct.rq"), "w") as f:
        f.write(P + q)
