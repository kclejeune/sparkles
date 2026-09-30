#!/usr/bin/env python3
"""Generate a synthetic N-Triples dataset (people, organisations, publications,
a small OWL class hierarchy) for smoke tests and benchmarks.

usage: gen-data.py N_PEOPLE > data.nt
"""
import random
import sys

n = int(sys.argv[1]) if len(sys.argv) > 1 else 10000
rnd = random.Random(42)
EX = "http://example.org/"
FOAF = "http://xmlns.com/foaf/0.1/"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
OWL = "http://www.w3.org/2002/07/owl#"
XSD = "http://www.w3.org/2001/XMLSchema#"
out = sys.stdout
w = out.write


def iri(x):
    return f"<{x}>"


def lit(s, dt=None, lang=None):
    s = s.replace("\\", "\\\\").replace('"', '\\"')
    if lang:
        return f'"{s}"@{lang}'
    if dt:
        return f'"{s}"^^<{XSD}{dt}>'
    return f'"{s}"'


def t(s, p, o):
    w(f"{s} {p} {o} .\n")


# ontology
classes = {
    "Agent": None, "Person": "Agent", "Organization": "Agent", "Employee": "Person",
    "Researcher": "Employee", "Manager": "Employee", "Student": "Person",
    "University": "Organization", "Company": "Organization", "Document": None,
    "Article": "Document", "Book": "Document",
}
for c, sup in classes.items():
    t(iri(EX + c), iri(RDF + "type"), iri(OWL + "Class"))
    t(iri(EX + c), iri(RDFS + "label"), lit(c, lang="en"))
    if sup:
        t(iri(EX + c), iri(RDFS + "subClassOf"), iri(EX + sup))
for p, d, r in [("worksFor", "Employee", "Organization"), ("authorOf", "Person", "Document"),
                ("advisor", "Student", "Researcher"), ("cites", "Document", "Document")]:
    t(iri(EX + p), iri(RDF + "type"), iri(OWL + "ObjectProperty"))
    t(iri(EX + p), iri(RDFS + "domain"), iri(EX + d))
    t(iri(EX + p), iri(RDFS + "range"), iri(EX + r))
t(iri(EX + "colleagueOf"), iri(RDF + "type"), iri(OWL + "SymmetricProperty"))
t(iri(EX + "managedBy"), iri(OWL + "inverseOf"), iri(EX + "manages"))

first = ["Ada", "Alan", "Grace", "Edsger", "Barbara", "Donald", "Leslie", "Frances", "John", "Margaret",
         "Tim", "Radia", "Ken", "Dennis", "Niklaus", "Shafi", "Judea", "Yoshua", "Fei-Fei", "Claude"]
last = ["Lovelace", "Turing", "Hopper", "Dijkstra", "Liskov", "Knuth", "Lamport", "Allen", "McCarthy",
        "Hamilton", "Berners-Lee", "Perlman", "Thompson", "Ritchie", "Wirth", "Goldwasser", "Pearl"]
cities = ["Kyoto", "Paris", "Berlin", "Boston", "Zurich", "Toronto", "Freiburg", "London", "Austin", "Oslo"]
n_orgs = max(10, n // 50)
for o in range(n_orgs):
    s = iri(f"{EX}org/{o}")
    t(s, iri(RDF + "type"), iri(EX + ("University" if o % 3 == 0 else "Company")))
    t(s, iri(FOAF + "name"), lit(f"Organization {o}"))
    t(s, iri(EX + "city"), lit(rnd.choice(cities)))
    t(s, iri(EX + "founded"), lit(f"{rnd.randint(1850, 2020)}-01-01", "date"))
n_docs = n // 2
for i in range(n):
    s = iri(f"{EX}person/{i}")
    kind = rnd.choices(["Researcher", "Manager", "Student", "Employee"], [3, 1, 3, 3])[0]
    t(s, iri(RDF + "type"), iri(EX + kind))
    t(s, iri(FOAF + "name"), lit(f"{rnd.choice(first)} {rnd.choice(last)} {i}"))
    t(s, iri(FOAF + "age"), lit(str(rnd.randint(18, 80)), "integer"))
    t(s, iri(EX + "salary"), lit(f"{rnd.randint(30000, 200000)}.{rnd.randint(0, 99):02d}", "decimal"))
    if kind != "Student":
        t(s, iri(EX + "worksFor"), iri(f"{EX}org/{rnd.randrange(n_orgs)}"))
    else:
        t(s, iri(EX + "advisor"), iri(f"{EX}person/{rnd.randrange(n)}"))
    for _ in range(rnd.randint(0, 5)):
        t(s, iri(FOAF + "knows"), iri(f"{EX}person/{rnd.randrange(n)}"))
    if rnd.random() < 0.3:
        t(s, iri(EX + "colleagueOf"), iri(f"{EX}person/{rnd.randrange(n)}"))
    if rnd.random() < 0.4:
        t(s, iri(EX + "authorOf"), iri(f"{EX}doc/{rnd.randrange(n_docs)}"))
for d in range(n_docs):
    s = iri(f"{EX}doc/{d}")
    t(s, iri(RDF + "type"), iri(EX + ("Article" if d % 4 else "Book")))
    t(s, iri(EX + "title"), lit(f"On the theory of topic {d % 997} ({d})", lang="en"))
    t(s, iri(EX + "year"), lit(str(rnd.randint(1950, 2026)), "integer"))
    for _ in range(rnd.randint(0, 3)):
        t(s, iri(EX + "cites"), iri(f"{EX}doc/{rnd.randrange(n_docs)}"))
