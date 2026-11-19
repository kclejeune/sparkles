# usage: symtab.py PERF DIR_A DIR_B QUERY : cycles per request by symbol (sample period 200003), A vs B
import subprocess, sys, collections
perf, da, db, q = sys.argv[1:5]
def load(d):
    n = dict(l.split() for l in open(f"{d}/counts.txt"))[q]
    out = subprocess.run([perf, "script", "-i", f"{d}/{q}.data", "-F", "event,ip,sym"], capture_output=True, text=True).stdout
    c = collections.Counter()
    for line in out.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3: c[parts[0].split('/')[0] + ' ' + parts[2]] += 1
    return {k: v * 200003 / int(n) for k, v in c.items()}
A, B = load(da), load(db)
ta, tb = sum(A.values()), sum(B.values())
print(f"total cycles/request A {ta/1e3:.1f}k B {tb/1e3:.1f}k ratio {tb/ta:.3f}")
keys = sorted(set(A) | set(B), key=lambda k: -abs(B.get(k, 0) - A.get(k, 0)))
for k in keys[:int(sys.argv[5]) if len(sys.argv) > 5 else 25]:
    print(f"{A.get(k,0)/1e3:9.1f}k {B.get(k,0)/1e3:9.1f}k {(B.get(k,0)-A.get(k,0))/1e3:+9.1f}k  {k[:120]}")
