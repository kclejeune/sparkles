"""Generate synthetic Ask questions from the DBpedia database, with gold SPARQL that is
executed before a question is kept.

usage: synth.py --bin SPARKLES --loc DBPEDIA_DB [--exclude SET.json ...] OUT_DIR [PER]

PER is the number of entities drawn for each relation of each class, 4 for the
committed sets. Every --exclude set's gold queries name resources that never become a
question's subject.

Questions come from six templates over well-known entities of eleven classes, phrased
by hand per relation in synth_phrases.py. Every
gold query runs on the database and a question is kept only when its gold returns a
bounded, non-empty answer. Each question's split (train, validation or test) is chosen
by the SHA-256 of "ask-synth-v2:" and its id, so the test split is fixed before any run.
No question is taken from QALD, db25 or their answers, and no entity that a gold query
of those sets names is used as a question's subject.
"""
import argparse, hashlib, json, os, pathlib, re, subprocess, sys

BIN = None
LOC = None
EXCLUDE = []
DBO = 'http://dbpedia.org/ontology/'
SEED = 'ask-synth-v2'
sys.path.insert(0, str(pathlib.Path(__file__).parent))
from phrases import CLASSES as PH, HOPS, SUPERLATIVES
CLASSES = os.environ['SYNTH_CLASSES'].split(',') if os.environ.get('SYNTH_CLASSES') else list(PH)
SKIP = ('wikiPage', 'abstract', 'thumbnail', 'Id', 'id', 'Work/', 'depiction', 'caption',
        'sameAs', 'type', 'seeAlso', 'filename', 'Url', 'url', 'Number', 'number', 'code', 'Code')


def query(text, timeout=60):
    r = subprocess.run(['timeout', str(timeout), str(BIN), 'query', '--loc', LOC, '--results', 'json', text],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return None
    d = json.loads(r.stdout)
    return d['results']['bindings'] if 'results' in d else d


rows = query


def h(s):
    return hashlib.sha256(s.encode()).hexdigest()


def label(iri):
    """The English label without a parenthesized qualifier, as a person would write it."""
    b = rows(f'SELECT ?l WHERE {{ <{iri}> <http://www.w3.org/2000/01/rdf-schema#label> ?l FILTER(LANG(?l) = "en") }} LIMIT 1')
    if not b:
        return None
    return re.sub(r'\s*\([^)]*\)', '', b[0]['l']['value']).strip() or None


def test_resources():
    """Every DBpedia resource that a gold query of the evaluation sets names."""
    out = set()
    for f in EXCLUDE:
        for q in json.loads(f.read_text()).get('questions', []):
            g = q.get('gold') or ''
            out.update(re.findall(r'<(http://dbpedia\.org/resource/[^>]+)>', g))
            for pfx in ('res', 'dbr'):
                out.update('http://dbpedia.org/resource/' + m for m in re.findall(pfx + r':([A-Za-z0-9_%().,\'-]+[A-Za-z0-9_%)])', g))
    return out


def entities(cls, p, k, banned):
    """Up to k well-known instances of the class that have p, chosen by seed from the
    3,000 with the longest pages."""
    b = rows(f'SELECT DISTINCT ?s WHERE {{ ?s a <{DBO}{cls}> ; <{p}> ?o ; <{DBO}wikiPageLength> ?len }} '
             f'ORDER BY DESC(?len) LIMIT 3000', 120) or []
    # A subject whose page title needs a parenthesized qualifier shares its name with
    # another page, so a question that names it without the qualifier is ambiguous.
    ss = sorted((r['s']['value'] for r in b if r['s']['value'] not in banned and '_(' not in r['s']['value']),
                key=lambda s: h(f'{SEED}:{cls}:{p}:{s}'))
    return ss[:k]


def main():
    global BIN, LOC, EXCLUDE
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin', required=True)
    ap.add_argument('--loc', required=True)
    ap.add_argument('--exclude', action='append', default=[], type=pathlib.Path)
    ap.add_argument('out', type=pathlib.Path)
    ap.add_argument('per', type=int, nargs='?', default=4)
    a = ap.parse_intermixed_args()
    BIN, LOC, EXCLUDE, per = a.bin, a.loc, a.exclude, a.per
    out = a.out; out.mkdir(parents=True, exist_ok=True)
    banned = test_resources()
    print('test resources excluded as subjects:', len(banned), file=sys.stderr)
    qs = []
    labels = {}
    def lab(i):
        if i not in labels:
            labels[i] = label(i)
        return labels[i]
    def add(t, q, g, cls, p):
        qs.append({'template': t, 'question': q, 'gold': g, 'class': cls, 'relation': p})
    for cls in CLASSES:
        one, many, rels = PH[cls]
        for pn, (attr, rel, ask, noun) in rels.items():
            p = DBO + pn
            for e in entities(cls, p, per, banned):
                el = lab(e)
                if not el:
                    continue
                vals = rows(f'SELECT DISTINCT ?o WHERE {{ <{e}> <{p}> ?o }} LIMIT 20') or []
                vals = [v['o']['value'] for v in vals if v['o']['type'] == 'uri']
                if not 1 <= len(vals) <= 5:
                    continue
                add('attribute', attr.format(e=el), f'SELECT DISTINCT ?x WHERE {{ <{e}> <{p}> ?x }}', cls, pn)
                o = sorted(vals, key=lambda v: h(f'{SEED}:{e}:{v}'))[0]
                ol = lab(o)
                if not ol:
                    continue
                n = rows(f'SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{DBO}{cls}> ; <{p}> <{o}> }}')
                cnt = int(n[0]['n']['value']) if n else 0
                # A list or count is kept only when the relation is how DBpedia links
                # instances to this value, so that another fair reading of the question
                # (through any other property) finds at most half again as many.
                m = rows(f'SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{DBO}{cls}> ; ?any <{o}> }}')
                anyc = int(m[0]['n']['value']) if m else 0
                if 2 <= cnt <= 30 and anyc <= cnt * 1.5:
                    add('list', f'Which {many} {rel.format(o=ol)}?', f'SELECT DISTINCT ?s WHERE {{ ?s a <{DBO}{cls}> ; <{p}> <{o}> }}', cls, pn)
                    add('count', f'How many {many} {rel.format(o=ol)}?', f'SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{DBO}{cls}> ; <{p}> <{o}> }}', cls, pn)
                if int(h(f'{SEED}:ask:{e}:{p}'), 16) % 2:
                    add('boolean', ask.format(e=el, o=ol), f'ASK {{ <{e}> <{p}> <{o}> }}', cls, pn)
                else:
                    other = rows(f'SELECT DISTINCT ?o WHERE {{ ?x a <{DBO}{cls}> ; <{p}> ?o . FILTER(?o != <{o}>) '
                                 f'FILTER NOT EXISTS {{ <{e}> <{p}> ?o }} }} LIMIT 50') or []
                    other = sorted((r['o']['value'] for r in other if r['o']['type'] == 'uri'), key=lambda v: h(f'{SEED}:{e}:no:{v}'))
                    if other and lab(other[0]):
                        add('boolean', ask.format(e=el, o=lab(other[0])), f'ASK {{ <{e}> <{p}> <{other[0]}> }}', cls, pn)
                if noun and len(vals) == 1:
                    have = rows(f'SELECT DISTINCT ?p2 WHERE {{ <{o}> ?p2 ?y . FILTER(isIRI(?y)) FILTER(STRSTARTS(STR(?p2), "{DBO}")) }}') or []
                    hops = sorted((r['p2']['value'][len(DBO):] for r in have if r['p2']['value'][len(DBO):] in HOPS), key=lambda x: h(f'{SEED}:hop:{e}:{x}'))
                    for p2 in hops[:1]:
                        y = rows(f'SELECT DISTINCT ?y WHERE {{ <{e}> <{p}> ?x . ?x <{DBO}{p2}> ?y }} LIMIT 10') or []
                        if 1 <= len(y) <= 5:
                            add('two-hop', HOPS[p2].format(x=noun.format(e=el)),
                                f'SELECT DISTINCT ?y WHERE {{ <{e}> <{p}> ?x . ?x <{DBO}{p2}> ?y }}', cls, f'{pn}/{p2}')
        for gp, num, text, order in SUPERLATIVES.get(cls, []):
            p, q = DBO + gp, DBO + num
            groups = rows(f'SELECT ?o (COUNT(DISTINCT ?s) AS ?n) WHERE {{ ?s a <{DBO}{cls}> ; <{p}> ?o ; <{q}> ?v }} '
                          f'GROUP BY ?o HAVING(COUNT(DISTINCT ?s) >= 5) ORDER BY DESC(?n) LIMIT 50', 120) or []
            for g in sorted((r['o']['value'] for r in groups), key=lambda v: h(f'{SEED}:sup:{q}:{v}'))[:per]:
                gl = lab(g)
                top = rows(f'SELECT ?s ?v WHERE {{ ?s a <{DBO}{cls}> ; <{p}> <{g}> ; <{q}> ?v }} ORDER BY {order}(?v) LIMIT 2')
                if gl and top and len(top) == 2 and top[0]['v']['value'] != top[1]['v']['value']:
                    add('superlative', text.format(g=gl),
                        f'SELECT ?s WHERE {{ ?s a <{DBO}{cls}> ; <{p}> <{g}> ; <{q}> ?v }} ORDER BY {order}(?v) LIMIT 1', cls, f'{gp}/{num}')
        print(cls, len(qs), file=sys.stderr, flush=True)
    seen, final = set(), []
    for q in qs:
        if q['question'] in seen:
            continue
        seen.add(q['question'])
        q['id'] = f"synth-{h(SEED + ':' + q['question'])[:12]}"
        q['kind'] = 'answerable'; q['category'] = q.pop('template')
        x = int(h(f"{SEED}:split:{q['id']}"), 16) % 10
        q['split'] = 'train' if x < 4 else 'validation' if x < 7 else 'test'
        final.append(q)
    for split in ('train', 'validation', 'test'):
        part = [dict((k, v) for k, v in q.items() if k != 'split') for q in final if q['split'] == split]
        doc = {'description': f'Synthetic Ask questions from DBpedia 2022.12.01, {split} split, generated by testsuite/ask/synth/synth.py with seed {SEED}. Gold queries were executed when generated.',
               'dataset': 'dbpedia', 'source': 'synthetic', 'language': 'en', 'seed': SEED, 'questions': part}
        (out / f'synth-{split}.json').write_text(json.dumps(doc, indent=1) + '\n')
        print(split, len(part), file=sys.stderr)


if __name__ == '__main__':
    main()
