#!/usr/bin/env python3
"""Write the DBpedia benchmark queries (scripts/bench-billion/queries/*.rq).

    instantiate.py <sparql-endpoint> [--seed N]

The workload follows the DBpedia SPARQL Benchmark's approach (query templates mined from
the public endpoint's logs, instantiated with values from the data), with templates written
for this benchmark, plus QLever-style analytic queries over the whole dataset (counts,
group-bys, property paths, text filters).

Each template's parameter is drawn with a fixed seed from the sorted answers of its
parameter query, run against <sparql-endpoint>: run it on the smallest scale
(scripts/bench-billion.sh, SCALE=10m), whose sampled entities occur at every larger
scale. The parameter queries ask only for what the entities sampled at that scale say
(an entity's own triples), so the chosen values have answers at every scale. The generated files are checked in; rerun this only to change the workload.
"""

import json
import os
import random
import sys
import urllib.parse
import urllib.request

PREFIXES = """PREFIX dbo: <http://dbpedia.org/ontology/>
PREFIX dbr: <http://dbpedia.org/resource/>
PREFIX dct: <http://purl.org/dc/terms/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX geo: <http://www.w3.org/2003/01/geo/wgs84_pos#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
"""

# name: (parameter query selecting ?x, query with %X%)
TEMPLATES = {
    # every fact about an entity (the most common shape in DBpedia's logs)
    "entity-facts": (
        "SELECT DISTINCT ?x WHERE { ?x a dbo:Person ; dbo:birthPlace ?p ; rdfs:comment ?c }",
        "SELECT ?p ?o WHERE { %X% ?p ?o }",
    ),
    # an entity's label and English abstract, and its optional thumbnail
    "entity-summary": (
        "SELECT DISTINCT ?x WHERE { ?x a dbo:Film ; dbo:director ?d }",
        "SELECT ?l ?c ?t WHERE { %X% rdfs:label ?l OPTIONAL { %X% rdfs:comment ?c FILTER(LANGMATCHES(LANG(?c), \"en\")) } OPTIONAL { %X% dbo:thumbnail ?t } }",
    ),
    # people born in a place, with an optional death date
    "place-births": (
        "SELECT ?x WHERE { ?p dbo:birthPlace ?x ; a dbo:Person } GROUP BY ?x HAVING (COUNT(?p) >= 10)",
        "SELECT ?p ?b ?d WHERE { ?p dbo:birthPlace %X% ; dbo:birthDate ?b OPTIONAL { ?p dbo:deathDate ?d } }",
    ),
    # people born or died in a place
    "place-union": (
        "SELECT ?x WHERE { ?p dbo:deathPlace ?x ; a dbo:Person } GROUP BY ?x HAVING (COUNT(?p) >= 5)",
        "SELECT DISTINCT ?p WHERE { { ?p dbo:birthPlace %X% } UNION { ?p dbo:deathPlace %X% } }",
    ),
    # the people of a Wikipedia category, with their labels
    "category-people": (
        "SELECT ?x WHERE { ?p dct:subject ?x ; a dbo:Person } GROUP BY ?x HAVING (COUNT(?p) >= 5)",
        "SELECT ?p ?l WHERE { ?p dct:subject %X% ; a dbo:Person ; rdfs:label ?l }",
    ),
    # follow a redirect to its target's label and types
    "redirect-target": (
        "SELECT DISTINCT ?x WHERE { ?x dbo:wikiPageRedirects ?t . ?t a dbo:Person }",
        "SELECT ?t ?l ?c WHERE { %X% dbo:wikiPageRedirects ?t . ?t rdfs:label ?l ; a ?c }",
    ),
    # how many pages link to a well-linked page
    "inlinks-count": (
        "SELECT ?x WHERE { ?s dbo:wikiPageWikiLink ?x } GROUP BY ?x HAVING (COUNT(?s) >= 500)",
        "SELECT (COUNT(?s) AS ?c) WHERE { ?s dbo:wikiPageWikiLink %X% }",
    ),
    # the classes of the pages an article links to
    "outlink-classes": (
        "SELECT DISTINCT ?x WHERE { ?x a dbo:Scientist ; dbo:wikiPageWikiLink ?o }",
        "SELECT ?c (COUNT(?o) AS ?n) WHERE { %X% dbo:wikiPageWikiLink ?o . ?o a ?c } GROUP BY ?c ORDER BY DESC(?n) ?c LIMIT 10",
    ),
}
INSTANCES = 2

QUERIES = {
    "count-all": "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }",
    "predicate-counts": "SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
    "class-counts": "SELECT ?c (COUNT(?s) AS ?n) WHERE { ?s a ?c } GROUP BY ?c ORDER BY DESC(?n) ?c LIMIT 50",
    "top-linked": "SELECT ?o (COUNT(?s) AS ?c) WHERE { ?s dbo:wikiPageWikiLink ?o } GROUP BY ?o ORDER BY DESC(?c) ?o LIMIT 20",
    "births-by-decade": "SELECT ?decade (COUNT(?p) AS ?n) WHERE { ?p a dbo:Person ; dbo:birthDate ?d BIND(FLOOR(YEAR(?d) / 10) * 10 AS ?decade) } GROUP BY ?decade ORDER BY ?decade",
    "country-population": "SELECT ?c ?pop WHERE { ?c a dbo:Country ; dbo:populationTotal ?pop } ORDER BY DESC(?pop) ?c LIMIT 20",
    "film-director-optional": "SELECT (COUNT(*) AS ?n) (COUNT(?b) AS ?budgets) WHERE { ?f a dbo:Film ; dbo:director ?d OPTIONAL { ?f dbo:budget ?b } }",
    "abstract-contains": "SELECT (COUNT(*) AS ?n) WHERE { ?f a dbo:Film ; rdfs:comment ?c FILTER(CONTAINS(?c, \"Hitchcock\")) }",
    "label-regex": "SELECT ?s ?l WHERE { ?s a dbo:Band ; rdfs:label ?l FILTER(REGEX(?l, \"^The B\")) } ORDER BY ?l ?s",
    "geo-box": "SELECT (COUNT(*) AS ?n) WHERE { ?s geo:lat ?lat ; geo:long ?long FILTER(?lat > 48.0 && ?lat < 49.0 && ?long > 2.0 && ?long < 3.0) }",
    "people-no-birthdate": "SELECT (COUNT(*) AS ?n) WHERE { ?p a dbo:Person FILTER NOT EXISTS { ?p dbo:birthDate ?d } }",
    "category-tree": "SELECT (COUNT(DISTINCT ?c) AS ?n) WHERE { ?c skos:broader+ <http://dbpedia.org/resource/Category:Physics> }",
    "costar-birthplace": "SELECT ?a (COUNT(DISTINCT ?f) AS ?n) WHERE { ?f dbo:starring ?a ; dbo:director ?d . ?d dbo:birthPlace ?pl . ?a dbo:birthPlace ?pl } GROUP BY ?a ORDER BY DESC(?n) ?a LIMIT 10",
    "sameas-subjects": "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s owl:sameAs ?o }",
    "export-1m": "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 1000000",
}


def select(endpoint, query):
    body = urllib.parse.urlencode({"query": PREFIXES + query}).encode()
    req = urllib.request.Request(endpoint, body, {"Accept": "application/sparql-results+json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        rows = json.load(r)["results"]["bindings"]
    return sorted(b["x"]["value"] for b in rows if b.get("x", {}).get("type") == "uri")


def main():
    endpoint = sys.argv[1]
    seed = int(sys.argv[sys.argv.index("--seed") + 1]) if "--seed" in sys.argv else 42
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "queries")
    os.makedirs(out, exist_ok=True)
    for f in os.listdir(out):
        if f.endswith(".rq"):
            os.remove(os.path.join(out, f))
    for name, (param, template) in TEMPLATES.items():
        values = select(endpoint, param)
        if len(values) < INSTANCES:
            sys.exit(f"{name}: only {len(values)} values for the parameter")
        rng = random.Random(f"{seed}:{name}")
        for i, v in enumerate(rng.sample(values, INSTANCES), 1):
            q = template.replace("%X%", f"<{v}>")
            open(os.path.join(out, f"{name}-{i}.rq"), "w").write(f"# {name}, seed {seed}, from {len(values)} candidates\n{PREFIXES}{q}\n")
            print(f"{name}-{i}: {v}")
    for name, q in QUERIES.items():
        open(os.path.join(out, f"{name}.rq"), "w").write(f"{PREFIXES}{q}\n")


if __name__ == "__main__":
    main()
