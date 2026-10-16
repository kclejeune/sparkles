"""Finite checks of the evaluator against a loopback Anthropic double.

The fake upstream deliberately supplies model text. These observations establish
routing, accounting and failure handling, never real accuracy, spend, latency or
provider behavior.
"""

import importlib.machinery
import importlib.util
import json
import pathlib
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import proxy as px
from evaluator import (
    Invalid,
    OfflinePolicy,
    capture_child,
    digest,
    offline_query,
    strict_json,
)

ROOT = pathlib.Path(__file__).resolve().parents[3]
MODELS = px.MODELS
FAKE_KEY = "FAKE_ECHO_NOT_A_CREDENTIAL"
FAKE_USAGE = {
    "input_tokens": 100,
    "output_tokens": 40,
    "service_tier": "standard",
    "inference_geo": "global",
}


def load_evaluator():
    loader = importlib.machinery.SourceFileLoader(
        "finite_eval_cli", str(ROOT / "scripts/eval-ask")
    )
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def need(value, message):
    if not value:
        raise Invalid(message)


class FakeAnthropic:
    """A counted loopback stand-in for the Messages API.

    `mode` picks the behavior. `framing` picks how a 200 reply is delimited.
    """

    def __init__(self, qsets, framing="content-length"):
        self.questions = {}
        for qset in qsets:
            self.questions.update({q["question"]: q for q in qset["questions"]})
        self.mode, self.calls, self.framing = "oracle", [], framing
        self.structured_failures = 0
        self.http_requests = []
        self.errors_sent, self.ok_models = 0, []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def parse_request(self):
                valid = super().parse_request()
                if valid:
                    owner.http_requests.append(
                        {"method": self.command, "path": self.path}
                    )
                return valid

            def do_GET(self):
                self.send_error(400)

            def send_json(self, status, out):
                self.send_response(status)
                self.send_header("content-type", "application/json")
                if status == 200 and owner.framing == "chunked":
                    self.send_header("transfer-encoding", "chunked")
                    self.end_headers()
                    half = len(out) // 2
                    for part in (out[:half], out[half:]):
                        self.wfile.write(b"%x\r\n" % len(part) + part + b"\r\n")
                    self.wfile.write(b"0\r\n\r\n")
                    return
                if status == 200 and owner.framing == "close":
                    self.send_header("connection", "close")
                    self.end_headers()
                else:
                    self.send_header("content-length", str(len(out)))
                    self.end_headers()
                self.wfile.write(out)

            def do_POST(self):
                size = int(self.headers.get("content-length", "0"))
                raw = self.rfile.read(size)
                need(self.headers.get("x-api-key") == FAKE_KEY, "upstream key")
                if self.path == "/v1/messages/count_tokens":
                    return self.send_json(200, b'{"input_tokens": 8}')
                body = strict_json(raw)
                output = body.get("output_config") or {}
                owner.calls.append(
                    {
                        "model": body["model"],
                        "schema": "format" in output,
                        "effort": output.get("effort"),
                        "bytes": len(raw),
                        "stream": body.get("stream"),
                    }
                )
                if owner.mode == "429":
                    return self.send_json(429, b'{"error":{"message":"FAKE_ECHO"}}')
                if owner.mode == "529-then-ok" and owner.errors_sent < 1:
                    owner.errors_sent += 1
                    return self.send_json(529, b'{"error":{"message":"overloaded"}}')
                if owner.mode == "schema-reject" and "format" in output:
                    return self.send_json(400, b'{"error":{"message":"no schema"}}')
                user = body["messages"][0]["content"]
                question = user.split("\n", 1)[0].removeprefix("Question: ").strip()
                q = owner.questions.get(question)
                if "rows of a query result" in body.get("system", ""):
                    content = {
                        "text": "The result answers the question [1].",
                        "citations": [1],
                    }
                else:
                    content = {
                        "query": "",
                        "explanation": "A fixture answer.",
                        "assumptions": [],
                        "clarify": {"question": "", "choices": []},
                        "graph": {"subject": "", "predicate": "", "object": ""},
                    }
                    if q is None:
                        # Repair messages include earlier context; locate the
                        # original exact question, rather than fabricate gold.
                        q = next(
                            (x for t, x in owner.questions.items() if t in user), None
                        )
                    if q is None:
                        content["explanation"] = (
                            "The fake could not identify the question."
                        )
                    elif q["kind"] == "clarify":
                        content["query"] = "SELECT ?s WHERE { ?s a ex:Team }"
                        content["clarify"] = {
                            "question": "Which reading?",
                            "choices": ["the first", "the second"],
                        }
                    elif q["kind"] == "answerable":
                        content["query"] = q["gold"]
                    if owner.mode == "cascade" and body["model"] == MODELS[0]:
                        content["query"] = "SELECT ?s WHERE { ?s a ex:Teem }"
                    if owner.mode == "structured" and owner.structured_failures == 0:
                        content = {}
                        owner.structured_failures += 1
                out = json.dumps(
                    {
                        "id": "fake-message",
                        "type": "message",
                        "role": "assistant",
                        "model": body["model"],
                        "content": [{"type": "text", "text": json.dumps(content)}],
                        "stop_reason": "end_turn",
                        "usage": FAKE_USAGE,
                    }
                ).encode()
                owner.ok_models.append(body["model"])
                self.send_json(200, out)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever)
        self.thread.start()

    def upstream(self):
        return px.HTTPUpstream(
            FAKE_KEY, "127.0.0.1", self.server.server_port, tls=False
        )

    def close(self):
        self.server.shutdown()
        self.thread.join(2)
        self.server.server_close()


def run_fresh(output, binary, sha):
    output = pathlib.Path(output)
    output.mkdir(mode=0o700)
    need(digest(binary) == sha, "binary hash mismatch")
    module = load_evaluator()
    qset = strict_json((ROOT / "testsuite/ask/demo.json").read_bytes())
    # One answer, clarification and refusal exercise the whole-pipeline semantics.
    selected = [
        next(q for q in qset["questions"] if q["id"] == "lookup-02"),
        next(q for q in qset["questions"] if q["kind"] == "clarify"),
        next(q for q in qset["questions"] if q["kind"] == "unanswerable"),
    ]
    checks = []
    offline = FakeAnthropic([qset])
    try:
        remote_query = (
            "SELECT ?s WHERE {SERVICE <http://127.0.0.1:"
            + str(offline.server.server_port)
            + "/query> {?s ?p ?o}}"
        )
        try:
            offline_query(remote_query)
        except OfflinePolicy:
            pass
        else:
            raise Invalid("offline SERVICE guard failed")
        # Real retained Query path, with the actually supported explicit allowlist.
        # It must refuse before contacting this independent loopback witness.
        argv = [
            str(binary),
            "query",
            "--data",
            str(ROOT / "testsuite/ask/org.ttl"),
            "--results",
            "json",
            "--timeout",
            "5",
            "--outbound-block-private",
            "--outbound-allow",
            "no-remote-eval.invalid",
            remote_query,
        ]
        record = capture_child(
            argv, output / "offline-query-policy", time.monotonic() + 5
        )
        need(
            record["returncode"] != 0
            and record["cleanup"]
            and not record["timeout"]
            and not record["byteCap"],
            "retained CLI offline policy refusal missing",
        )
        need(offline.http_requests == [], "offline query contacted loopback witness")
        (output / "offline-policy.json").write_text(
            json.dumps(
                {
                    "actual": record,
                    "observedRequests": len(offline.http_requests),
                    "guardRefusedBeforeSpawn": True,
                    "scope": "existing CLI allowlist plus conservative SERVICE guard, not a deny-all CLI flag",
                },
                indent=2,
            )
        )
    finally:
        offline.close()
    for mode, framing in (
        ("oracle", "chunked"),
        ("structured", "content-length"),
        ("cascade", "close"),
        ("429", "content-length"),
        ("schema-reject", "content-length"),
        ("529-then-ok", "content-length"),
    ):
        d = output / mode
        d.mkdir(mode=0o700)
        upstream = FakeAnthropic([qset], framing)
        upstream.mode = mode
        ledger = px.Ledger(d / "ledger.jsonl", 10**12)
        proxy = px.Proxy(ledger, upstream.upstream(), FAKE_KEY)
        server = proxy.serve()
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            cfg = proxy.config()
            if mode == "schema-reject":
                cfg["models"]["providers"]["evaluation"]["structuredOutput"] = "auto"
            path = d / "models.json"
            path.write_text(json.dumps(cfg))
            oracle = mode == "oracle"
            pairs = (
                list(MODELS)
                if oracle
                else list(MODELS[:2])
                if mode == "cascade"
                else [MODELS[0]]
            )
            labels = ["evaluation/" + m for m in pairs]
            only = ",".join(q["id"] for q in selected) if oracle else "lookup-02"
            args = module.parse_args(
                [
                    "--sparkles",
                    str(binary),
                    "--sparkles-sha256",
                    sha,
                    "--model-config",
                    str(path),
                    "--set",
                    str(ROOT / "testsuite/ask/demo.json"),
                    "--only",
                    only,
                    "--out",
                    str(d / "report"),
                    "--deadline",
                    "60",
                    "--allow-loopback",
                    "--model-secret",
                    "evalgate=env:SPARKLES_EVAL_GATE_TOKEN",
                ]
                + [v for m in pairs for v in ("--pair", "evaluation/" + m)]
            )
            _, report = module.run_matrix(args, proxy)
            money = ledger.report()
            rows = {r["pair"]: r for r in report["rows"]}
            need(report["complete"], "evaluator did not finish " + mode)
            need(
                all(r["cliEstimatedCostUSD"] is None for r in report["rows"]),
                "unpriced SDK zero must remain unknown, not free",
            )
            need(
                all(
                    c["effort"] == "medium" and c["stream"] is False
                    for c in upstream.calls
                ),
                "the proxy did not set effort and nonstreaming",
            )
            need(
                money["settledNanodollars"]
                == sum(px.cost(m, FAKE_USAGE) for m in upstream.ok_models)
                and money["unresolvedNanodollars"] == 0,
                "ledger does not match the counted replies",
            )
            if mode in ("oracle", "structured", "529-then-ok"):
                need(
                    all(c["correct"] for c in report["cases"]),
                    "fixture oracle scoring failed",
                )
            if oracle:
                need(" > ".join(labels) in rows, "cascade row missing")
                need(rows[" > ".join(labels)]["answeredBy"][labels[0]] == 3, "cascade")
            if mode == "structured":
                need(
                    upstream.structured_failures == 1 and len(upstream.calls) >= 3,
                    "real structured retry was not separately reserved",
                )
            if mode == "cascade":
                cascade = rows[" > ".join(labels)]
                need(
                    rows["evaluation/" + MODELS[0]]["endToEndSuccess"]["numerator"] == 0
                    and cascade["endToEndSuccess"]["numerator"] == 1
                    and cascade["answeredBy"]["evaluation/" + MODELS[1]] == 1,
                    "cascade did not recover with the later model",
                )
                need(
                    any(c["model"] == MODELS[1] for c in upstream.calls),
                    "cascade did not dispatch the second pair",
                )
            if mode == "429":
                need(
                    report["cases"][0]["detail"]["status"] == "provider_error"
                    and len(upstream.calls) >= 1
                    and money["settledNanodollars"] == 0,
                    "a rate limit was not scored as a free provider error",
                )
            if mode == "529-then-ok":
                need(
                    upstream.errors_sent == 1 and len(upstream.calls) >= 2,
                    "the client did not retry through the proxy",
                )
            if mode == "schema-reject":
                need(
                    upstream.calls[0]["schema"]
                    and any(not c["schema"] for c in upstream.calls),
                    "auto structured output did not downgrade through the proxy",
                )
            checks.append(
                {
                    "case": mode,
                    "framing": framing,
                    "complete": report["complete"],
                    "upstreamCalls": len(upstream.calls),
                    "ledger": money,
                    "wireRequests": upstream.calls,
                    "httpRequests": upstream.http_requests,
                }
            )
            with (d / "check.json").open("x") as f:
                json.dump(checks[-1], f, indent=2)
        finally:
            server.shutdown()
            thread.join(2)
            server.server_close()
            ledger.close()
            upstream.close()
    with (output / "result.json").open("x") as f:
        json.dump(
            {
                "schema": 3,
                "binary": str(binary),
                "binarySha256": sha,
                "realExternalCalls": 0,
                "cases": checks,
            },
            f,
            indent=2,
        )
    return checks


def self_test(args):
    sha = args.sparkles_sha256 or digest(args.sparkles)
    out = args.out or str(
        pathlib.Path(tempfile.gettempdir())
        / ("sparkles-ask-selftest-" + __import__("secrets").token_hex(12))
    )
    run_fresh(out, args.sparkles, sha)
    print("eval-ask: fake-provider checks passed")
    return 0
