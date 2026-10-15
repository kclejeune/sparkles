"""Finite retained-CLI checks with a counted loopback Anthropic double only.

The fake upstream deliberately supplies model text. These observations establish
routing/accounting controls, never real accuracy, spend, latency or provider behavior.
"""

import hashlib
import http.client
import importlib.machinery
import importlib.util
import json
import pathlib
import sqlite3
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from anthropic_gate import (
    MODELS,
    RESPONSE_CAP,
    Gate,
    GateError,
    Wallet,
    effective_config,
    serve_gate,
)
from evaluator import (
    Invalid,
    OfflinePolicy,
    capture_child,
    digest,
    offline_query,
    strict_json,
)

ROOT = pathlib.Path(__file__).resolve().parents[3]
RETAINED_SHA = "8453532536b45d74c730906f441dd662389a5a088bea432d4a0f05d20db3074d"


def load_evaluator():
    l = importlib.machinery.SourceFileLoader(
        "finite_eval_cli", str(ROOT / "scripts/eval-ask")
    )
    spec = importlib.util.spec_from_loader(l.name, l)
    m = importlib.util.module_from_spec(spec)
    l.exec_module(m)
    return m


def need(value, message):
    if not value:
        raise Invalid(message)


class FakeAnthropic:
    def __init__(self, qset):
        self.questions = {q["question"]: q for q in qset["questions"]}
        self.mode, self.calls, self.structured_failures = "oracle", [], 0
        self.http_requests = []
        self.wallet_path = None
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
                owner.calls.append({"method": "GET", "path": self.path})
                self.send_error(400)

            def do_POST(self):
                size = int(self.headers.get("content-length", "0"))
                if size > 128 * 1024:
                    self.send_error(413)
                    return
                raw = self.rfile.read(size)
                body = strict_json(raw)
                db = sqlite3.connect(owner.wallet_path, timeout=1)
                try:
                    prior = db.execute(
                        "SELECT id,case_id,model,payload_sha,bound FROM attempts WHERE state='RESERVED_UNRESOLVED' AND payload_sha=?",
                        (hashlib.sha256(raw).hexdigest(),),
                    ).fetchall()
                finally:
                    db.close()
                need(
                    len(prior) == 1 and prior[0][2] == body["model"],
                    "upstream received a request without a prior committed matching reservation",
                )
                owner.calls.append(
                    {
                        "model": body["model"],
                        "sha256": hashlib.sha256(raw).hexdigest(),
                        "schema": "output_config" in body,
                        "priorCommittedReservation": prior[0],
                        "controls": {
                            k: body[k]
                            for k in (
                                "max_tokens",
                                "stream",
                                "service_tier",
                                "inference_geo",
                            )
                        },
                    }
                )
                if owner.mode in ("429", "schema-reject"):
                    out = b'{"error":{"message":"FAKE_ECHO_NOT_A_CREDENTIAL"}}'
                    status = 429 if owner.mode == "429" else 400
                else:
                    status = 200
                    user = body["messages"][0]["content"]
                    question = user.split("\n", 1)[0].removeprefix("Question: ").strip()
                    q = owner.questions.get(question)
                    if "rows of a query result" in body["system"]:
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
                                (
                                    x
                                    for text, x in owner.questions.items()
                                    if text in user
                                ),
                                None,
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
                        if (
                            owner.mode == "structured"
                            and owner.structured_failures == 0
                        ):
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
                            "usage": {
                                "input_tokens": 100,
                                "output_tokens": 40,
                                "service_tier": "standard",
                                "inference_geo": "global",
                            },
                        }
                    ).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(out)))
                self.end_headers()
                self.wfile.write(out)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever)
        self.thread.start()

    def dispatch(self, payload, key, timeout):
        c = http.client.HTTPConnection(
            "127.0.0.1", self.server.server_port, timeout=min(timeout, 2)
        )
        try:
            # Explicit fake sentinel only; no real credential can be configured.
            c.request(
                "POST",
                "/v1/messages",
                payload,
                {
                    "content-type": "application/json",
                    "x-api-key": "FAKE_ECHO_NOT_A_CREDENTIAL",
                },
            )
            r = c.getresponse()
            if not 200 <= r.status < 300:
                raise GateError("upstream_status")
            b = r.read(RESPONSE_CAP + 1)
            if len(b) > RESPONSE_CAP:
                raise GateError("upstream_cap")
            return b
        finally:
            c.close()

    def close(self):
        self.server.shutdown()
        self.thread.join(2)
        self.server.server_close()


def run_fresh(output, binary):
    need(digest(binary) == RETAINED_SHA, "exact retained CLI hash mismatch")
    output = pathlib.Path(output)
    output.mkdir(mode=0o700)
    module = load_evaluator()
    qset = strict_json((ROOT / "testsuite/ask/demo.json").read_bytes())
    # One answer, clarification and refusal exercise the whole-pipeline semantics.
    selected = [
        next(q for q in qset["questions"] if q["id"] == "lookup-02"),
        next(q for q in qset["questions"] if q["kind"] == "clarify"),
        next(q for q in qset["questions"] if q["kind"] == "unanswerable"),
    ]
    checks = []
    offline = FakeAnthropic(qset)
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
    for mode in (
        "oracle",
        "structured",
        "cascade",
        "429",
        "schema-reject",
        "transport-retry",
    ):
        d = output / mode
        d.mkdir(mode=0o700)
        upstream = FakeAnthropic(qset)
        wallet = Wallet(d / "wallet", d / "grant", {"run": "synthetic-" + mode})
        upstream.wallet_path = wallet.path / "wallet.sqlite3"
        gate = Gate(wallet, "FAKE_ECHO_NOT_A_CREDENTIAL", upstream.dispatch)
        server = serve_gate(gate)
        if mode == "transport-retry":
            base_handler = server.RequestHandlerClass

            class RetryStatusFixture(base_handler):
                """Test-only status adapter triggers the actual Rust retry loop.

                It masks the first two terminal refusals as 429, while the real
                gate/SQLite path still refuses all later upstream dispatch.
                """

                rewritten = 0

                def send_response(self, code, message=None):
                    if code in (400, 402) and type(self).rewritten < 2:
                        type(self).rewritten += 1
                        code = 429
                    super().send_response(code, message)

            server.RequestHandlerClass = RetryStatusFixture
        gate.server_port = server.server_port
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            cfg = effective_config(gate)
            if mode == "schema-reject":
                cfg["models"]["providers"]["evaluation"]["structuredOutput"] = "auto"
            path = d / "models.json"
            path.write_text(json.dumps(cfg))
            upstream.mode = "429" if mode == "transport-retry" else mode
            pairs = (
                [v for m in MODELS for v in ("--pair", "evaluation/" + m)]
                if mode == "oracle"
                else ["--pair", "evaluation/" + MODELS[0]]
            )
            only = (
                ",".join(q["id"] for q in selected) if mode == "oracle" else "lookup-02"
            )
            args = module.parse_args(
                [
                    "--sparkles",
                    str(binary),
                    "--sparkles-sha256",
                    RETAINED_SHA,
                    "--model-config",
                    str(path),
                    "--set",
                    str(ROOT / "testsuite/ask/demo.json"),
                    "--only",
                    only,
                    "--out",
                    str(d / "report"),
                    "--jobs",
                    "1",
                    "--deadline",
                    "30",
                    "--phase-deadline",
                    "120",
                    "--allow-loopback",
                    "--model-secret",
                    "evalgate=env:SPARKLES_EVAL_GATE_TOKEN",
                ]
                + pairs
            )
            if mode == "cascade":
                args.pair = ["evaluation/" + m for m in MODELS[:2]]
            _, report = module.run_matrix(args, gate)
            money = wallet.snapshot_report()
            need(
                all(r["cliEstimatedCostUSD"] is None for r in report["rows"]),
                "unpriced SDK zero must remain unknown, not free",
            )
            if mode in ("429", "schema-reject", "transport-retry"):
                need(
                    not report["complete"]
                    and len(upstream.calls) == 1
                    and money["unresolvedUpperNanodollars"] > 0,
                    "ambiguous billing did not stop real client",
                )
                if mode == "schema-reject":
                    need(
                        gate.received >= 2,
                        "actual auto downgrade did not return through the gate",
                    )
                if mode == "transport-retry":
                    need(
                        gate.received == 3 and RetryStatusFixture.rewritten == 2,
                        "actual Rust three-attempt retry was not exercised",
                    )
            else:
                need(report["complete"], "retained CLI did not complete " + mode)
                if mode in ("oracle", "structured"):
                    need(
                        all(c["correct"] for c in report["cases"]),
                        "fixture oracle scoring failed",
                    )
                if mode == "structured":
                    need(
                        upstream.structured_failures == 1
                        and len(upstream.calls) >= 3
                        and money["acceptedRequests"] == len(upstream.calls),
                        "real structured retry was not separately reserved",
                    )
                if mode == "cascade":
                    c = [c for c in report["cases"] if " > " in c["pair"]]
                    need(
                        c and c[0]["correct"],
                        "real cascade did not recover with later model",
                    )
                    need(
                        any(row["model"] == MODELS[1] for row in upstream.calls),
                        "cascade did not dispatch second pair",
                    )
            checks.append(
                {
                    "case": mode,
                    "complete": report["complete"],
                    "upstreamCalls": len(upstream.calls),
                    "validInboundRequests": gate.received,
                    "transportStatusAdapter": mode == "transport-retry",
                    "wallet": money,
                    "wireRequests": upstream.calls,
                    "httpRequests": upstream.http_requests,
                }
            )
            with (d / "check.json").open("x") as f:
                json.dump(checks[-1], f, indent=2)
        finally:
            gate.set_case(None)
            server.shutdown()
            thread.join(2)
            server.server_close()
            wallet.close()
            upstream.close()
    with (output / "result.json").open("x") as f:
        json.dump(
            {
                "schema": 2,
                "binary": str(binary),
                "binarySha256": RETAINED_SHA,
                "realExternalCalls": 0,
                "cases": checks,
            },
            f,
            indent=2,
        )
    return checks


def self_test(args):
    if args.sparkles_sha256 is None:
        args.sparkles_sha256 = RETAINED_SHA
    need(
        args.sparkles_sha256 == RETAINED_SHA,
        "self-test requires the admitted retained binary",
    )
    out = args.out or str(
        pathlib.Path(tempfile.gettempdir())
        / ("sparkles-ask-gate-selftest-" + __import__("secrets").token_hex(12))
    )
    run_fresh(out, args.sparkles)
    print("eval-ask: retained CLI fake-provider checks passed")
    return 0
