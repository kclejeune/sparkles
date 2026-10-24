"""The proxy and ledger against loopback fakes. No test reaches a real provider."""

import http.client
import json
import pathlib
import socket
import sys
import tempfile
import threading
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "eval_support"))
import proxy as px
from evaluator import Invalid, leaf_environment

HAIKU, SONNET, OPUS = px.MODELS
KEY = "sk-ant-FAKE-KEY-FOR-TESTS"
REPLY = {
    "id": "msg_x",
    "type": "message",
    "role": "assistant",
    "model": HAIKU,
    "content": [{"type": "text", "text": "{}"}],
    "stop_reason": "end_turn",
    "usage": {
        "input_tokens": 10,
        "output_tokens": 2,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "service_tier": "standard",
        "inference_geo": "global",
    },
}
BODY = json.dumps(REPLY).encode()
HAIKU_COST = 10 * 100 + 2 * 500


def request(model=HAIKU, text="q", **extra):
    return dict(
        {
            "model": model,
            "max_tokens": 1024,
            "system": "s",
            "messages": [{"role": "user", "content": text}],
        },
        **extra,
    )


def raw(**kw):
    return json.dumps(request(**kw)).encode()


def wire_server(chunks, close=False):
    """One reply, byte for byte, on loopback. Returns the port."""
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)

    def serve():
        conn, _ = srv.accept()
        with conn:
            data = b""
            while b"\r\n\r\n" not in data:
                more = conn.recv(65536)
                if not more:
                    break
                data += more
            head, _, rest = data.partition(b"\r\n\r\n")
            length = 0
            for line in head.split(b"\r\n"):
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":")[1])
            while len(rest) < length:  # An unread request would reset the connection.
                rest += conn.recv(65536)
            for chunk in chunks:
                conn.sendall(chunk)
        srv.close()

    threading.Thread(target=serve, daemon=True).start()
    return srv.getsockname()[1]


def framings(body):
    half = len(body) // 2
    head = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
    return {
        "content-length": [
            head + b"Content-Length: %d\r\n\r\n" % len(body) + body,
        ],
        "chunked": [
            head
            + b"Transfer-Encoding: chunked\r\n\r\n"
            + b"%x\r\n" % half
            + body[:half]
            + b"\r\n"
            + b"%x\r\n" % (len(body) - half)
            + body[half:]
            + b"\r\n0\r\n\r\n"
        ],
        "close-delimited": [head + b"Connection: close\r\n\r\n" + body],
    }


class Upstream:
    """A scripted stand-in for HTTPUpstream. Each script entry is one reply."""

    def __init__(self, *script):
        self.script, self.calls = list(script), []

    def send(self, path, body, timeout):
        self.calls.append((path, json.loads(body)))
        step = self.script.pop(0) if self.script else (200, [], BODY)
        if isinstance(step, Exception):
            raise step
        return step


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="ask-proxy-")
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.ledgers = []

    def ledger(self, cap=10**10, carryover=None, name="ledger.jsonl"):
        ledger = px.Ledger(self.root / name, cap, carryover)
        self.ledgers.append(ledger)
        self.addCleanup(ledger.close)
        return ledger

    def proxy(self, upstream, **kw):
        ledger = kw.pop("ledger", None) or self.ledger()
        return px.Proxy(ledger, upstream, KEY, **kw)

    def lines(self, name="ledger.jsonl"):
        return [json.loads(x) for x in (self.root / name).read_text().splitlines()]


class Framing(Base):
    def test_reply_framings_all_settle_at_the_exact_cost(self):
        for name, chunks in framings(BODY).items():
            with self.subTest(framing=name):
                port = wire_server(chunks)
                upstream = px.HTTPUpstream(KEY, "127.0.0.1", port, tls=False)
                ledger = self.ledger(name=name + ".jsonl")
                proxy = px.Proxy(ledger, upstream, KEY)
                status, _, body = proxy.handle(raw())
                self.assertEqual(status, 200)
                self.assertEqual(json.loads(body), REPLY)
                self.assertEqual(ledger.report()["settledNanodollars"], HAIKU_COST)
                self.assertEqual(ledger.report()["unresolvedNanodollars"], 0)

    def test_fixture_from_a_real_reply_when_present(self):
        path = HERE / "fixtures/anthropic-reply-headers.json"
        if not path.is_file():
            self.skipTest("the fixture is captured by the first paid preflight")
        fixture = json.loads(path.read_text())
        head = b"HTTP/1.1 %d OK\r\n" % fixture["message"]["status"]
        for key, value in fixture["message"]["headers"]:
            if key.lower() not in ("content-length", "transfer-encoding"):
                head += f"{key}: {value}\r\n".encode()
        chunked = head + b"Transfer-Encoding: chunked\r\n\r\n"
        chunked += b"%x\r\n" % len(BODY) + BODY + b"\r\n0\r\n\r\n"
        upstream = px.HTTPUpstream(KEY, "127.0.0.1", wire_server([chunked]), tls=False)
        proxy = px.Proxy(self.ledger(), upstream, KEY)
        self.assertEqual(proxy.handle(raw())[0], 200)

    def test_chunked_reply_is_not_refused(self):
        # The pilot's gate refused any Transfer-Encoding. http.client decodes it.
        port = wire_server(framings(BODY)["chunked"])
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
        self.addCleanup(conn.close)
        conn.request("POST", "/v1/messages", body=b"{}")
        self.assertEqual(json.loads(conn.getresponse().read()), REPLY)

    def test_the_pilots_first_request_now_settles_at_its_exact_cost(self):
        # The pilot stopped with upstream_framing on its first request, a Haiku draft
        # of lookup-01, 7.3 s after it left. Its gate took only a single
        # Content-Length, so a 2xx reply in chunks was refused after the model had
        # run, and the reserve stayed at its bound. The same shape of reply, chunked
        # with the API's usual headers and a draft's usage, settles here exactly.
        usage = dict(REPLY["usage"], input_tokens=10240, output_tokens=380)
        body = json.dumps(dict(REPLY, usage=usage)).encode()
        head = (
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
            b"request-id: req_pilot\r\nanthropic-organization-id: org\r\n"
            b"Transfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n"
        )
        third = len(body) // 3
        parts = [body[:third], body[third : 2 * third], body[2 * third :]]
        wire = head + b"".join(b"%x\r\n" % len(p) + p + b"\r\n" for p in parts)
        wire += b"0\r\n\r\n"
        upstream = px.HTTPUpstream(KEY, "127.0.0.1", wire_server([wire]), tls=False)
        carry = {"name": "pilot", "cost": 1_002_560_000}
        ledger = self.ledger(cap=10**10, carryover=carry)
        proxy = px.Proxy(ledger, upstream, KEY)
        self.assertEqual(proxy.handle(raw())[0], 200)
        settle = self.lines()[-1]
        self.assertEqual(settle["cost"], 10240 * 100 + 380 * 500)
        self.assertEqual(settle["requestId"], "req_pilot")
        self.assertEqual(
            [h["target"] for h in ledger.report()["heldAtBound"]], ["carryover"]
        )

    def test_a_compressed_reply_is_decoded_and_settles_exactly(self):
        import gzip
        import zlib

        packed = {
            "gzip": gzip.compress(BODY),
            "deflate": zlib.compress(BODY),
        }
        for coding, data in packed.items():
            with self.subTest(coding=coding):
                wire = (
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                    + f"Content-Encoding: {coding}\r\n".encode()
                    + b"Content-Length: %d\r\n\r\n" % len(data)
                    + data
                )
                upstream = px.HTTPUpstream(
                    KEY, "127.0.0.1", wire_server([wire]), tls=False
                )
                ledger = self.ledger(name=coding + ".jsonl")
                status, _, body = px.Proxy(ledger, upstream, KEY).handle(raw())
                self.assertEqual((status, json.loads(body)), (200, REPLY))
                self.assertEqual(ledger.report()["settledNanodollars"], HAIKU_COST)

    def test_a_compression_bomb_is_a_lost_reply(self):
        import gzip

        data = gzip.compress(b" " * (px.RESPONSE_CAP + 10))
        wire = (
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n"
            + b"Content-Length: %d\r\n\r\n" % len(data)
            + data
        )
        upstream = px.HTTPUpstream(KEY, "127.0.0.1", wire_server([wire]), tls=False)
        ledger = self.ledger()
        self.assertEqual(px.Proxy(ledger, upstream, KEY).handle(raw())[0], 502)
        reserve, settle = self.lines()[-2:]
        self.assertEqual(settle["cost"], reserve["bound"])
        self.assertEqual(settle["status"], "failure:reply too large")


class Reconcile(Base):
    CARRY = {"name": "pilot", "cost": 1_002_560_000}

    def test_the_carryover_is_reconciled_to_the_billed_amount(self):
        ledger = self.ledger(cap=10**10, carryover=self.CARRY)
        self.assertEqual(ledger.spent(), self.CARRY["cost"])
        line = ledger.reconcile(
            "carryover", 1_600_000, "Console cost on 2026-10-15 for the pilot key"
        )
        self.assertEqual(line["previous"], self.CARRY["cost"])
        self.assertEqual(ledger.spent(), 1_600_000)
        self.assertEqual(ledger.report()["heldAtBound"], [])
        with self.assertRaises(Invalid):
            ledger.reconcile("carryover", 0, "again")
        ledger.close()
        again = px.Ledger(self.root / "ledger.jsonl", 10**10, self.CARRY)
        self.addCleanup(again.close)
        self.assertEqual(again.spent(), 1_600_000)
        self.assertEqual(again.report()["reconciled"][0]["target"], "carryover")

    def test_a_reconcile_needs_evidence_and_a_held_target(self):
        ledger = self.ledger(cap=10**10, carryover=self.CARRY)
        ok = ledger.reserve(HAIKU, 1000, "case")
        ledger.settle(ok, 30, "ok")
        for target, amount, evidence in (
            ("carryover", 5, ""),
            ("carryover", 5, "   "),
            ("carryover", 5, "x" * 1001),
            ("carryover", -1, "Console cost"),
            ("carryover", 1.5, "Console cost"),
            (ok, 5, "Console cost"),  # settled from its usage, so nothing is held
            (999, 5, "Console cost"),
            ("carryover", 5, "short"),
            ("carryover", 5, "Console\ncost of the day"),
            ("carryover", 5, " Console cost "),
            ("carryover", 10**12, "Console cost of the day"),
        ):
            with self.subTest(target=target, amount=amount):
                with self.assertRaises(Invalid):
                    ledger.reconcile(target, amount, evidence)
        self.assertEqual(ledger.spent(), self.CARRY["cost"] + 30)
        self.assertEqual(len(self.lines()), 4)  # no reconcile line was written

    def test_a_reply_without_usage_and_a_crashed_reserve_are_reconciled(self):
        reply = dict(REPLY)
        del reply["usage"]
        upstream = Upstream(
            (200, [("request-id", "req_1")], json.dumps(reply).encode())
        )
        ledger = self.ledger(cap=10**10, carryover=self.CARRY)
        px.Proxy(ledger, upstream, KEY).handle(raw())
        crashed = ledger.reserve(SONNET, 5000, "case-2")
        ledger.close()
        again = px.Ledger(self.root / "ledger.jsonl", 10**10, self.CARRY)
        self.addCleanup(again.close)
        held = {h["target"]: h for h in again.report()["heldAtBound"]}
        self.assertEqual(set(held), {"carryover", 1, crashed})
        self.assertEqual(held[1]["status"], "usage-unknown")
        self.assertEqual(held[1]["requestId"], "req_1")
        self.assertEqual(held[crashed]["status"], "unsettled")
        before = again.spent()
        again.reconcile(1, 2000, "usage export row req_1")
        again.reconcile(crashed, 0, "no request reached the API, per the Console log")
        self.assertEqual(
            again.spent(), before - held[1]["cost"] + 2000 - held[crashed]["cost"]
        )
        self.assertEqual(again.case_costs()[("case-2", SONNET)], 0)
        self.assertEqual(
            {h["target"] for h in again.report()["heldAtBound"]}, {"carryover"}
        )
        again.close()
        third = px.Ledger(self.root / "ledger.jsonl", 10**10, self.CARRY)
        self.addCleanup(third.close)
        self.assertEqual(third.spent(), before - held[1]["cost"] + 2000 - 5000)

    def test_a_tampered_reconcile_line_is_refused_on_replay(self):
        ledger = self.ledger(cap=10**10, carryover=self.CARRY)
        ledger.close()
        path = self.root / "ledger.jsonl"
        bad = {
            "op": "reconcile",
            "target": "carryover",
            "previous": 1,
            "cost": 0,
            "evidence": "Console cost of the day",
        }
        with path.open("a") as f:
            f.write(json.dumps(bad) + "\n")
        with self.assertRaises(Invalid):
            px.Ledger(path, 10**10, self.CARRY)

    def test_a_lower_bill_is_recorded_and_a_higher_one_needs_allow_raise(self):
        ledger = self.ledger(cap=10**10, carryover=self.CARRY)
        evidence = "Console cost of 2026-10-15"
        with self.assertRaises(Invalid):
            ledger.reconcile("carryover", self.CARRY["cost"] + 1, evidence)
        self.assertEqual(len(self.lines()), 2)
        line = ledger.reconcile("carryover", self.CARRY["cost"] + 1, evidence, True)
        self.assertIs(line["raise"], True)
        self.assertEqual(ledger.spent(), self.CARRY["cost"] + 1)
        ledger.close()
        again = px.Ledger(self.root / "ledger.jsonl", 10**10, self.CARRY)
        self.addCleanup(again.close)
        self.assertEqual(again.spent(), self.CARRY["cost"] + 1)
        lower = self.ledger(cap=10**10, carryover=self.CARRY, name="lower.jsonl")
        self.assertNotIn("raise", lower.reconcile("carryover", 1_600_000, evidence))
        self.assertEqual(lower.spent(), 1_600_000)

    def test_every_reconcile_check_runs_on_replay(self):
        good = {
            "op": "reconcile",
            "target": "carryover",
            "previous": self.CARRY["cost"],
            "cost": 1_600_000,
            "evidence": "Console cost of the day",
        }
        raised = self.CARRY["cost"] + 1
        cases = {
            "evidence": [dict(good, evidence="")],
            "type": [dict(good, cost="1600000")],
            "unknown target": [dict(good, target=7)],
            "twice": [good, dict(good, previous=1_600_000)],
            "raise without flag": [dict(good, cost=raised)],
            "flag not a bool": [dict(good, cost=raised, **{"raise": 1})],
            "past the hard cap": [dict(good, cost=px.HARD_CAP + 1, **{"raise": True})],
        }
        for name, lines in cases.items():
            with self.subTest(name):
                path = self.root / (name.replace(" ", "-") + ".jsonl")
                px.Ledger(path, 10**10, self.CARRY).close()
                with path.open("a") as f:
                    f.writelines(json.dumps(x) + "\n" for x in lines)
                with self.assertRaises(Invalid):
                    px.Ledger(path, 10**10, self.CARRY)


class Replay(Base):
    """A ledger is replayed strictly. Each case appends lines to a fresh ledger."""

    CARRY = {"name": "pilot", "cost": 500}

    def refused(self, *lines, carryover=CARRY):
        path = self.root / "tampered.jsonl"
        if path.exists():
            path.unlink()
        px.Ledger(path, 10**9, carryover).close()
        with path.open("a") as f:
            f.writelines(json.dumps(x) + "\n" for x in lines)
        with self.assertRaises(Invalid):
            px.Ledger(path, 10**9, carryover)

    def reserve(self, ident=1, bound=100):
        return {"op": "reserve", "id": ident, "model": HAIKU, "bound": bound}

    def settle(self, ident=1, cost=5):
        return {"op": "settle", "id": ident, "cost": cost, "status": "ok"}

    def test_a_valid_ledger_replays(self):
        path = self.root / "ok.jsonl"
        px.Ledger(path, 10**9, self.CARRY).close()
        with path.open("a") as f:
            for x in (self.reserve(), self.settle(), self.reserve(2)):
                f.write(json.dumps(x) + "\n")
        ledger = px.Ledger(path, 10**9, self.CARRY)
        self.addCleanup(ledger.close)
        self.assertEqual(ledger.spent(), 500 + 5 + 100)

    def test_negative_or_non_integer_amounts_are_refused(self):
        for lines in (
            [self.reserve(), self.settle(cost=-95)],
            [self.reserve(bound="100")],
            [self.reserve(bound=-1)],
            [self.reserve(bound=True)],
            [self.reserve(bound=1.5)],
            [self.reserve(), self.settle(cost=2.5)],
        ):
            with self.subTest(lines=lines):
                self.refused(*lines)

    def test_bad_ids_and_orphans_are_refused(self):
        for lines in (
            [self.reserve(ident=0)],
            [self.reserve(ident="1")],
            [self.reserve(), self.reserve()],
            [self.reserve(), self.settle(), self.reserve()],
            [self.settle(ident=9)],
            [self.reserve(), self.settle(), self.settle()],
            [dict(self.reserve(), model="claude-unknown")],
            [["not", "a", "line"]],
        ):
            with self.subTest(lines=lines):
                self.refused(*lines)

    def test_a_carryover_must_follow_the_header_once(self):
        self.refused({"carryover": "again", "cost": 1})
        self.refused({"carryover": "pilot", "cost": -500}, carryover=None)
        path = self.root / "late.jsonl"
        head = {"ledger": px.LEDGER_VERSION, "capNanodollars": 10**9}
        late = (head, self.reserve(), {"carryover": "pilot", "cost": 500})
        path.write_text("".join(json.dumps(x) + "\n" for x in late))
        with self.assertRaises(Invalid):
            px.Ledger(path, 10**9, self.CARRY)


class Rates(unittest.TestCase):
    def test_the_rate_table_is_pinned(self):
        # Anthropic's model and pricing documentation of October 2026, in nanodollars
        # per token: input, 5 minute write, 1 hour write, cache read, output.
        self.assertEqual(px.MODELS, (HAIKU, SONNET, OPUS))
        self.assertEqual(
            px.MODELS, ("claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5")
        )
        self.assertEqual(px.LONG_CONTEXT, 100000)
        self.assertEqual(
            px.RATES,
            {
                HAIKU: ((100, 125, 200, 10, 500), (500, 625, 1000, 50, 2500)),
                SONNET: ((2000, 2500, 4000, 200, 10000),) * 2,
                OPUS: ((4000, 5000, 8000, 200, 20000),) * 2,
            },
        )

    def test_cache_reads_on_every_model(self):
        usage = {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 1000}
        self.assertEqual(px.cost(HAIKU, usage), 1000 * 10)  # $0.01 per million
        self.assertEqual(px.cost(SONNET, usage), 1000 * 200)  # $0.20 per million
        self.assertEqual(px.cost(OPUS, usage), 1000 * 200)  # $0.20 per million
        big = dict(usage, cache_read_input_tokens=100001)
        self.assertEqual(px.cost(HAIKU, big), 100001 * 50)
        self.assertEqual(px.cost(OPUS, big), 100001 * 200)


class Errors(Base):
    def test_one_upstream_error_does_not_end_the_campaign(self):
        upstream = Upstream(px.UpstreamFailure("timeout", True))
        proxy = self.proxy(upstream)
        self.assertEqual(proxy.handle(raw())[0], 502)
        self.assertEqual(proxy.handle(raw())[0], 200)
        self.assertEqual(len(upstream.calls), 2)

    def test_529_then_200_both_reach_upstream_and_settle(self):
        error = (529, [("retry-after", "1")], b'{"type":"error"}')
        upstream = Upstream(error)
        proxy = self.proxy(upstream)
        status, headers, body = proxy.handle(raw())
        self.assertEqual((status, dict(headers)["retry-after"]), (529, "1"))
        self.assertEqual(body, b'{"type":"error"}')
        self.assertEqual(proxy.handle(raw())[0], 200)
        settles = [x for x in self.lines() if x.get("op") == "settle"]
        self.assertEqual([x["cost"] for x in settles], [0, HAIKU_COST])
        self.assertEqual(len(upstream.calls), 2)

    def test_timeout_after_send_settles_at_the_bound_and_dispatch_goes_on(self):
        upstream = Upstream(px.UpstreamFailure("timeout", True))
        proxy = self.proxy(upstream)
        proxy.handle(raw())
        reserve, settle = self.lines()[1:3]
        self.assertEqual(settle["cost"], reserve["bound"])
        self.assertEqual(proxy.handle(raw())[0], 200)

    def test_failure_before_the_body_left_costs_nothing(self):
        proxy = self.proxy(Upstream(px.UpstreamFailure("gaierror", False)))
        proxy.handle(raw())
        self.assertEqual(self.lines()[2]["cost"], 0)

    def test_timeout_is_enforced_for_a_silent_server(self):
        srv = socket.socket()
        srv.bind(("127.0.0.1", 0))
        srv.listen(1)
        self.addCleanup(srv.close)
        upstream = px.HTTPUpstream(KEY, "127.0.0.1", srv.getsockname()[1], tls=False)
        with self.assertRaises(px.UpstreamFailure) as caught:
            upstream.send("/v1/messages", b"{}", 0.3)
        self.assertTrue(caught.exception.sent)

    def test_missing_usage_settles_at_the_bound(self):
        replies = ({"model": HAIKU}, {"usage": {"input_tokens": "x"}}, None)
        for n, reply in enumerate(replies):
            with self.subTest(reply=reply):
                data = b"not json" if reply is None else json.dumps(reply).encode()
                name = f"missing-{n}.jsonl"
                proxy = self.proxy(
                    Upstream((200, [], data)), ledger=self.ledger(name=name)
                )
                proxy.handle(raw())
                reserve, settle = self.lines(name)[-2:]
                self.assertEqual(settle["cost"], reserve["bound"])

    def test_additive_usage_fields_are_ignored(self):
        usage = dict(REPLY["usage"], speed="standard", surprise={"a": 1})
        self.assertEqual(px.cost(HAIKU, usage), HAIKU_COST)

    def test_priced_or_unknown_tiers_are_never_undercounted(self):
        usage = dict(REPLY["usage"])
        base = px.cost(SONNET, usage)
        # A US-only reply carries a premium on every model.
        for model in (HAIKU, SONNET, OPUS):
            exact = px.cost(model, usage)
            self.assertGreater(px.cost(model, dict(usage, inference_geo="us")), exact)
        self.assertGreaterEqual(
            px.cost(SONNET, dict(usage, inference_geo="us")), base * 11 // 10
        )
        # Anything unrecognised settles at the bound or at a large multiple.
        limit = 10**8
        for field, value in (
            ("service_tier", "priority"),
            ("inference_geo", "mars"),
            ("speed", "fast"),
        ):
            for model in (HAIKU, SONNET, OPUS):
                with self.subTest(field=field, model=model):
                    odd = dict(usage, **{field: value})
                    self.assertGreaterEqual(px.cost(model, odd, limit), limit)
                    self.assertGreaterEqual(
                        px.cost(model, odd), 6 * px.cost(model, usage)
                    )

    def test_a_settlement_above_its_bound_is_recorded(self):
        ledger = self.ledger()
        ident = ledger.reserve(HAIKU, 100)
        ledger.settle(ident, 101, "ok")
        self.assertEqual(ledger.report()["settledAboveBound"], 1)
        self.assertTrue(self.lines()[-1]["overBound"])

    def test_unrecognised_tier_settles_at_the_bound_in_the_ledger(self):
        reply = dict(REPLY, usage=dict(REPLY["usage"], speed="fast"))
        proxy = self.proxy(Upstream((200, [], json.dumps(reply).encode())))
        proxy.handle(raw())
        reserve, settle = self.lines()[-2:]
        self.assertGreaterEqual(settle["cost"], reserve["bound"])

    def test_cache_and_long_context_rates(self):
        usage = {
            "input_tokens": 100,
            "output_tokens": 10,
            "cache_creation_input_tokens": 30,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 10,
                "ephemeral_1h_input_tokens": 20,
            },
            "cache_read_input_tokens": 50,
        }
        self.assertEqual(
            px.cost(SONNET, usage),
            100 * 2000 + 10 * 2500 + 20 * 4000 + 50 * 200 + 10 * 10000,
        )
        usage = dict(usage, cache_creation={"ephemeral_5m_input_tokens": 1})
        self.assertIsNone(px.cost(SONNET, usage))
        big = {"input_tokens": 100001, "output_tokens": 1}
        self.assertEqual(px.cost(HAIKU, big), 100001 * 500 + 2500)

    def test_any_other_upstream_fault_settles_at_the_bound(self):
        proxy = self.proxy(Upstream(RuntimeError("boom " + KEY)))
        status, _, body = proxy.handle(raw())
        self.assertEqual(status, 502)
        self.assertNotIn(KEY.encode(), body)
        reserve, settle = self.lines()[-2:]
        self.assertEqual(settle["cost"], reserve["bound"])
        self.assertEqual(settle["status"], "failure:internal-RuntimeError")
        self.assertEqual(proxy.ledger.report()["unresolvedNanodollars"], 0)

    def test_a_header_error_never_carries_the_key(self):
        bad = KEY + "\n"
        port = wire_server(framings(BODY)["content-length"])
        upstream = px.HTTPUpstream(bad, "127.0.0.1", port, tls=False)
        with self.assertRaises(px.UpstreamFailure) as caught:
            upstream.send("/v1/messages", b"{}", 5)
        self.assertNotIn(KEY, str(caught.exception))
        self.assertIsNone(caught.exception.__cause__)
        self.assertEqual(caught.exception.reason, "ValueError")

    def test_the_key_is_stripped_and_checked_once(self):
        self.assertEqual(px.clean_key(KEY + "\n"), KEY)
        for bad in ("", "  ", "sk-ant a", "sk-ant\x00a", "sk-ant-é"):
            with self.subTest(bad=bad), self.assertRaises(Invalid) as caught:
                px.clean_key(bad)
            self.assertNotIn("sk-ant", str(caught.exception))

    def test_x_gzip_decodes_and_a_corrupt_gzip_settles_at_the_bound(self):
        import gzip

        for coding, data, exact in (
            ("x-gzip", gzip.compress(BODY), True),
            ("gzip", gzip.compress(BODY)[:-12] + b"garbage-tail", False),
        ):
            with self.subTest(coding=coding, exact=exact):
                wire = (
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                    + f"Content-Encoding: {coding}\r\n".encode()
                    + b"Content-Length: %d\r\n\r\n" % len(data)
                    + data
                )
                upstream = px.HTTPUpstream(
                    KEY, "127.0.0.1", wire_server([wire]), tls=False
                )
                name = f"{coding}-{exact}.jsonl"
                ledger = self.ledger(name=name)
                px.Proxy(ledger, upstream, KEY).handle(raw())
                reserve, settle = self.lines(name)[-2:]
                if exact:
                    self.assertEqual(settle["cost"], HAIKU_COST)
                else:
                    self.assertEqual(settle["cost"], reserve["bound"])
                    self.assertEqual(settle["status"], "usage-unknown")


class Cap(Base):
    def test_boundary(self):
        limit = px.bound(
            HAIKU, len(json.dumps(self.sent(), separators=(",", ":"))), 1024
        )
        exact = self.proxy(
            Upstream(), ledger=self.ledger(cap=limit, name="exact.jsonl")
        )
        self.assertEqual(exact.handle(raw())[0], 200)
        short = self.proxy(
            Upstream(), ledger=self.ledger(cap=limit - 1, name="short.jsonl")
        )
        self.assertEqual(short.handle(raw())[0], 402)
        self.assertEqual(short.cap_refusals, 1)
        self.assertEqual(short.upstream.calls, [])
        self.assertEqual(len(self.lines("short.jsonl")), 1)

    def sent(self):
        """The body as the proxy forwards it, which sets stream and effort."""
        upstream = Upstream()
        self.proxy(upstream, ledger=self.ledger(name="probe.jsonl")).handle(raw())
        return upstream.calls[0][1]

    def test_size_based_bound(self):
        # The worst Opus request at the 128 KiB cap stays near a dollar.
        worst = px.bound(OPUS, px.REQUEST_CAP, 4096)
        self.assertLess(worst, 1.3 * 10**9)
        self.assertGreater(worst, 1.0 * 10**9)

    def test_replay_counts_unsettled_reserves_and_carryover(self):
        carry = {"name": "pilot", "cost": 500}
        ledger = self.ledger(cap=10**9, carryover=carry)
        ident = ledger.reserve(HAIKU, 1000, "case")
        ledger.settle(ledger.reserve(HAIKU, 700, "case"), 30, "ok")
        ledger.close()
        again = px.Ledger(self.root / "ledger.jsonl", 10**9, carry)
        self.addCleanup(again.close)
        self.assertEqual(again.spent(), 500 + 30 + 1000)
        self.assertEqual(again.next_id, ident + 2)
        again.close()
        for cap, carryover in (
            (10**9 + 1, carry),
            (10**9, None),
            (10**9, {"name": "x", "cost": 1}),
        ):
            with self.subTest(cap=cap, carryover=carryover), self.assertRaises(Invalid):
                px.Ledger(self.root / "ledger.jsonl", cap, carryover)

    def test_lock_and_unknown_version(self):
        ledger = self.ledger()
        with self.assertRaises(Invalid):
            px.Ledger(self.root / "ledger.jsonl", ledger.cap)
        ledger.close()
        path = self.root / "ledger.jsonl"
        for text in (
            '{"ledger":3,"capNanodollars":1}\n',
            '{"ledger":1,"capNanodollars":1}\n',
            '{"ledger":2,"capNanodollars":1}\n{"op":"settle","id":9}\n',
            '{"ledger":2,"capNanodollars":2}\n',
        ):
            with self.subTest(text=text):
                path.write_text(text)
                with self.assertRaises(Invalid):
                    px.Ledger(path, 1)
        self.assertFalse((self.root / "ledger.jsonl.lock").exists())

    def test_stale_lock_of_a_dead_owner_is_recovered_and_a_live_one_is_not(self):
        import subprocess

        dead = subprocess.Popen([sys.executable, "-c", "pass"])
        dead.wait()
        lock = self.root / "ledger.jsonl.lock"
        lock.write_text(str(dead.pid))
        ledger = self.ledger()  # The owner is gone, so the lock is stale.
        self.assertEqual(lock.read_text(), str(__import__("os").getpid()))
        ledger.close()
        self.assertFalse(lock.exists())
        for owner in (str(__import__("os").getpid()), "", "not a pid"):
            with self.subTest(owner=owner):
                lock.write_text(owner)
                with self.assertRaises(Invalid):
                    px.Ledger(self.root / "ledger.jsonl", 10**10)
                self.assertTrue(lock.exists())
                lock.unlink()

    def test_the_cap_is_global_not_per_ledger_file(self):
        carry = {"name": "pilot", "cost": 1_002_560_000}
        self.assertEqual(px.HARD_CAP, 50 * 10**9)
        with self.assertRaises(Invalid):
            px.Ledger(self.root / "big.jsonl", px.HARD_CAP, carry)  # with carryover
        with self.assertRaises(Invalid):
            px.Ledger(self.root / "none.jsonl", px.HARD_CAP + 1)
        self.ledger(cap=px.HARD_CAP - carry["cost"], carryover=carry)
        # A second ledger file beside the first is refused for a real run.
        with self.assertRaises(Invalid):
            px.require_single_ledger(self.root / "other.jsonl")
        px.require_single_ledger(self.root / "ledger.jsonl")

    def test_wait_idle_waits_for_a_request_in_flight(self):
        gate, started = threading.Event(), threading.Event()

        class Slow(Upstream):
            def send(self, path, body, timeout):
                started.set()
                gate.wait(5)
                return super().send(path, body, timeout)

        proxy = self.proxy(Slow())
        proxy.set_case("c")
        thread = threading.Thread(target=proxy.handle, args=(raw(),))
        thread.start()
        started.wait(5)
        self.assertFalse(proxy.wait_idle(0.05))  # Still in flight.
        gate.set()
        self.assertTrue(proxy.wait_idle(5))
        thread.join(5)
        self.assertEqual(proxy.case_cost("c"), HAIKU_COST)

    def test_concurrent_reserves_never_pass_the_cap(self):
        ledger = self.ledger(cap=1000)
        barrier = threading.Barrier(16)
        admitted, refused = [], []

        def take():
            barrier.wait(5)
            for _ in range(10):
                try:
                    admitted.append(ledger.reserve(HAIKU, 7, "race"))
                except px.CapExceeded:
                    refused.append(1)

        threads = [threading.Thread(target=take) for _ in range(16)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(10)
        self.assertEqual(len(admitted), 1000 // 7)
        self.assertEqual(len(set(admitted)), len(admitted))
        self.assertLessEqual(ledger.spent(), 1000)
        self.assertEqual(len(refused), 160 - 1000 // 7)

    def test_costs_by_case_and_model(self):
        proxy = self.proxy(Upstream())
        proxy.set_case("c1")
        proxy.handle(raw())
        proxy.handle(raw())
        proxy.set_case(None)
        proxy.handle(raw())
        self.assertEqual(proxy.ledger.case_costs(), {("c1", HAIKU): 2 * HAIKU_COST})


class Preflight(Base):
    def test_every_model_is_tried_with_the_runs_request_shape(self):
        upstream = Upstream()
        proxy = self.proxy(upstream, models=px.MODELS)
        result = px.preflight(proxy, upstream)
        sent = [body for path, body in upstream.calls if path == "/v1/messages"]
        self.assertEqual([b["model"] for b in sent], list(px.MODELS))
        for body in sent:
            self.assertEqual(body["output_config"]["format"]["type"], "json_schema")
            self.assertEqual(body["output_config"]["effort"], "medium")
            self.assertIs(body["stream"], False)
        self.assertEqual(set(result["models"]), set(px.MODELS))

    def test_a_failing_model_stops_the_preflight(self):
        ok = (200, [], BODY)
        for bad in ((400, [], b'{"type":"error"}'), (401, [], b"{}")):
            with self.subTest(status=bad[0]):
                upstream = Upstream(ok, ok, bad)  # count_tokens, haiku, sonnet
                proxy = self.proxy(
                    upstream,
                    ledger=self.ledger(name=f"pre-{bad[0]}.jsonl"),
                    models=px.MODELS,
                )
                with self.assertRaises(Invalid):
                    px.preflight(proxy, upstream)

    def test_identifying_headers_are_not_recorded(self):
        headers = [
            ("content-type", "application/json"),
            ("anthropic-organization-id", "org-secret"),
            ("anthropic-workspace-id", "ws-secret"),
            ("anthropic-ratelimit-requests-limit", "50"),
            ("request-id", "req_1"),
            ("set-cookie", "c"),
        ]
        upstream = Upstream((200, headers, BODY), (200, headers, BODY))
        result = px.preflight(self.proxy(upstream), upstream)
        for recorded in (result["countTokens"], result["message"]):
            self.assertEqual(
                recorded["headers"], [["content-type", "application/json"]]
            )

    def test_a_reply_that_is_not_the_requested_json_stops_the_preflight(self):
        text = dict(REPLY, content=[{"type": "text", "text": "not json"}])
        upstream = Upstream((200, [], BODY), (200, [], json.dumps(text).encode()))
        proxy = self.proxy(upstream)
        with self.assertRaises(Invalid):
            px.preflight(proxy, upstream)


class Requests(Base):
    def test_refused_requests_cost_nothing(self):
        upstream = Upstream()
        proxy = self.proxy(upstream)
        for body in (
            raw(tools=[{"name": "t"}]),
            raw(tool_choice={"type": "tool"}),
            raw(model="claude-unknown"),
            raw(max_tokens=100000),
            raw(system="x" * 200000),
            b"[]",
            raw(messages=[{"role": "user", "content": "q", "cache_control": {}}]),
            raw(service_tier="priority"),
            raw(messages=[{"role": "user", "content": [{"type": "image"}]}]),
            raw(system=[{"type": "text", "text": "s"}]),
        ):
            with self.subTest(body=body[:60]):
                self.assertEqual(proxy.handle(body)[0], 400)
        self.assertEqual(upstream.calls, [])
        self.assertEqual(len(self.lines()), 1)

    def test_proxy_sets_stream_effort_and_keeps_the_schema(self):
        upstream = Upstream()
        proxy = self.proxy(upstream, effort="medium")
        schema = {"format": {"type": "json_schema", "schema": {"type": "object"}}}
        proxy.handle(raw(stream=True, output_config=schema))
        sent = upstream.calls[0][1]
        self.assertIs(sent["stream"], False)
        self.assertEqual(sent["output_config"]["effort"], "medium")
        self.assertEqual(sent["output_config"]["format"], schema["format"])
        self.assertEqual(upstream.calls[0][0], "/v1/messages")

    def test_key_never_appears_in_children_ledger_or_replies(self):
        echo = dict(REPLY, content=[{"type": "text", "text": "the key is " + KEY}])
        upstream = Upstream(
            (200, [], json.dumps(echo).encode()),
            (400, [], ("bad " + KEY).encode()),
        )
        proxy = self.proxy(upstream)
        status, _, body = proxy.handle(raw())
        self.assertEqual(status, 200)
        self.assertNotIn(KEY.encode(), body)
        self.assertIn(b"[REDACTED]", body)
        self.assertNotIn(KEY.encode(), proxy.handle(raw())[2])
        self.assertNotIn(KEY, (self.root / "ledger.jsonl").read_text())
        server = proxy.serve()
        self.addCleanup(server.server_close)
        config = json.dumps(proxy.config())
        self.assertNotIn(KEY, config)
        self.assertNotIn(
            KEY, json.dumps(leaf_environment({"SPARKLES_EVAL_GATE_TOKEN": proxy.token}))
        )
        self.assertIn(f"127.0.0.1:{server.server_port}", config)

    def test_requests_and_replies_are_recorded_without_the_key(self):
        echo = dict(REPLY, content=[{"type": "text", "text": "key " + KEY}])
        upstream = Upstream(
            (200, [("request-id", "req_9")], json.dumps(echo).encode()),
            (529, [], b"overloaded " + KEY.encode()),
        )
        path = self.root / "replies.jsonl"
        proxy = self.proxy(upstream, replies=path)
        proxy.set_case("c1")
        proxy.handle(raw(text="question " + KEY))
        proxy.handle(raw())
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        text = path.read_text()
        self.assertNotIn(KEY, text)
        first, second = (json.loads(x) for x in text.splitlines())
        self.assertEqual(
            (first["case"], first["model"], first["status"]), ("c1", HAIKU, 200)
        )
        self.assertEqual((first["requestId"], second["requestId"]), ("req_9", None))
        # A fault while recording never costs the request its reply.
        broken = self.proxy(
            Upstream(),
            ledger=self.ledger(name="broken.jsonl"),
            replies=self.root / "no-dir" / "r.jsonl",
        )
        self.assertEqual(broken.handle(raw())[0], 200)
        self.assertEqual(
            first["request"]["messages"][0]["content"], "question [REDACTED]"
        )
        self.assertEqual(first["reply"]["content"][0]["text"], "key [REDACTED]")
        self.assertEqual(first["reply"]["usage"], REPLY["usage"])
        self.assertEqual(second["status"], 529)
        self.assertEqual(second["reply"], "overloaded [REDACTED]")

    def test_served_route_needs_the_token(self):
        proxy = self.proxy(Upstream())
        server = proxy.serve()
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        def post(token, path="/v1/messages"):
            conn = http.client.HTTPConnection(
                "127.0.0.1", server.server_port, timeout=5
            )
            conn.request(
                "POST",
                path,
                raw(),
                {"x-api-key": token, "content-length": str(len(raw()))},
            )
            return conn.getresponse().status

        self.assertEqual(post("wrong"), 401)
        self.assertEqual(post(proxy.token, "/other"), 401)
        self.assertEqual(post(proxy.token), 200)


if __name__ == "__main__":
    unittest.main()
