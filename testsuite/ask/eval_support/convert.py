"""Shared steps of the converters that turn public question sets into Ask sets.

Each converter pins its source files by commit and SHA256, runs every gold query on
the data that the run will use when it is given that data, drops the questions whose
gold returns nothing, fails or cannot be scored, and compares the recomputed answers
with the answers the source publishes, so the drift between the source's knowledge
graph and the loaded one is counted rather than hidden.
"""

import hashlib
import pathlib
import re
import time
import urllib.request

from evaluator import (
    QUERY_GRACE_SECONDS,
    Invalid,
    OfflinePolicy,
    Unsupported,
    answer_set,
    capture_child,
    offline_query,
    parse_child,
    strict_json,
)


def fetch_pinned(url, sha256, cap, target):
    """Download one file, refuse it above `cap` bytes or with another SHA256."""
    with urllib.request.urlopen(url, timeout=60) as reply:
        data = reply.read(cap + 1)
    if len(data) > cap:
        raise Invalid(f"{url} is too large")
    if hashlib.sha256(data).hexdigest() != sha256:
        raise Invalid(f"{url} does not match its pinned SHA256")
    pathlib.Path(target).write_bytes(data)


def category(query):
    """A coarse label from the shape of the gold query."""
    text = re.sub(r"\s+", " ", query).upper()
    body = re.sub(r"PREFIX [^ ]+ <[^>]*> ?", "", text)
    if body.lstrip().startswith("ASK"):
        return "ask"
    if re.search(r"\b(COUNT|SUM|AVG|MIN|MAX)\s*\(|GROUP BY|HAVING", body):
        return "aggregate"
    if re.search(r"NOT EXISTS|MINUS|FILTER\s*\(\s*!", body):
        return "negation"
    if "ORDER BY" in body:
        return "ordering"
    if "UNION" in body:
        return "union"
    if "FILTER" in body:
        return "filter"
    return "join" if body.count(" . ") + body.count(" ; ") >= 1 else "lookup"


# The prefixes that DBpedia's Virtuoso endpoint declares itself, which QALD and
# Text2SPARQL gold queries use without declaring them, and `res:`, which older QALD
# releases use for resources.
DBPEDIA_PREFIXES = {
    "dbo": "http://dbpedia.org/ontology/",
    "dbr": "http://dbpedia.org/resource/",
    "res": "http://dbpedia.org/resource/",
    "dbp": "http://dbpedia.org/property/",
    "dbc": "http://dbpedia.org/resource/Category:",
    "yago": "http://dbpedia.org/class/yago/",
    "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "rdfs": "http://www.w3.org/2000/01/rdf-schema#",
    "xsd": "http://www.w3.org/2001/XMLSchema#",
    "owl": "http://www.w3.org/2002/07/owl#",
    "foaf": "http://xmlns.com/foaf/0.1/",
    "skos": "http://www.w3.org/2004/02/skos/core#",
    "dc": "http://purl.org/dc/elements/1.1/",
    "dct": "http://purl.org/dc/terms/",
    "geo": "http://www.w3.org/2003/01/geo/wgs84_pos#",
    "georss": "http://www.georss.org/georss/",
    "prov": "http://www.w3.org/ns/prov#",
}
# A projection of one aggregate or function call with no parentheses around it, which
# Virtuoso accepts: `SELECT DISTINCT COUNT(?x)`, `SELECT Count(?x) as ?c`,
# `SELECT COUNT(DISTINCT ?y AS ?y)`, `SELECT SUM(?n)` and `SELECT DISTINCT xsd:date(?d)`.
BARE_PROJECTION = re.compile(
    r"\bSELECT(\s+DISTINCT)?\s+"
    r"(COUNT|SUM|AVG|MIN|MAX|SAMPLE|[A-Za-z][\w-]*:[A-Za-z][\w-]*)\s*\(\s*(DISTINCT\s+)?(\?\w+)"
    r"(?:\s+AS\s+\?\w+)?\s*\)(?:\s+AS\s+(\?\w+))?(?=\s+(?:WHERE\b|\{))",
    re.IGNORECASE,
)


def normalize_dbpedia(query):
    """Make a gold query written for DBpedia's Virtuoso endpoint standard SPARQL.

    Two dialect habits are rewritten: prefixes the endpoint predeclares are declared,
    and a bare aggregate or function projection gets its parentheses and a variable.
    Returns the query and the list of the rewrites, empty when it was standard.
    """
    changes = []
    match = BARE_PROJECTION.search(query)
    if match:
        distinct, func, inner, var, alias = match.groups()
        name = alias or ("?count" if func.upper() == "COUNT" else "?value")
        replacement = f"SELECT{distinct or ''} ({func}({inner or ''}{var}) AS {name})"
        query = query[: match.start()] + replacement + query[match.end() :]
        changes.append("projection")
    body = re.sub(r'<[^>\s]*>|"(?:[^"\\]|\\.)*"|' + r"'(?:[^'\\]|\\.)*'", " ", query)
    declared = {m.lower() for m in re.findall(r"PREFIX\s+([\w-]*):", body, re.I)}
    used = set(re.findall(r"(?<![\w?$:/#-])([A-Za-z][\w-]*):(?=[\w(%])", body))
    missing = sorted(p for p in used - declared if p in DBPEDIA_PREFIXES)
    if missing:
        query = (
            "".join(f"PREFIX {p}: <{DBPEDIA_PREFIXES[p]}>\n" for p in missing) + query
        )
        changes.append("prefixes:" + ",".join(missing))
    return query, changes


def gold_result(sparkles, source, query, directory, timeout):
    """Run one gold query on `source`, the `--loc` or `--data` arguments.

    Returns (answer set, None) or (None, reason), where the reason is `unsupported`,
    `failed` or `empty`.
    """
    try:
        offline_query(query)
    except OfflinePolicy:
        return None, "unsupported"
    record = capture_child(
        [
            sparkles,
            "query",
            *source,
            "--results",
            "json",
            "--timeout",
            str(timeout),
            "--outbound-block-private",
            "--outbound-allow",
            "no-remote-eval.invalid",
            query,
        ],
        directory,
        time.monotonic() + timeout + QUERY_GRACE_SECONDS,
    )
    try:
        result = answer_set(parse_child(record, directory))
    except Unsupported:
        return None, "unsupported"
    except Invalid:
        return None, "failed"
    if type(result) is not bool and not result.rows:
        return None, "empty"
    return result, None


def values(result):
    """The lexical values of every cell, the way Text2SPARQL publishes answers."""
    if type(result) is bool:
        return {"true"} if result else set()
    return {cell[1] for row in result.rows for cell in row if len(cell) > 1}


def compare(published, recomputed):
    """Compare a published answer with a recomputed one by their lexical values.

    Each is a boolean, an answer set from `answer_set` or a set of lexical values,
    and `published` may be None. Columns and datatypes are not compared, because
    the sources publish bare values. The record's `status` is `equal`, `differs` or
    `unpublished`.
    """
    if published is None:
        return {"status": "unpublished"}
    if type(published) is bool or type(recomputed) is bool:
        same = type(published) is bool and published is recomputed
        return {"status": "equal" if same else "differs", "kind": "ask"}
    want = published if isinstance(published, set) else values(published)
    got = values(recomputed)
    return {
        "status": "equal" if want == got else "differs",
        "published": len(want),
        "recomputed": len(got),
        "common": len(want & got),
    }


def database_record(manifest):
    """The load manifest of the database the gold ran on, kept whole in the set with
    its SHA256, so a set names the exact files, binary and options of its snapshot."""
    if manifest is None:
        return None
    data = pathlib.Path(manifest).read_bytes()
    if len(data) > 256 * 1024:
        raise Invalid("the manifest is too large")
    return {
        "manifestSha256": hashlib.sha256(data).hexdigest(),
        "manifest": strict_json(data),
    }


def summarize(comparisons):
    """Counts of each comparison status, and the IDs that differ."""
    counts = {}
    for c in comparisons.values():
        counts[c["status"]] = counts.get(c["status"], 0) + 1
    return {
        "counts": counts,
        "differs": sorted(
            k for k, c in comparisons.items() if c["status"] == "differs"
        ),
    }
