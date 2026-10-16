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

    def ledger(self, cap=10**12, carryover=None, name="ledger.jsonl"):
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
            100 * 2000 + 10 * 2500 + 20 * 4000 + 50 * 100 + 10 * 10000,
        )
        usage = dict(usage, cache_creation={"ephemeral_5m_input_tokens": 1})
        self.assertIsNone(px.cost(SONNET, usage))
        big = {"input_tokens": 100001, "output_tokens": 1}
        self.assertEqual(px.cost(HAIKU, big), 100001 * 500 + 2500)


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
        path.write_text('{"ledger":2,"capNanodollars":1}\n')
        with self.assertRaises(Invalid):
            px.Ledger(path, 1)
        path.write_text('{"ledger":1,"capNanodollars":1}\n{"op":"settle","id":9}\n')
        with self.assertRaises(Invalid):
            px.Ledger(path, 1)
        self.assertFalse((self.root / "ledger.jsonl.lock").exists())

    def test_costs_by_case_and_model(self):
        proxy = self.proxy(Upstream())
        proxy.set_case("c1")
        proxy.handle(raw())
        proxy.handle(raw())
        proxy.set_case(None)
        proxy.handle(raw())
        self.assertEqual(proxy.ledger.case_costs(), {("c1", HAIKU): 2 * HAIKU_COST})


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
