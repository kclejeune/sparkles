"""Actual SQLite transactions and counted fake dispatch, never a real provider."""

import hashlib
import http.client
import json
import pathlib
import sqlite3
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "eval_support"))
from anthropic_gate import (
    AUTHORITY_HASHES,
    BOUNDS,
    CAP,
    MODELS,
    Gate,
    GateError,
    Wallet,
    dispatch_once,
    effective_config,
    redact,
    serve_gate,
    validate_request,
    validate_usage,
)
from evaluator import capture_child
from test_eval_transport import (
    header_prefix,
    length_wire,
    raw_upstream,
    record_observation,
    status_usage_body,
    status_wire,
)


def request(model=MODELS[0]):
    return {
        "model": model,
        "max_tokens": 1024,
        "system": "a system",
        "messages": [{"role": "user", "content": "a question"}],
    }


def response(model=MODELS[0], **usage):
    return {
        "model": model,
        "content": [{"type": "text", "text": "{}"}],
        "usage": dict(
            input_tokens=10,
            output_tokens=2,
            service_tier="standard",
            inference_geo="global",
            **usage,
        ),
    }


def invalid_request_cases():
    for key, value in [
        ("temperature", 0),
        ("top_p", 1),
        ("thinking", {"type": "enabled"}),
        ("tools", []),
        ("cache_control", {}),
        ("service_tier", "auto"),
        ("inference_geo", "us"),
        ("stream", True),
        ("max_tokens", 1025),
        ("model", "unknown"),
    ]:
        r = request()
        r[key] = value
        code = (
            "request_model_limit"
            if key in ("model", "max_tokens")
            else "request_control_conflict"
            if key in ("service_tier", "inference_geo", "stream")
            else "request_fields"
        )
        yield key, json.dumps(r).encode(), code, code, None
    valid = json.dumps(request()).encode()
    # The permissively decoded objects otherwise pass validation. Each fixture
    # isolates its own raw JSON/cap restriction instead of failing a later field.
    yield (
        "duplicate-key",
        valid[:-1] + b',"system":"duplicate"}',
        "invalid JSON",
        "gate_internal",
        "duplicate JSON field",
    )
    r = request()
    r["output_config"] = {
        "format": {"type": "json_schema", "schema": {"const": float("nan")}}
    }
    yield (
        "nonfinite-json",
        json.dumps(r).encode(),
        "invalid JSON",
        "gate_internal",
        "nonfinite JSON number",
    )
    yield (
        "oversize",
        valid + b" " * (128 * 1024 + 1 - len(valid)),
        "request_cap",
        "request_cap",
        None,
    )


class Budget(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="ask-wallet-")
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.w = Wallet(
            self.root / "wallet",
            self.root / "grant",
            {"run": "synthetic", "authority": AUTHORITY_HASHES},
        )
        self.addCleanup(self.w.close)

    def gate(self, dispatch=None):
        observed = []

        def counted(payload, key, timeout):
            # Independent connection sees only committed rows at actual send time.
            db = sqlite3.connect(self.w.path / "wallet.sqlite3")
            rows = db.execute(
                "SELECT state,bound FROM attempts WHERE state='RESERVED_UNRESOLVED'"
            ).fetchall()
            db.close()
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0][1], BOUNDS[json.loads(payload)["model"]])
            record_observation(
                {
                    "case": self.id(),
                    "priorCommittedReservation": rows,
                    "model": json.loads(payload)["model"],
                }
            )
            observed.append(json.loads(payload))
            return (
                dispatch
                or (
                    lambda p, k, t: json.dumps(
                        response(json.loads(p)["model"])
                    ).encode()
                )
            )(payload, key, timeout)

        g = Gate(self.w, "FAKE_SENTINEL_NOT_A_KEY", counted)
        g.set_case("case-one")
        return g, observed

    def test_bodyless_status_complete_framing_before_settlement(self):
        for status in (200, 204, 205):
            variants = [("valid-usage-tail", status_usage_body())]
            if status != 200:
                variants += [("empty", b""), ("malformed", b"{")]
            for label, body in variants:
                with self.subTest(status=status, variant=label):
                    directory = self.root / f"status-{status}-{label}"
                    directory.mkdir()
                    wallet = Wallet(
                        directory / "wallet",
                        directory / "grant",
                        {"run": f"synthetic-status-{status}-{label}"},
                    )
                    witnesses = []

                    def counted_dispatch(
                        payload, key, timeout, wallet=wallet, witnesses=witnesses
                    ):
                        independent = sqlite3.connect(wallet.path / "wallet.sqlite3")
                        try:
                            rows = independent.execute(
                                "SELECT id,case_id,model,payload_sha,bound,state FROM attempts WHERE state='RESERVED_UNRESOLVED'"
                            ).fetchall()
                        finally:
                            independent.close()
                        witnesses.append(
                            {
                                "priorCommittedReservation": rows,
                                "payloadHex": payload.hex(),
                                "independentConnectionClosed": True,
                            }
                        )
                        self.assertEqual(len(rows), 1)
                        self.assertEqual(
                            rows[0][2:],
                            (
                                MODELS[0],
                                hashlib.sha256(payload).hexdigest(),
                                BOUNDS[MODELS[0]],
                                "RESERVED_UNRESOLVED",
                            ),
                        )
                        return dispatch_once(payload, key, min(timeout, 2))

                    gate = Gate(wallet, "FAKE", counted_dispatch)
                    gate.set_case("bodyless-status")
                    wire = status_wire(status, body)
                    try:
                        with raw_upstream(wire) as seen:
                            errors = []
                            snapshots = []
                            for _ in range(2):
                                error = None
                                try:
                                    gate.process(json.dumps(request()).encode())
                                except GateError as failure:
                                    error = failure.code
                                errors.append(error)
                                snapshots.append(wallet.snapshot_report())
                            # Both requests and their evidence precede desired
                            # assertions, including on the unfixed source.
                            observation = {
                                "walletStatus": status,
                                "variant": label,
                                "wireHex": wire.hex(),
                                "errors": errors,
                                "first": snapshots[0],
                                "final": snapshots[1],
                                "upstreamSends": len(seen),
                                "priorWitnesses": witnesses,
                                "gateStopped": gate.stopped,
                            }
                            record_observation(observation)
                            first, final = snapshots
                            if status == 200:
                                self.assertEqual(errors, [None, None])
                                self.assertEqual(len(seen), 2)
                                self.assertEqual(first["acceptedRequests"], 1)
                                self.assertEqual(final["acceptedRequests"], 2)
                                self.assertEqual(first["knownSettledNanodollars"], 2000)
                                self.assertEqual(final["knownSettledNanodollars"], 4000)
                                self.assertEqual(final["unresolvedUpperNanodollars"], 0)
                                self.assertFalse(gate.stopped)
                                self.assertFalse(final["dispatchClosed"])
                            else:
                                self.assertEqual(
                                    first["unresolvedUpperNanodollars"],
                                    BOUNDS[MODELS[0]],
                                )
                                self.assertEqual(
                                    errors, ["upstream_framing", "dispatch_stopped"]
                                )
                                self.assertEqual(len(seen), 1)
                                self.assertTrue(gate.stopped)
                                for snapshot in snapshots:
                                    self.assertEqual(snapshot["acceptedRequests"], 1)
                                    self.assertEqual(
                                        snapshot["knownSettledNanodollars"], 0
                                    )
                                    self.assertEqual(
                                        snapshot["unresolvedUpperNanodollars"],
                                        BOUNDS[MODELS[0]],
                                    )
                                    self.assertTrue(snapshot["dispatchClosed"])
                                    self.assertEqual(len(snapshot["attempts"]), 1)
                                    self.assertEqual(
                                        snapshot["attempts"][0]["state"],
                                        "RESERVED_UNRESOLVED",
                                    )
                    finally:
                        wallet.close()
                        record_observation(
                            {
                                "statusWalletClosed": status,
                                "variant": label,
                                "databaseClosed": wallet.db is None,
                                "lockClosed": wallet.lock_fd is None,
                            }
                        )

    def test_complete_framing_before_settlement(self):
        body = json.dumps(response()).encode()
        for label, wire, malformed in [
            ("exact-length", length_wire(body), False),
            ("short-length-valid-json", length_wire(body, len(body) + 7), True),
            (
                "duplicate-length",
                length_wire(
                    body, extra=b"Content-Length: " + str(len(body)).encode() + b"\r\n"
                ),
                True,
            ),
            (
                "conflicting-framing",
                length_wire(body, extra=b"Transfer-Encoding: chunked\r\n"),
                True,
            ),
            (
                "valid-chunked-unsupported",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
                + f"{len(body):x}\r\n".encode()
                + body
                + b"\r\n0\r\n\r\n",
                True,
            ),
            ("close-delimited-unsupported", b"HTTP/1.1 200 OK\r\n\r\n" + body, True),
            (
                "header-cap",
                header_prefix(
                    65537,
                    headers=b"Content-Length: " + str(len(body)).encode() + b"\r\n",
                )
                + body,
                True,
            ),
            ("body-cap", length_wire(body, 1024 * 1024 + 1), True),
        ]:
            d = self.root / label
            d.mkdir()
            self.w = Wallet(d / "wallet", d / "grant", {"run": "synthetic-" + label})
            self.addCleanup(self.w.close)
            with raw_upstream(wire) as seen:
                g, witnesses = self.gate(dispatch_once)
                error = None
                try:
                    g.process(json.dumps(request()).encode())
                except GateError as exception:
                    error = exception.code
                first = self.w.snapshot_report()
                record_observation(
                    {
                        "case": label,
                        "error": error,
                        "wallet": first,
                        "priorReservationPayloads": witnesses,
                    }
                )
                if malformed:
                    self.assertEqual(
                        first["unresolvedUpperNanodollars"], BOUNDS[MODELS[0]]
                    )
                    self.assertIn(
                        error, ("upstream_framing", "upstream_headers", "upstream_cap")
                    )
                    self.assertEqual(first["knownSettledNanodollars"], 0)
                    self.assertTrue(g.stopped)
                    with self.assertRaisesRegex(GateError, "dispatch_stopped"):
                        g.process(json.dumps(request()).encode())
                else:
                    self.assertIsNone(error)
                    self.assertEqual(first["unresolvedUpperNanodollars"], 0)
                    self.assertEqual(first["knownSettledNanodollars"], 2000)
                self.assertEqual(len(seen), 1)

    def test_each_model_and_durable_prior_reservation(self):
        g, obs = self.gate()
        for model in MODELS:
            body, info = g.process(json.dumps(request(model)).encode())
            self.assertEqual(json.loads(body)["model"], model)
            self.assertEqual(obs[-1]["service_tier"], "standard_only")
            self.assertEqual(obs[-1]["inference_geo"], "global")
            self.assertIs(obs[-1]["stream"], False)
            self.assertNotEqual(info["preControlSha256"], info["wireSha256"])
        s = self.w.snapshot_report()
        self.assertEqual(s["acceptedRequests"], 3)
        self.assertEqual(s["unresolvedUpperNanodollars"], 0)
        self.assertLessEqual(s["highWaterNanodollars"], CAP)

    def seed_spend(self, amount):
        # Real admitted/settled attempts, no direct UPDATE to fabricate a wallet.
        while amount:
            ident = self.w.reserve_attempt(MODELS[2], "synthetic prior", "0" * 64)
            actual = min(BOUNDS[MODELS[2]], amount)
            self.w.settle_once(
                ident, actual, {"fixture": "explicit wallet transition test"}
            )
            amount -= actual

    def test_exact_fit_and_one_nanodollar_short(self):
        for offset in (0, 1):
            # Independent synthetic grants exercise the two admission boundaries;
            # no paid grant is reinitialized or reused.
            d = self.root / ("boundary-" + str(offset))
            d.mkdir()
            self.w = Wallet(
                d / "wallet", d / "grant", {"run": "synthetic-boundary-" + str(offset)}
            )
            self.addCleanup(self.w.close)
            self.seed_spend(CAP - BOUNDS[MODELS[0]] + offset)
            g, obs = self.gate()
            if offset:
                with self.assertRaisesRegex(GateError, "budget_exhausted"):
                    g.process(json.dumps(request()).encode())
            else:
                g.process(json.dumps(request()).encode())
            self.assertEqual(len(obs), 1 - offset)
            self.assertLessEqual(
                self.w.snapshot_report()["upperLiabilityNanodollars"], CAP
            )

    def test_usage_tiers_cache_and_thinking_once(self):
        for n, rate in [(100000, 100), (100001, 500)]:
            r = response()
            r["usage"]["input_tokens"] = n
            self.assertEqual(
                validate_usage(MODELS[0], r)[0],
                n * rate + 2 * (500 if n == 100000 else 2500),
            )
        r = response(
            cache_creation_input_tokens=30,
            cache_read_input_tokens=40,
            cache_creation={
                "ephemeral_5m_input_tokens": 10,
                "ephemeral_1h_input_tokens": 20,
            },
            output_tokens_details={"thinking_tokens": 1},
        )
        self.assertEqual(
            validate_usage(MODELS[0], r)[0],
            10 * 100 + 10 * 125 + 20 * 200 + 40 * 10 + 2 * 500,
        )
        for change in [
            ("input_tokens", True),
            ("input_tokens", -1),
            ("input_tokens", 1.5),
            ("input_tokens", 10**20),
            ("input_tokens", None),
            ("service_tier", "priority"),
            ("inference_geo", "us"),
            ("unknown_billable", 1),
            ("cache_creation_input_tokens", 5),
        ]:
            bad = response()
            bad["usage"][change[0]] = change[1]
            with self.subTest(change=change), self.assertRaises(GateError):
                validate_usage(MODELS[0], bad)
        for r in [
            dict(response(), model=MODELS[1]),
            dict(response(), usage={}),
            response(cache_creation_input_tokens=1),
            response(
                cache_creation={
                    "ephemeral_5m_input_tokens": 1,
                    "ephemeral_1h_input_tokens": 1,
                }
            ),
        ]:
            with self.assertRaises(GateError):
                validate_usage(MODELS[0], r)

    def assert_invalid_request_case(self, fixture):
        label, raw, direct_code, gate_code, cause = fixture
        with self.assertRaisesRegex(ValueError, "^" + direct_code + "$") as direct:
            validate_request(raw)
        if cause:
            self.assertEqual(str(direct.exception.__cause__), cause)
        d = self.root / (label + "-" + str(len(list(self.root.iterdir()))))
        d.mkdir()
        wallet = Wallet(d / "wallet", d / "grant", {"run": "synthetic-" + label})
        calls = []
        gate = Gate(
            wallet,
            "FAKE",
            lambda *args: calls.append(True) or json.dumps(response()).encode(),
        )
        gate.set_case("invalid-fixture")
        try:
            self.assertFalse(gate.stopped)
            with self.assertRaisesRegex(GateError, "^" + gate_code + "$"):
                gate.process(raw)
            self.assertEqual(calls, [])
            state = wallet.snapshot_report()
            self.assertEqual(state["acceptedRequests"], 0)
            record_observation(
                {
                    "case": label,
                    "directReason": direct_code,
                    "cause": cause,
                    "gateReason": gate_code,
                    "dispatches": calls,
                    "wallet": state,
                }
            )
        finally:
            wallet.close()

    def test_invalid_request_zero_dispatch(self):
        for fixture in invalid_request_cases():
            with self.subTest(case=fixture[0]):
                self.assert_invalid_request_case(fixture)

    def test_invalid_request_checks_detect_each_validation_bypass(self):
        for fixture in invalid_request_cases():
            label, target, *_ = fixture

            def bypass(raw, target=target):
                # An in-memory double simulates accepting just this invalid
                # fixture. Unrelated validation remains the production function.
                return validate_request(
                    json.dumps(request()).encode() if raw == target else raw
                )

            with (
                self.subTest(case=label),
                mock.patch("anthropic_gate.validate_request", side_effect=bypass),
            ):
                with self.assertRaisesRegex(AssertionError, "GateError not raised"):
                    self.assert_invalid_request_case(fixture)
                record_observation(
                    {"validationBypass": label, "correctedFixtureDetectedBypass": True}
                )

    def test_invalid_request_latches_separately(self):
        g, obs = self.gate()
        invalid = dict(request(), temperature=0)
        with self.assertRaisesRegex(GateError, "^request_fields$"):
            g.process(json.dumps(invalid).encode())
        with self.assertRaisesRegex(GateError, "^dispatch_stopped$"):
            g.process(json.dumps(request()).encode())
        self.assertEqual(obs, [])
        self.assertEqual(self.w.snapshot_report()["acceptedRequests"], 0)
        self.assertEqual(
            self.w.db.execute("SELECT count(*) FROM attempts").fetchone()[0], 0
        )

    def test_unresolved_no_retry_refund_or_restart(self):
        def failed(*args):
            raise GateError("upstream_status")

        g, obs = self.gate(failed)
        for _ in range(3):
            with self.assertRaises(GateError):
                g.process(json.dumps(request()).encode())
        self.assertEqual(len(obs), 1)
        s = self.w.snapshot_report()
        self.assertEqual(s["unresolvedUpperNanodollars"], BOUNDS[MODELS[0]])
        self.w.close()
        for wallet, claim in [("wallet", "grant"), ("replacement", "grant")]:
            with self.assertRaises(FileExistsError):
                Wallet(self.root / wallet, self.root / claim, {"run": "synthetic"})
        db = sqlite3.connect(self.root / "wallet/wallet.sqlite3")
        self.assertEqual(
            db.execute("SELECT bound,state FROM attempts").fetchone(),
            (BOUNDS[MODELS[0]], "RESERVED_UNRESOLVED"),
        )
        db.close()

    def test_storage_faults_keep_reservation(self):
        for phase in ("after_reserve", "after_response", "before_settlement"):
            d = self.root / phase
            d.mkdir()
            w = Wallet(d / "wallet", d / "grant", {"run": phase})
            calls = []

            def fault(p, phase=phase):
                if p == phase:
                    raise OSError("synthetic storage failure")

            w.fault = fault
            g = Gate(
                w,
                "FAKE",
                lambda *args, calls=calls: (
                    calls.append(True) or json.dumps(response()).encode()
                ),
            )
            g.set_case("case")
            with self.assertRaises(GateError):
                g.process(json.dumps(request()).encode())
            self.assertEqual(len(calls), int(phase != "after_reserve"))
            self.assertEqual(
                w.snapshot_report()["unresolvedUpperNanodollars"], BOUNDS[MODELS[0]]
            )
            w.close()
        # Failed reserve commit is independently observed as no dispatch/no row.
        self.w.fault = lambda p: (
            (_ for _ in ()).throw(OSError("commit refused"))
            if p == "before_commit"
            else None
        )
        g, obs = self.gate()
        with self.assertRaises(GateError):
            g.process(json.dumps(request()).encode())
        self.assertEqual(obs, [])
        self.assertEqual(
            self.w.db.execute("SELECT count(*) FROM attempts").fetchone()[0], 0
        )

    def test_duplicate_settlement_concurrent_callers(self):
        ident = self.w.reserve_attempt(MODELS[0], "case", "0" * 64)
        self.w.settle_once(ident, 1, {})
        with self.assertRaises(GateError):
            self.w.settle_once(ident, 0, {})
        g, obs = self.gate()
        errors = []

        def call():
            try:
                g.process(json.dumps(request()).encode())
            except Exception as e:  # noqa: BLE001 -- collect worker failures before asserting
                errors.append(type(e).__name__)

        threads = [threading.Thread(target=call) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(2)
            self.assertFalse(t.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(len(obs), 4)

    def test_redaction_short_unicode_control_and_exception(self):
        for key in ["x", "FAKE_SECRET", "\u96ea\n\t"]:
            obj = {"message": key, key: [key]}
            safe, changed = redact(obj, key)
            self.assertTrue(changed)
            self.assertNotIn(key, json.dumps(safe, ensure_ascii=False))

        def echoes(payload, key, timeout):
            r = response()
            r["content"][0]["text"] = key
            return json.dumps(r).encode()

        g, _obs = self.gate(echoes)
        body, info = g.process(json.dumps(request()).encode())
        self.assertNotIn(b"FAKE_SENTINEL_NOT_A_KEY", body)
        self.assertTrue(info["redacted"])
        g.dispatch = lambda *args: (_ for _ in ()).throw(
            OSError("FAKE_SENTINEL_NOT_A_KEY")
        )
        with self.assertRaises(GateError) as cm:
            g.process(json.dumps(request()).encode())
        self.assertNotIn("FAKE_SENTINEL_NOT_A_KEY", str(cm.exception))

    def test_http_auth_path_framing_and_effective_configuration(self):
        g, obs = self.gate()
        s = serve_gate(g)
        g.server_port = s.server_port
        t = threading.Thread(target=s.serve_forever)
        t.start()
        self.addCleanup(s.server_close)
        self.addCleanup(t.join, 2)
        self.addCleanup(s.shutdown)
        for route, token in [("/wrong", g.token), (g.route, "not-token")]:
            c = http.client.HTTPConnection("127.0.0.1", s.server_port, timeout=2)
            c.request(
                "POST",
                route,
                json.dumps(request()),
                {"content-type": "application/json", "x-api-key": token},
            )
            r = c.getresponse()
            self.assertEqual(r.status, 400)
            r.read()
            c.close()
        self.assertEqual(obs, [])
        c = http.client.HTTPConnection("127.0.0.1", s.server_port, timeout=2)
        c.request(
            "POST",
            g.route,
            json.dumps(request()),
            {"content-type": "application/json", "x-api-key": g.token},
        )
        r = c.getresponse()
        self.assertEqual(r.status, 200)
        r.read()
        c.close()
        self.assertEqual(len(obs), 1)
        cfg = effective_config(g)
        p = cfg["models"]["providers"]["evaluation"]
        self.assertEqual(p["structuredOutput"], "json-schema")
        self.assertEqual(p["maxOutputTokens"], 1024)
        self.assertNotIn("FAKE_SENTINEL_NOT_A_KEY", json.dumps(cfg))
        self.assertNotIn("temperature", p)

    def test_real_process_crash_after_durable_reserve(self):
        d = self.root / "crash"
        d.mkdir()
        source = self.root / "crash.py"
        source.write_text(
            "import sys,os\nsys.path.insert(0,"
            + repr(str(pathlib.Path(__file__).parent / "eval_support"))
            + ")\nfrom anthropic_gate import Wallet,MODELS\nw=Wallet("
            + repr(str(d / "wallet"))
            + ","
            + repr(str(d / "grant"))
            + ',{"run":"crash"})\nw.reserve_attempt(MODELS[0],"case","0"*64)\nos._exit(19)\n'
        )
        python = str(pathlib.Path(sys.base_prefix) / "bin/python3")
        r = capture_child(
            [python, "-I", "-S", "-B", str(source)],
            self.root / "crash-output",
            time.monotonic() + 5,
        )
        self.assertEqual(r["returncode"], 19)
        self.assertTrue(r["cleanup"])
        db = sqlite3.connect(d / "wallet/wallet.sqlite3")
        self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")
        self.assertEqual(
            db.execute("SELECT state,bound FROM attempts").fetchone(),
            ("RESERVED_UNRESOLVED", BOUNDS[MODELS[0]]),
        )
        db.close()
        with self.assertRaises(FileExistsError):
            Wallet(d / "otherwallet", d / "grant", {"run": "crash"})


class Boundaries(unittest.TestCase):
    def test_reservations_decimal_authority(self):
        from decimal import Decimal

        for m, rate, output in zip(MODELS, ("1", "4", "8"), ("2.5", "10", "20")):
            self.assertEqual(
                int(
                    (Decimal(1000000) * Decimal(rate) + Decimal(1024) * Decimal(output))
                    * Decimal(1000)
                ),
                BOUNDS[m],
            )

    def test_context_output_and_unknown_billing(self):
        for field, value in [
            ("output_tokens", 1025),
            ("input_tokens", 999999),
            ("cache_creation", {"ephemeral_5m_input_tokens": 0}),
            ("output_tokens_details", {"thinking_tokens": 3}),
            ("server_tool_use", {"web_search_requests": 1}),
        ]:
            r = response()
            r["usage"][field] = value
            with self.assertRaises(GateError):
                validate_usage(MODELS[0], r)
        for key, value in [
            ("stream", 0),
            (
                "output_config",
                {"format": {"type": "json_schema", "schema": {}, "extra": 1}},
            ),
            (
                "messages",
                [
                    {
                        "role": "user",
                        "content": [{"type": "text", "text": "x", "cache_control": {}}],
                    }
                ],
            ),
        ]:
            r = request()
            r[key] = value
            with self.assertRaises(GateError):
                validate_request(json.dumps(r).encode())

    def test_response_caps_malformed_model_and_usage_keep_full_bound(self):
        for i, value in enumerate(
            [
                b"x" * (1024 * 1024 + 1),
                b"{",
                json.dumps(response(MODELS[1])).encode(),
                b'{"model":"claude-haiku-5-5","usage":{"input_tokens":true}}',
            ]
        ):
            with tempfile.TemporaryDirectory(prefix="ask-bad-response-") as d:
                d = pathlib.Path(d)
                w = Wallet(d / "w", d / "grant", {"run": str(i)})
                g = Gate(w, "FAKE", lambda *args, value=value: value)
                g.set_case("case")
                try:
                    with self.assertRaises(GateError):
                        g.process(json.dumps(request()).encode())
                    self.assertEqual(
                        w.snapshot_report()["unresolvedUpperNanodollars"],
                        BOUNDS[MODELS[0]],
                    )
                    with self.assertRaises(GateError):
                        g.process(json.dumps(request()).encode())
                    self.assertEqual(w.snapshot_report()["acceptedRequests"], 1)
                finally:
                    w.close()

    def test_real_storage_crash_phases(self):
        python = str(pathlib.Path(sys.base_prefix) / "bin/python3")
        for phase in (
            "before_commit",
            "after_reserve",
            "after_response",
            "before_settlement",
        ):
            with tempfile.TemporaryDirectory(prefix="ask-storage-phase-") as d:
                d = pathlib.Path(d)
                source = d / "crash.py"
                source.write_text(
                    "import sys,os,json\nsys.path.insert(0,"
                    + repr(str(pathlib.Path(__file__).parent / "eval_support"))
                    + ")\nfrom anthropic_gate import Wallet,Gate,MODELS\nw=Wallet("
                    + repr(str(d / "wallet"))
                    + ","
                    + repr(str(d / "grant"))
                    + ',{"run":"crash"})\ndef fault(p):\n if p=='
                    + repr(phase)
                    + ":os._exit(23)\nw.fault=fault\ndef send(*args):\n open("
                    + repr(str(d / "sent"))
                    + ',"x").write("one")\n return json.dumps({"model":MODELS[0],"usage":{"input_tokens":1,"output_tokens":1,"service_tier":"standard","inference_geo":"global"}}).encode()\ng=Gate(w,"FAKE",send);g.set_case("case")\ng.process(json.dumps({"model":MODELS[0],"max_tokens":1024,"system":"x","messages":[{"role":"user","content":"x"}]}).encode())\n'
                )
                r = capture_child(
                    [python, "-I", "-S", "-B", str(source)],
                    d / "capture",
                    time.monotonic() + 5,
                )
                self.assertEqual(r["returncode"], 23)
                db = sqlite3.connect(d / "wallet/wallet.sqlite3")
                self.assertEqual(
                    db.execute("PRAGMA integrity_check").fetchone()[0], "ok"
                )
                rows = db.execute("SELECT state,bound FROM attempts").fetchall()
                db.close()
                if phase == "before_commit":
                    self.assertEqual(rows, [])
                else:
                    self.assertEqual(rows, [("RESERVED_UNRESOLVED", BOUNDS[MODELS[0]])])
                self.assertEqual(
                    (d / "sent").exists(),
                    phase in ("after_response", "before_settlement"),
                )
                with self.assertRaises(FileExistsError):
                    Wallet(d / "another", d / "grant", {"run": "crash"})

    def test_owner_process_exclusive_claim(self):
        python = str(pathlib.Path(sys.base_prefix) / "bin/python3")
        with tempfile.TemporaryDirectory(prefix="ask-owner-") as d:
            d = pathlib.Path(d)
            w = Wallet(d / "w", d / "grant", {"run": "owned"})
            try:
                source = d / "other.py"
                source.write_text(
                    "import sys\nsys.path.insert(0,"
                    + repr(str(pathlib.Path(__file__).parent / "eval_support"))
                    + ")\nfrom anthropic_gate import Wallet\ntry:Wallet("
                    + repr(str(d / "replacement"))
                    + ","
                    + repr(str(d / "grant"))
                    + ',{"run":"other"})\nexcept FileExistsError:raise SystemExit(0)\nraise SystemExit(41)\n'
                )
                r = capture_child(
                    [python, "-I", "-S", "-B", str(source)],
                    d / "capture",
                    time.monotonic() + 5,
                )
                self.assertEqual(r["returncode"], 0)
                self.assertFalse((d / "replacement").exists())
            finally:
                w.close()


class SQLiteFailures(unittest.TestCase):
    def test_actual_journal_insert_and_commit_refusal(self):
        for phase in ("journal", "reserve_commit", "settle_commit"):
            with tempfile.TemporaryDirectory(prefix="ask-sqlite-refusal-") as d:
                d = pathlib.Path(d)
                w = Wallet(d / "wallet", d / "claim", {"run": phase})
                calls = []

                def deny_commit(action, arg1, arg2, db, trigger):
                    if action == sqlite3.SQLITE_TRANSACTION and arg1 == "COMMIT":
                        return sqlite3.SQLITE_DENY
                    return sqlite3.SQLITE_OK

                if phase == "journal":
                    w.db.execute(
                        "CREATE TRIGGER refuse_journal BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT,'synthetic journal refusal'); END"
                    )
                if phase == "reserve_commit":
                    w.db.set_authorizer(deny_commit)

                def send(*args, calls=calls, phase=phase, w=w):
                    calls.append(True)
                    if phase == "settle_commit":
                        w.db.set_authorizer(deny_commit)
                    return json.dumps(response()).encode()

                g = Gate(w, "FAKE", send)
                g.set_case("case")
                try:
                    with self.assertRaises(GateError):
                        g.process(json.dumps(request()).encode())
                    w.db.set_authorizer(None)
                    self.assertTrue(g.stopped)
                    self.assertEqual(len(calls), int(phase == "settle_commit"))
                    state = w.snapshot_report()
                    self.assertEqual(
                        state["acceptedRequests"], int(phase == "settle_commit")
                    )
                    self.assertEqual(
                        state["unresolvedUpperNanodollars"],
                        BOUNDS[MODELS[0]] if phase == "settle_commit" else 0,
                    )
                    self.assertEqual(state["knownSettledNanodollars"], 0)
                finally:
                    w.close()


class SettlementDurability(unittest.TestCase):
    def test_committed_settlement_directory_failure_keeps_full_bound(self):
        with tempfile.TemporaryDirectory(prefix="ask-settle-sync-") as d:
            d = pathlib.Path(d)
            w = Wallet(d / "wallet", d / "grant", {"run": "synthetic-sync"})
            settling = False

            def fault(phase):
                nonlocal settling
                if phase == "after_response":
                    settling = True
                if settling and phase == "after_commit_before_directory_sync":
                    raise OSError("synthetic post-commit directory EIO")

            w.fault = fault
            calls = []
            g = Gate(
                w,
                "FAKE",
                lambda *args: calls.append(True) or json.dumps(response()).encode(),
            )
            g.set_case("case")
            try:
                with self.assertRaises(GateError):
                    g.process(json.dumps(request()).encode())
                self.assertEqual(len(calls), 1)
                self.assertEqual(
                    w.db.execute("SELECT state,actual FROM attempts").fetchone(),
                    ("SETTLED", 2000),
                )
                report = w.snapshot_report()
                self.assertEqual(report["knownSettledNanodollars"], 0)
                self.assertEqual(
                    report["unresolvedUpperNanodollars"], BOUNDS[MODELS[0]]
                )
                self.assertEqual(report["settlementDurabilityUncertainIds"], [1])
                with self.assertRaises(GateError):
                    w.reserve_attempt(MODELS[0], "case", "0" * 64)
                with self.assertRaises(GateError):
                    g.process(json.dumps(request()).encode())
                self.assertEqual(len(calls), 1)
                w.fault = lambda phase: None
                self.assertEqual(
                    w.snapshot_report()["upperLiabilityNanodollars"], BOUNDS[MODELS[0]]
                )
            finally:
                w.close()

    def test_rejected_duplicate_does_not_taint_confirmed_settlement(self):
        with tempfile.TemporaryDirectory(prefix="ask-settle-once-") as d:
            d = pathlib.Path(d)
            w = Wallet(d / "wallet", d / "grant", {"run": "synthetic-once"})
            try:
                ident = w.reserve_attempt(MODELS[0], "case", "0" * 64)
                w.settle_once(ident, 2000, {})
                with self.assertRaises(GateError):
                    w.settle_once(ident, 0, {})
                report = w.snapshot_report()
                self.assertEqual(report["knownSettledNanodollars"], 2000)
                self.assertEqual(report["unresolvedUpperNanodollars"], 0)
                self.assertEqual(report["settlementDurabilityUncertainIds"], [])
            finally:
                w.close()


if __name__ == "__main__":
    unittest.main()
