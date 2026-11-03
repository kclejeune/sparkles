"""Execute every gold query of a synthetic set directory and record the answer sizes in
check.json beside the sets.

usage: check.py --bin SPARKLES --loc DBPEDIA_DB DIR"""
import argparse, collections, json, pathlib, sys
sys.path.insert(0, str(pathlib.Path(__file__).parent))
import synth


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin', required=True)
    ap.add_argument('--loc', required=True)
    ap.add_argument('dir', type=pathlib.Path)
    a = ap.parse_args()
    synth.BIN, synth.LOC = a.bin, a.loc
    res, bad, bools = {}, [], collections.Counter()
    for s in ['train', 'validation', 'test']:
        for q in json.loads((a.dir / f'synth-{s}.json').read_text())['questions']:
            r = synth.query(q['gold'], 120)
            if r is None:
                bad.append((q['id'], 'error'))
                continue
            if isinstance(r, dict) and 'boolean' in r:
                bools[r['boolean']] += 1
                n = 1
            else:
                n = int(r[0]['n']['value']) if q['category'] == 'count' and r else len(r)
            res[q['id']] = n
            if n == 0:
                bad.append((q['id'], 'empty'))
    (a.dir / 'check.json').write_text(json.dumps(
        {'answers': res, 'bad': bad, 'booleans': {str(k): v for k, v in bools.items()}}, indent=1) + '\n')
    print('checked', len(res), 'bad', bad[:10], 'booleans', dict(bools))
    sys.exit(1 if bad else 0)


if __name__ == '__main__':
    main()
