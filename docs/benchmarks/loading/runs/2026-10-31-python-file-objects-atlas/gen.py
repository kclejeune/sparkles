import sys, random
n = int(sys.argv[1]); out = open(sys.argv[2], "w")
random.seed(438)
preds = [f"<http://ex.org/p{i}>" for i in range(40)]
for i in range(n):
    s = f"<http://ex.org/s{i // 10}>"
    p = preds[i % 40]
    r = i % 4
    if r == 0:
        o = f"<http://ex.org/s{random.randrange(n // 10)}>"
    elif r == 1:
        o = f'"{random.randrange(10**6)}"^^<http://www.w3.org/2001/XMLSchema#integer>'
    elif r == 2:
        o = f'"label {i} with some words"@en'
    else:
        o = f'"text value number {random.randrange(10**7)}"'
    out.write(f"{s} {p} {o} .\n")
