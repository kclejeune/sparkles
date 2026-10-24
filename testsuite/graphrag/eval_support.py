"""Pieces of scripts/eval-graphrag that need no provider: the demo data, the fake API,
the text retrieval baseline and the statistics of the pairwise judgements."""

import hashlib
import json
import math
import random
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

FAKE_KEY = "FAKE_ECHO_NOT_A_CREDENTIAL"

# The criteria of Edge et al., "From Local to Global" (2024), §3.4, in their wording.
CRITERIA = {
    "comprehensiveness": "How much detail does the answer provide to cover all aspects and details of the question?",
    "diversity": "How varied and rich is the answer in providing different perspectives and insights on the question?",
    "empowerment": "How well does the answer help the reader understand and make informed judgements about the topic?",
    "directness": "How specifically and clearly does the answer address the question?",
}


def demo_turtle(org_ttl, sample):
    """The demo organisation graph with the gold facts of the labelled local sample, as
    if its documents had been ingested without error. A new entity `_:key` of the
    sample becomes `res:ingested-key` with its label and types."""
    lines = [org_ttl.rstrip("\n"), ""]
    lines += [f"@prefix {k}: <{v}> ." for k, v in sample["prefixes"].items()]

    def term(value):
        if value.startswith("_:"):
            return "res:ingested-" + value[2:]
        return value

    seen = set()
    for doc in sample["documents"]:
        for entity in doc.get("entities", []):
            key = term(entity["key"])
            if key in seen:
                continue
            seen.add(key)
            lines.append(f"{key} rdfs:label {json.dumps(entity['label'])} .")
            for t in entity.get("types", []):
                lines.append(f"{key} a {t} .")
        for fact in doc["facts"]:
            lines.append(f"{term(fact['s'])} {fact['p']} {term(fact['o'])} .")
    return "\n".join(lines) + "\n"


def two_clusters(size=4, extra=0):
    """N-Triples of two dense clusters joined by one edge (C22 A1), with `extra` more
    facts about a member of the first cluster."""
    out = []
    label = "<http://www.w3.org/2000/01/rdf-schema#label>"
    knows = "<http://example.org/knows>"
    for c in ("x", "y"):
        for i in range(size):
            out.append(f'<http://example.org/{c}{i}> {label} "{c} person {i}" .')
            for j in range(i + 1, size):
                out.append(
                    f"<http://example.org/{c}{i}> {knows} <http://example.org/{c}{j}> ."
                )
    out.append(f"<http://example.org/x0> {knows} <http://example.org/y0> .")
    for n in range(extra):
        out.append(
            f'<http://example.org/x0> <http://example.org/note> "note {n}" .'
        )
    return "\n".join(out) + "\n"


def chunks(sample):
    """The ingested sources as retrieval chunks, one per paragraph of each document."""
    out = []
    for doc in sample["documents"]:
        for n, para in enumerate(p for p in re.split(r"\n\s*\n", doc["text"]) if p.strip()):
            out.append({"id": f"{doc['id']}#{n}", "title": doc["title"], "text": para.strip()})
    return out


def tokens(text):
    return re.findall(r"[a-z0-9]+", text.lower())


def bm25(chunk_list, query, k=8, k1=1.2, b=0.75):
    """The `k` best chunks for a query under Okapi BM25, ties broken by order."""
    docs = [tokens(c["title"] + " " + c["text"]) for c in chunk_list]
    avg = sum(map(len, docs)) / max(1, len(docs))
    df = {}
    for d in docs:
        for t in set(d):
            df[t] = df.get(t, 0) + 1
    n = len(docs)
    scores = []
    for i, d in enumerate(docs):
        s = 0.0
        for t in set(tokens(query)):
            if t not in df:
                continue
            idf = math.log(1 + (n - df[t] + 0.5) / (df[t] + 0.5))
            f = d.count(t)
            s += idf * f * (k1 + 1) / (f + k1 * (1 - b + b * len(d) / avg))
        scores.append((-s, i))
    return [chunk_list[i] for _, i in sorted(scores)[:k]]


def strip_markers(text):
    """An answer without its citation markers, such as [2] or [1, 3], so the judge
    compares what the answers say and not how each mode cites."""
    return re.sub(r"\s*\[\d+(?:\s*,\s*\d+)*\]", "", text or "").strip()


def judge_scores(verdicts):
    """Per question, the score of the first answer of a pair from the verdicts of both
    orders: a win counts 1, a tie 0.5 and a loss 0, averaged over the two orders.
    `verdicts` holds (question, first_is_a, winner) with winner in A, B or tie."""
    by_q = {}
    for q, first_is_a, winner in verdicts:
        if winner == "tie":
            s = 0.5
        else:
            s = 1.0 if (winner == "A") == first_is_a else 0.0
        by_q.setdefault(q, []).append(s)
    return {q: sum(v) / len(v) for q, v in by_q.items()}


def bootstrap(values, rounds=2000, seed=0):
    """The mean and its 95% percentile bootstrap interval over questions."""
    if not values:
        return None
    rng = random.Random(seed)
    n = len(values)
    means = sorted(sum(rng.choice(values) for _ in range(n)) / n for _ in range(rounds))
    return {
        "mean": sum(values) / n,
        "low": means[int(0.025 * rounds)],
        "high": means[int(0.975 * rounds) - 1],
        "n": n,
    }


def stable_sample(items, key, n):
    """`n` items in the order of the SHA256 of their key, so a sample never depends on
    the order of the source."""
    return sorted(items, key=lambda x: hashlib.sha256(key(x).encode()).hexdigest())[:n]


def fill(schema, name=None):
    """A small instance of a JSON schema, with plausible numbers for the members the
    GraphRAG and Ask prompts use, for the fake API of a dry run."""
    kind = schema.get("type")
    if "enum" in schema:
        # A dry run takes the global path of Ask whenever a draft may choose it.
        if name == "scope" and "global" in schema["enum"]:
            return "global"
        return schema["enum"][0]
    if kind == "object":
        props = schema.get("properties", {})
        return {k: fill(v, k) for k, v in props.items()}
    if kind == "array":
        items = schema.get("items", {"type": "string"})
        return [fill(items, name) for _ in range(max(1, schema.get("minItems", 1)))]
    if kind == "string":
        return {"reports": "R1", "findings": "F1.1", "query": ""}.get(
            name, "A fixture sentence [1]."
        )
    if kind == "integer":
        value = {"score": 50, "rating": 5}.get(name, 1)
        return max(value, schema.get("minimum", value))
    if kind == "number":
        return 1.0
    if kind == "boolean":
        return True
    return None


class SchemaFake:
    """A local stand-in for the Messages API that answers every request with an
    instance of the request's JSON schema. It reads no key and costs nothing."""

    def __init__(self):
        owner = self
        self.calls = []
        self.lock = threading.Lock()

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                raw = self.rfile.read(int(self.headers.get("content-length", "0")))
                if self.headers.get("x-api-key") != FAKE_KEY:
                    self.send_response(401)
                    self.end_headers()
                    return
                if self.path.endswith("/count_tokens"):
                    return self.reply({"input_tokens": max(1, len(raw) // 4)})
                body = json.loads(raw)
                schema = (
                    ((body.get("output_config") or {}).get("format") or {}).get("schema")
                    or {"type": "object", "properties": {"text": {"type": "string"}}}
                )
                with owner.lock:
                    owner.calls.append({"model": body["model"], "bytes": len(raw)})
                content = fill(schema)
                self.reply(
                    {
                        "id": "fake",
                        "type": "message",
                        "role": "assistant",
                        "model": body["model"],
                        "content": [{"type": "text", "text": json.dumps(content)}],
                        "stop_reason": "end_turn",
                        "usage": {
                            "input_tokens": max(1, len(raw) // 4),
                            "output_tokens": 40,
                        },
                    }
                )

            def reply(self, value):
                out = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(out)))
                self.end_headers()
                self.wfile.write(out)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.thread.join(2)
        self.server.server_close()
