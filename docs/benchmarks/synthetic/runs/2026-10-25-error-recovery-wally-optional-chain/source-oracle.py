"""Reconstruct the stock optional-chain bag from exact source graph relations."""
import collections
import hashlib
import importlib.util
import json
import pathlib
import sys

source, query, answers_script, output = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('answers', answers_script)
a = importlib.util.module_from_spec(spec)
spec.loader.exec_module(a)
ex = '<http://example.org/'
rdf_type = '<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>'
advisor, works, city = (ex + name + '>' for name in ('advisor', 'worksFor', 'city'))
relations = {p: collections.defaultdict(set) for p in (advisor, works, city)}
students = set()
digest = hashlib.sha256()
with source.open('rb') as f:
    for raw in f:
        digest.update(raw)
        line = raw.decode()
        fields = line.split(None, 2)
        if len(fields) != 3 or fields[1] not in (*relations, rdf_type):
            continue
        match = a.NT_LINE.fullmatch(line.strip())
        assert match, 'source triple must parse completely'
        subject, predicate, obj = match.groups()
        if predicate == rdf_type:
            if obj == ex + 'Student>':
                students.add(subject)
        else:
            relations[predicate][subject].add(obj)
rows = []
for person in students:
    matches = [(organization, c) for teacher in relations[advisor].get(person, ())
               for organization in relations[works].get(teacher, ())
               for c in relations[city].get(organization, {None})]
    if not matches:
        matches = [(None, None)]
    for organization, c in matches:
        # Projected variables are c,o,p in canonical alphabetical order.
        rows.append(tuple(a.nt_term(term) if term is not None else None for term in (c, organization, person)))
result = {'oracle_version': 1, 'method': 'source-relations-nested-left-join-bag',
          'source_sha256': digest.hexdigest(), 'query_sha256': hashlib.sha256(query.read_bytes()).hexdigest(),
          'students': len(students), 'rows': len(rows),
          'exact': a.canon(tuple(a.term(term, False) for term in row) for row in rows),
          'value': a.canon(tuple(a.term(term, True) for term in row) for row in rows),
          'semantics': 'Source RDF triples are sets; every advisor/workplace path contributes a solution, including projected duplicates. Absent optional paths produce unbound projected values.'}
output.write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result), flush=True)
