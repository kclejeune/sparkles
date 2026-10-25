import hashlib, importlib.util, json, pathlib, sys
source, raw, oracle_script, output = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('oracle', oracle_script)
o = importlib.util.module_from_spec(spec)
spec.loader.exec_module(o)
tables, names, years = o.source_tables(source)
classes = {c for c, bases in tables[o.SUBCLASS].items() if o.DOCUMENT in bases}
documents = {d for d, types in tables[o.RDF_TYPE].items() if types & classes}
earliest = {}
for doc in documents:
    for year in years.get(doc, ()):
        for author in tables[o.CREATOR].get(doc, ()):
            v = int(year[1])
            if author not in earliest or v < earliest[author][0]:
                earliest[author] = (v, doc)
examples = []
with raw.open() as f:
    header = [x.lstrip('?') for x in next(f).rstrip('\n').split('\t')]
    for row_number, line in enumerate(f, 1):
        fields = dict(zip(header, line.rstrip('\n').split('\t')))
        year = int(o.TERMS.tsv_term(fields['yr'])['value'])
        name = o.TERMS.tsv_term(fields['name'])['value']
        doc = fields['document']
        authors = [a for a in tables[o.CREATOR].get(doc, ()) if name in names.get(a, ())]
        if authors and all(earliest[a][0] < year for a in authors):
            examples.append({'row': row_number, 'document': doc, 'name': name, 'actual_year': year,
                             'raw_tsv': line.rstrip('\n'),
                             'authors': [{'author': a, 'earliest_year': earliest[a][0],
                                          'earlier_document': earliest[a][1],
                                          'earlier_issued_source_terms': sorted(tables[o.ISSUED][earliest[a][1]])}
                                         for a in authors]})
            if len(examples) == 5:
                break
h = hashlib.sha256()
rows = -1
with raw.open('rb') as f:
    for line in f:
        h.update(line)
        rows += 1
result = {'witness_version': 1, 'method': 'Actual complete QLever q6 rows checked against independently reconstructed author earliest years from the exact NT source.',
          'full_raw_tsv_rows': rows, 'full_raw_tsv_bytes': raw.stat().st_size, 'full_raw_tsv_sha256': h.hexdigest(),
          'source_path': str(source), 'raw_response_path': str(raw), 'oracle_script_sha256': hashlib.sha256(oracle_script.read_bytes()).hexdigest(),
          'datatype_contract': 'The source oracle explicitly checks all issued terms as xsd:integer and names as xsd:string; documents are IRIs.',
          'examples': examples}
output.write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result), flush=True)
