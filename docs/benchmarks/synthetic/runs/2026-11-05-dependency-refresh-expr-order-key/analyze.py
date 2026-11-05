#!/usr/bin/env python3
"""Validate the bounded stock-harness control and summarize paired processes."""
import collections
import csv
import json
import math
from pathlib import Path
import random
import statistics

ROOT=Path(__file__).resolve().parent
D=ROOT/'evidence/results'
assert (D/'done').read_text().strip()=='ok'
with (D/'samples.tsv').open() as f:
    rows=list(csv.DictReader(f,delimiter='\t'))
expected={(e,'ABBA'[(e-1)%4],rep) for e in range(1,33) for rep in range(1,41)}
got=[]
processes=collections.defaultdict(lambda:collections.defaultdict(list))
for row in rows:
    assert row['query']=='expr-order-key'
    e,rep=int(row['epoch']),int(row['rep'])
    assert int(row['round'])==(e-1)//4+1
    got.append((e,row['variant'],rep))
    for k in ['exec_ms','wall_ms']:
        value=float(row[k])
        assert math.isfinite(value) and value>=0
        processes[e,row['variant']][k].append(value)
assert len(got)==len(set(got)) and set(got)==expected
answers=list(csv.reader((D/'answers.tsv').open(),delimiter='\t'))
assert {r[0] for r in answers}=={'A','B'} and len(answers)==2
assert answers[0][1:]==answers[1][1:] and answers[0][2]=='10'
assert len(answers[0])==5 and answers[0][4].startswith('rows:')
warm=list(csv.reader((D/'warm-answers.tsv').open(),delimiter='\t'))
assert {(int(r[0]),r[1],int(r[3])) for r in warm}==expected and len(warm)==len(expected)
assert all(r[2]=='expr-order-key' for r in warm)
assert len({r[4] for r in warm})==1 and warm[0][4].startswith('10 rows, schema ')
assert ', distinct 10,' in warm[0][4] and ', rows:' in warm[0][4]
summary={'comparison':'origin 6d55f1e6 -> batch 6724abe9','query':'expr-order-key','threads':1,'rounds':8,'requests_per_process':40,'warmups':5,'timed_requests':len(rows),'native_answer_checks':len(warm),'answers':answers,'native_fingerprint':warm[0][4],'metrics':{}}
medians={key:{m:statistics.median(values) for m,values in metric.items()} for key,metric in processes.items()}
for metric in ['exec_ms','wall_ms']:
    a=[v[metric] for (e,label),v in medians.items() if label=='A']
    b=[v[metric] for (e,label),v in medians.items() if label=='B']
    av,bv=statistics.median(a),statistics.median(b)
    rng=random.Random(534498)
    ratios=[]
    for _ in range(10000):
        ar,br=[],[]
        for block in rng.choices(range(8),k=8):
            ar.extend(medians[block*4+slot,'A'][metric] for slot in [1,4])
            br.extend(medians[block*4+slot,'B'][metric] for slot in [2,3])
        ratios.append(statistics.median(br)/statistics.median(ar))
    ratios.sort()
    summary['metrics'][metric]={'origin_ms':av,'batch_ms':bv,'ratio':bv/av,'added_ms':bv-av,'origin_process_range':[min(a),max(a)],'batch_process_range':[min(b),max(b)],'paired_abba_bootstrap_95_ratio_interval':[ratios[249],ratios[9749]],'process_medians':{f'{e}-{v}':record[metric] for (e,v),record in sorted(medians.items())}}
summary['bootstrap']='10000 resamples of complete paired ABBA rounds; seed534498; median of16 process medians per variant; nearest-rank2.5/97.5percentiles'
(ROOT/'summary.json').write_text(json.dumps(summary,indent=2,sort_keys=True)+'\n')
print(json.dumps(summary,indent=2,sort_keys=True))
