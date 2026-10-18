#!/usr/bin/env python3
"""The OWL2Bench SPARQL queries as query files, for scripts/bench-owl2bench.sh.

    queries.py <queries.txt> <out dir> <profile>

<queries.txt> is the Zenodo record of the 22 queries (CC-BY-4.0), which gives each query on
one line that starts with SELECT, followed by its description and a "Profile:" line that
lists the OWL 2 profiles under which the query has a defined answer. The queries are numbered
q1 to q22 in the order of the file. The script writes <out dir>/<name>.rq for every query
whose profile list contains <profile> (EL, QL, RL or DL), with the prefixes the texts use and
leave undeclared, and prints the names. <out dir>/profiles.tsv lists every query with its
profiles and whether it was written.
"""

import os
import re
import sys

PREFIXES = (
    "PREFIX : <http://benchmark/OWL2Bench#>\n"
    "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\n"
)


def parse(text):
    """[(query text, [profiles])] in file order"""
    out, cur = [], None
    for line in text.splitlines():
        s = line.strip()
        if s.upper().startswith("SELECT"):
            if cur is not None:
                out.append(cur)
            cur = [re.sub(r"\s+", " ", s), []]
        elif cur is not None and s.startswith("Profile:"):
            cur[1] = [p.strip() for p in s[len("Profile:"):].split(",") if p.strip()]
    if cur is not None:
        out.append(cur)
    return out


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    src, out, profile = sys.argv[1], sys.argv[2], sys.argv[3].upper()
    with open(src, encoding="utf-8") as f:
        qs = parse(f.read())
    if len(qs) != 22 or any(not p for _, p in qs):
        sys.exit(f"{src}: expected 22 queries, each with a Profile line, found {len(qs)}")
    os.makedirs(out, exist_ok=True)
    rows, names = [], []
    for i, (q, profiles) in enumerate(qs, 1):
        name = f"q{i}"
        keep = profile in profiles
        rows.append(f"{name}\t{','.join(profiles)}\t{'yes' if keep else 'no'}")
        if keep:
            with open(os.path.join(out, name + ".rq"), "w", encoding="utf-8") as f:
                f.write(PREFIXES + q + "\n")
            names.append(name)
    with open(os.path.join(out, "profiles.tsv"), "w", encoding="utf-8") as f:
        f.write("query\tprofiles\twritten\n" + "\n".join(rows) + "\n")
    print(" ".join(names))


if __name__ == "__main__":
    main()
