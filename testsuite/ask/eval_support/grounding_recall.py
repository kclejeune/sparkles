"""Offline grounding recall of Ask prompts against gold queries.

usage: grounding_recall.py SET_JSON RUN_DIR [RUN_DIR...] [--out OUT_JSON]

RUN_DIR is a run's output directory, which holds `replies.jsonl` and the set's
`report.json` below it. For each answerable question it takes the last first-draft
prompt Ask sent, after any clarification, and checks whether every schema term and
every resource that the gold query names occurs in that prompt, by full IRI or by a
prefixed name under the prompt's own prefix lines, or as a local name listed under its
namespace. The draft's own query and the
repair prompts never count. No model is called. A dry run with the fake provider
gives the same prompts as a paid run, so the metric can be computed for free.

A resource is an IRI whose path holds `/resource/`, as DBpedia and Wikidata name
entities, and every other IRI is a schema term. `rdf:type`, `rdfs:label`,
`foaf:name` and XSD datatypes are left out, since every prompt can use them.
"""

import collections
import json
import pathlib
import re
import sys

from evaluator import gold_terms, prefix_header

IGNORE = {
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://xmlns.com/foaf/0.1/name",
}
XSD = "http://www.w3.org/2001/XMLSchema#"
DRAFT_SYSTEM = "You translate"


def is_resource(iri):
    return "/resource/" in iri or iri.startswith("http://www.wikidata.org/entity/Q")


def prompt_text(request):
    parts = []
    for m in request.get("messages") or []:
        c = m.get("content")
        if isinstance(c, str):
            parts.append(c)
        else:
            parts.extend(b.get("text", "") for b in c or [] if isinstance(b, dict))
    return "".join(parts)


def system_text(request):
    s = request.get("system")
    if isinstance(s, list):
        return "".join(b.get("text", "") for b in s if isinstance(b, dict))
    return s or ""


def drafts(replies_path):
    """case -> list of (prompt text, input tokens) of first-draft requests in order."""
    out = collections.defaultdict(list)
    for line in open(replies_path):
        r = json.loads(line)
        if not r.get("case") or r.get("status") != 200:
            continue
        req = r["request"]
        if not system_text(req).startswith(DRAFT_SYSTEM):
            continue
        text = prompt_text(req)
        if "```sparql" in text.split("Question:", 1)[-1]:
            continue  # a repair quotes the failed query
        u = (r.get("reply") or {}).get("usage") or {}
        tokens = sum(
            u.get(k) or 0
            for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")
        )
        out[r["case"]].append((system_text(req) + "\n" + text, tokens))
    return out


def grouped_terms(text):
    """IRIs listed under a namespace, as in `<http://x/> a 3, b 1; p: c 2`."""
    out = set()
    prefixes = prompt_prefixes(text)
    for head, body in re.findall(r"(<[^<>\s]+[/#]>|[A-Za-z][\w-]*:) ((?:[\w-]+ \d+(?:, )?)+)", text):
        ns = head[1:-1] if head.startswith("<") else prefixes.get(head[:-1])
        if ns:
            out |= {ns + local for local in re.findall(r"([\w-]+) \d+", body)}
    return out


def present(term, text, prefixes, grouped=frozenset()):
    if term in grouped:
        return True
    if f"<{term}>" in text or re.search(r"(?<![\w:/])" + re.escape(term) + r"(?![\w%-])", text):
        return True
    for p, ns in prefixes.items():
        if term.startswith(ns) and len(term) > len(ns):
            local = term[len(ns) :]
            pat = r"(?<![\w-])" + re.escape(f"{p}:{local}") + r"(?![\w%-])"
            if re.search(pat, text):
                return True
    return False


def prompt_prefixes(text):
    return dict(re.findall(r"^([A-Za-z][\w-]*): <([^<>\s]+)>$", text, re.M))


def recall(qset, run_dir):
    run_dir = pathlib.Path(run_dir)
    # The one report whose questions all belong to this set. A run of several sets
    # or several pairs is refused rather than mixed.
    reports = [json.load(open(p)) for p in run_dir.glob("*/report.json")]
    ids = {q["id"] for q in qset["questions"]}
    matching = [r for r in reports if {c["id"] for c in r["cases"]} <= ids]
    if len(matching) != 1:
        raise ValueError(f"{run_dir}: {len(matching)} reports match the set; give a run of one set")
    report = matching[0]
    if len(report.get("pairs") or []) != 1:
        raise ValueError(f"{run_dir}: the run has several pairs; recall is per pair")
    by_case = drafts(run_dir / "replies.jsonl")
    header = prefix_header(qset.get("prefixes", {}))
    qs = {q["id"]: q for q in qset["questions"]}
    rows = {}
    for c in report["cases"]:
        q = qs.get(c["id"])
        if q is None or q["kind"] != "answerable":
            continue
        iris, _ = gold_terms(header + q["gold"])
        iris = {t for t in iris if t not in IGNORE and not t.startswith(XSD)}
        schema = sorted(t for t in iris if not is_resource(t))
        resources = sorted(t for t in iris if is_resource(t))
        prompts = by_case.get(c["case"]) or []
        if not prompts:
            rows[c["id"]] = {"prompt": False, "schema": schema, "resources": resources}
            continue
        text, tokens = prompts[-1]
        pre = prompt_prefixes(text)
        grouped = grouped_terms(text)
        miss_s = [t for t in schema if not present(t, text, pre, grouped)]
        miss_r = [t for t in resources if not present(t, text, pre, grouped)]
        rows[c["id"]] = {
            "prompt": True,
            "promptChars": len(text),
            "promptTokens": tokens,
            "schema": schema,
            "resources": resources,
            "missingSchema": miss_s,
            "missingResources": miss_r,
            "schemaComplete": not miss_s,
            "resourcesComplete": not miss_r,
            "complete": not miss_s and not miss_r,
        }
    return rows


def summary(rows):
    have = [r for r in rows.values() if r["prompt"]]
    n = len(rows)
    terms_s = sum(len(r["schema"]) for r in have)
    terms_r = sum(len(r["resources"]) for r in have)
    chars = sorted(r["promptChars"] for r in have)
    toks = sorted(r["promptTokens"] for r in have)
    return {
        "questions": n,
        "withPrompt": len(have),
        "schemaComplete": sum(r["schemaComplete"] for r in have),
        "resourcesComplete": sum(r["resourcesComplete"] for r in have),
        "complete": sum(r["complete"] for r in have),
        "schemaTermRecall": [terms_s - sum(len(r["missingSchema"]) for r in have), terms_s],
        "resourceRecall": [terms_r - sum(len(r["missingResources"]) for r in have), terms_r],
        "promptCharsMean": sum(chars) / len(chars) if chars else None,
        "promptCharsMax": chars[-1] if chars else None,
        "promptTokensMean": sum(toks) / len(toks) if toks else None,
    }


def main(argv):
    out = None
    if "--out" in argv:
        i = argv.index("--out")
        out = argv[i + 1]
        argv = argv[:i] + argv[i + 2 :]
    qset = json.load(open(argv[0]))
    result = {}
    for d in argv[1:]:
        rows = recall(qset, d)
        result[str(d)] = {"summary": summary(rows), "questions": rows}
        print(d, json.dumps(result[str(d)]["summary"]))
    if out:
        pathlib.Path(out).write_text(json.dumps(result, indent=1) + "\n")


if __name__ == "__main__":
    main(sys.argv[1:])
