#!/usr/bin/env python3
"""Runs compare.sh on the premise ontology of every OWL 2 RL case of the W3C OWL 2 test
cases (the approved export of https://www.w3.org/2009/11/owl-test/).

    scripts/rl-reference/w3c.py ALL_NT WORKDIR

ALL_NT is the suite's all.rdf converted to N-Triples (run.sh prepares it). A case takes
part when it lists the RL profile and has an RDF/XML premise ontology and no imports.
WORKDIR/<identifier>/ gets the premise and the comparison of that case, and
WORKDIR/summary.json lists every case with its outcome and the counts of compare.sh. The
exit status is 1 when a comparison has an unexplained difference or fails.

Env: COMPARE (default compare.sh beside this file), and the variables of compare.sh.
"""

import json
import os
import re
import subprocess
import sys
from collections import defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rl_terms  # noqa: E402

TEST = "http://www.w3.org/2007/OWL/testOntology#"


def fix_entities(data):
    """The suite writes some entity declarations with single quotes, which the RDF/XML
    reader does not take."""
    return "\n".join(
        line.replace("'", '"') if line.lstrip().startswith("<!ENTITY") else line for line in data.split("\n")
    )


def main(all_nt, work):
    by_s = defaultdict(lambda: defaultdict(list))
    for s, p, o in rl_terms.triples(all_nt):
        by_s[s][p].append(o)
    cases = []
    for s, props in by_s.items():
        if f"<{TEST}RL>" not in props.get(f"<{TEST}profile>", []):
            continue
        premise = props.get(f"<{TEST}rdfXmlPremiseOntology>", [])
        ident = props.get(f"<{TEST}identifier>", [])
        if len(premise) != 1 or len(ident) != 1:
            continue
        name = rl_terms.literal(ident[0])[0]
        imports = props.get(f"<{TEST}importedOntology>", [])
        cases.append((name, rl_terms.literal(premise[0])[0], bool(imports)))
    cases.sort()
    compare = os.environ.get("COMPARE") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "compare.sh")
    os.makedirs(work, exist_ok=True)
    summary, failed = [], 0
    for name, premise, imports in cases:
        safe = re.sub(r"[^A-Za-z0-9_.-]", "_", name)
        d = os.path.join(work, safe)
        if imports:
            summary.append({"case": name, "outcome": "skipped", "reason": "the premise imports another ontology"})
            continue
        os.makedirs(d, exist_ok=True)
        path = os.path.join(d, "premise.rdf")
        with open(path, "w", encoding="utf-8") as f:
            f.write(fix_entities(premise))
        r = subprocess.run([compare, path, d], capture_output=True, text=True)
        entry = {"case": name, "exit": r.returncode}
        if r.returncode == 2:
            entry["outcome"] = "error"
            entry["error"] = r.stderr.strip().splitlines()[-1:] or [""]
        else:
            entry["outcome"] = "equal" if r.returncode == 0 else "unexplained"
            for key in ("diff", "core"):
                with open(os.path.join(d, f"{key}.json"), encoding="utf-8") as f:
                    rep = json.load(f)
                entry[key] = {
                    "sparkles": rep["sparkles"],
                    "reference": rep["reference"],
                    "sparkles_only": rep["differences"]["sparkles"]["total"],
                    "reference_only": rep["differences"]["reference"]["total"],
                    "explained": {
                        side: rep["differences"][side]["by_explanation"] for side in ("sparkles", "reference")
                    },
                }
        failed += entry["outcome"] in ("error", "unexplained")
        summary.append(entry)
        print(f"{entry['outcome']:12} {name}", flush=True)
    counts = defaultdict(int)
    for e in summary:
        counts[e["outcome"]] += 1
    with open(os.path.join(work, "summary.json"), "w", encoding="utf-8") as f:
        json.dump({"counts": counts, "cases": summary}, f, indent=1)
    print(dict(counts))
    return 1 if failed else 0


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2]))
