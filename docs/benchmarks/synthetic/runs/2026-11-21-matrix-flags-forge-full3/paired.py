import sys,csv,statistics as st,collections
# Usage: python3 paired.py [record dir]. Pairs each arm with its neighbour in the palindrome
# and prints the per-epoch execution ratios against 6dbf2a08.
base=sys.argv[1] if len(sys.argv)>1 else "."
# per invocation, per (variant,query), list of epoch medians in order
def load(inv):
    d=collections.defaultdict(lambda: collections.defaultdict(list))
    for r in csv.DictReader(open(f"{base}/samples/{inv}.tsv"),delimiter='\t'):
        d[(r['variant'],r['query'])][(int(r['round']),int(r['epoch']))].append(float(r['exec_ms']))
    return {k:[st.median(v[e]) for e in sorted(v)] for k,v in d.items()}
inv={n:load(n) for n in ['inv-1-6dbf2a08','inv-2-b47b582b','inv-3-0779d666','inv-4-0779d666','inv-5-b47b582b','inv-6-6dbf2a08']}
pairs={'b47b':[('inv-2-b47b582b','inv-1-6dbf2a08'),('inv-5-b47b582b','inv-6-6dbf2a08')],
       '0779':[('inv-3-0779d666','inv-1-6dbf2a08'),('inv-4-0779d666','inv-6-6dbf2a08')]}
for k in sorted(inv['inv-1-6dbf2a08']):
    out=[]
    for arm,pp in pairs.items():
        rs=[]
        for c,b in pp:
            rs+= [x/y for x,y in zip(inv[c][k],inv[b][k])]
        tag='ALL>1' if all(r>1 for r in rs) else 'ALL<1' if all(r<1 for r in rs) else ''
        out.append(f"{arm} med {st.median(rs):.3f} min {min(rs):.3f} max {max(rs):.3f} {tag}")
    b=st.median(inv['inv-1-6dbf2a08'][k]+inv['inv-6-6dbf2a08'][k])
    print(f"{k[0][:1]} {k[1]:22s} base {b:8.4f} | "+' | '.join(out))
