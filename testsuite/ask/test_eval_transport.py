"""Loopback transport with DNS/TLS doubles, never a real upstream or credential."""

import contextlib
import json
import os
import pathlib
import socket
import socketserver
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "eval_support"))
from anthropic_gate import (
    MODELS,
    RESPONSE_CAP,
    GateError,
    dispatch_once,
    read_upstream_response,
)


# Optional owned evidence output is supplied only by credential-free test runners.
def record_observation(value):
    destination = os.environ.get("ASK_TEST_RECEIPTS")
    if destination:
        with pathlib.Path(destination).open("a") as receipt:
            receipt.write(json.dumps(value) + "\n")


@contextlib.contextmanager
def raw_upstream(wire):
    """Actual loopback bytes through production dispatch with fake DNS and TLS."""
    seen = []

    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            self.request.settimeout(2)
            headers = []
            while True:
                line = self.rfile.readline(65537)
                headers.append(line)
                if line in (b"\r\n", b"\n", b""):
                    break
            length = next(
                int(line.split(b":", 1)[1])
                for line in headers
                if line.lower().startswith(b"content-length:")
            )
            payload = self.rfile.read(length)
            seen.append({"payloadHex": payload.hex()})
            try:
                self.wfile.write(wire)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = socketserver.TCPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(
        target=server.serve_forever, kwargs={"poll_interval": 0.01}
    )
    thread.start()

    class TLSFixture:
        def wrap_socket(self, raw, server_hostname):
            if server_hostname != "api.anthropic.com":
                raise AssertionError(server_hostname)
            return raw

    address = [
        (
            socket.AF_INET,
            socket.SOCK_STREAM,
            socket.IPPROTO_TCP,
            "",
            server.server_address,
        )
    ]
    try:
        with (
            mock.patch("anthropic_gate.socket.getaddrinfo", return_value=address),
            mock.patch(
                "anthropic_gate.ssl.create_default_context", return_value=TLSFixture()
            ),
        ):
            yield seen
    finally:
        server.shutdown()
        thread.join(3)
        server.server_close()
        record_observation(
            {
                "wireHex": wire.hex(),
                "dispatches": seen,
                "threadStopped": not thread.is_alive(),
            }
        )
        if thread.is_alive():
            raise AssertionError("raw upstream thread still alive")


def length_wire(body=b"{}", declared=None, extra=b""):
    length = len(body) if declared is None else declared
    return (
        b"HTTP/1.1 200 OK\r\nContent-Length: "
        + str(length).encode()
        + b"\r\n"
        + extra
        + b"\r\n"
        + body
    )


def status_wire(status, body=b"", headers=None):
    if headers is None:
        headers = b"Content-Length: " + str(len(body)).encode() + b"\r\n"
    return f"HTTP/1.1 {status} Fixture\r\n".encode() + headers + b"\r\n" + body


def status_usage_body():
    return json.dumps(
        {
            "model": MODELS[0],
            "content": [{"type": "text", "text": "{}"}],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "service_tier": "standard",
                "inference_geo": "global",
            },
        }
    ).encode()


def header_prefix(
    total, status=b"HTTP/1.1 200 OK\r\n", headers=b"Content-Length: 2\r\n"
):
    """Count status, every raw header line and the terminating CRLF in total."""
    remaining = total - len(status) - len(headers) - 2
    lines = []
    while remaining:
        size = min(16000, remaining)
        if remaining - size in range(1, 5):
            size -= 5
        line = b"X: " + b"a" * (size - 5) + b"\r\n"
        lines.append(line)
        remaining -= len(line)
    prefix = status + headers + b"".join(lines) + b"\r\n"
    if len(prefix) != total:
        raise AssertionError(len(prefix))
    return prefix


class Transport(unittest.TestCase):
    def test_bodyless_status_refused_before_body_read(self):
        import io

        class CountedStream(io.BytesIO):
            def __init__(self, wire):
                super().__init__(wire)
                self.body_reads = []

            def read(self, length):
                self.body_reads.append(length)
                return super().read(length)

        body = status_usage_body()
        for status in (200, 204, 205):
            with self.subTest(status=status):
                stream = CountedStream(status_wire(status, body))
                error, returned = None, None
                try:
                    returned = read_upstream_response(stream)
                except GateError as failure:
                    error = failure.code
                record_observation(
                    {
                        "readerStatus": status,
                        "error": error,
                        "bodyReads": stream.body_reads,
                        "returnedHex": returned.hex() if returned is not None else None,
                    }
                )
                self.assertEqual(
                    stream.body_reads, [len(body)] if status == 200 else []
                )
                self.assertEqual(error, None if status == 200 else "upstream_framing")
                self.assertEqual(returned, body if status == 200 else None)

    def test_bodyless_status_transport_refusal(self):
        body = status_usage_body()
        length = b"Content-Length: " + str(len(body)).encode() + b"\r\n"
        for status in (200, 204, 205):
            variants = [("valid-usage-tail", body, length)]
            if status != 200:
                variants += [
                    ("no-length", body, b""),
                    ("zero-length", b"", b"Content-Length: 0\r\n"),
                    ("zero-with-tail", body, b"Content-Length: 0\r\n"),
                    ("malformed-length", body, b"Content-Length: -1\r\n"),
                    ("duplicate-length", body, length * 2),
                    ("cl-and-te", body, length + b"Transfer-Encoding: chunked\r\n"),
                ]
            for label, tail, headers in variants:
                with (
                    self.subTest(status=status, variant=label),
                    raw_upstream(status_wire(status, tail, headers)) as seen,
                ):
                    error, returned = None, None
                    try:
                        returned = dispatch_once(b"{}", "FAKE", 2)
                    except GateError as failure:
                        error = failure.code
                    record_observation(
                        {
                            "transportStatus": status,
                            "variant": label,
                            "error": error,
                            "returnedHex": returned.hex()
                            if returned is not None
                            else None,
                            "upstreamSends": len(seen),
                        }
                    )
                    self.assertEqual(len(seen), 1)
                    self.assertEqual(
                        error, None if status == 200 else "upstream_framing"
                    )
                    self.assertEqual(returned, body if status == 200 else None)

    def test_complete_declared_length_required(self):
        with (
            raw_upstream(length_wire(b"{}", declared=9)),
            self.assertRaisesRegex(GateError, "upstream_framing"),
        ):
            dispatch_once(b"{}", "FAKE", 2)

    def test_framing_refusals_and_exact_body_cap(self):
        for label, wire, expected in [
            ("exact", length_wire(), None),
            ("zero", length_wire(b""), None),
            ("body-cap", length_wire(b"x" * RESPONSE_CAP), None),
            ("over-cap", length_wire(declared=RESPONSE_CAP + 1), "upstream_cap"),
            ("negative", length_wire(declared=-1), "upstream_framing"),
            ("plus", length_wire(declared="+2"), "upstream_framing"),
            ("decimal", length_wire(declared="2.0"), "upstream_framing"),
            ("combined", length_wire(declared="2, 2"), "upstream_framing"),
            (
                "duplicate",
                length_wire(extra=b"Content-Length: 2\r\n"),
                "upstream_framing",
            ),
            (
                "conflicting",
                length_wire(extra=b"Content-Length: 3\r\n"),
                "upstream_framing",
            ),
            (
                "cl-and-te",
                length_wire(extra=b"Transfer-Encoding: chunked\r\n"),
                "upstream_framing",
            ),
            (
                "unknown-te",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n{}",
                "upstream_framing",
            ),
            ("close-delimited", b"HTTP/1.1 200 OK\r\n\r\n{}", "upstream_framing"),
            (
                "valid-chunked-unsupported",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n",
                "upstream_framing",
            ),
            (
                "missing-terminal-chunk",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n",
                "upstream_framing",
            ),
            (
                "incomplete-chunk",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\n{}",
                "upstream_framing",
            ),
            (
                "incomplete-trailer",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\nX: incomplete",
                "upstream_framing",
            ),
        ]:
            with self.subTest(case=label), raw_upstream(wire) as seen:
                if expected:
                    with self.assertRaisesRegex(GateError, "^" + expected + "$"):
                        dispatch_once(b"{}", "FAKE", 2)
                else:
                    self.assertEqual(
                        dispatch_once(b"{}", "FAKE", 2), wire.split(b"\r\n\r\n", 1)[1]
                    )
                self.assertEqual(len(seen), 1)

    def test_aggregate_status_header_budget(self):
        for total, expected in [(65536, None), (65537, "upstream_headers")]:
            with self.subTest(total=total), raw_upstream(header_prefix(total) + b"{}"):
                if expected:
                    with self.assertRaisesRegex(GateError, expected):
                        dispatch_once(b"{}", "FAKE", 2)
                else:
                    self.assertEqual(dispatch_once(b"{}", "FAKE", 2), b"{}")

    def test_aggregate_budget_includes_all_informational_blocks(self):
        interim = header_prefix(
            24000, status=b"HTTP/1.1 103 Early Hints\r\n", headers=b""
        )
        for total, expected in [(65536, None), (65537, "upstream_headers")]:
            wire = interim * 2 + header_prefix(total - 48000) + b"{}"
            with self.subTest(total=total), raw_upstream(wire):
                if expected:
                    with self.assertRaisesRegex(GateError, "^" + expected + "$"):
                        dispatch_once(b"{}", "FAKE", 2)
                else:
                    self.assertEqual(dispatch_once(b"{}", "FAKE", 2), b"{}")

    def test_header_overflow_reads_only_budget_plus_one(self):
        import io

        class CountedStream(io.BytesIO):
            def __init__(self, wire):
                super().__init__(wire)
                self.consumed = 0

            def readline(self, limit):
                self.asserted_limit = limit
                raw = super().readline(limit)
                self.consumed += len(raw)
                return raw

            def read(self, *args):
                raise AssertionError("oversized headers reached body reading")

        # Individually short lines cross the cumulative cap long before the
        # supplied six-megabyte header section could be accumulated.
        stream = CountedStream(
            b"HTTP/1.1 200 OK\r\n"
            + b"X: "
            + b"a" * 1000
            + b"\r\n"
            + (b"X: " + b"a" * 1000 + b"\r\n") * 6200
        )
        with self.assertRaisesRegex(GateError, "^upstream_headers$"):
            read_upstream_response(stream)
        self.assertEqual(stream.consumed, 65537)

    def test_one_fixed_route_no_redirect_and_bounded_response(self):
        for status, body, expected in [
            (302, b"fake-error", "upstream_status"),
            (500, b"fake-error", "upstream_status"),
            (200, b"x" * (1024 * 1024 + 1), "upstream_cap"),
            (200, b"{}", None),
        ]:
            calls, names = [], []

            class Handler(BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass

                def do_POST(self, status=status, body=body, calls=calls):
                    raw = self.rfile.read(int(self.headers["content-length"]))
                    calls.append(
                        {
                            "path": self.path,
                            "host": self.headers["host"],
                            "body": raw,
                            "headers": {h.lower() for h in self.headers},
                        }
                    )
                    self.send_response(status)
                    if status == 302:
                        self.send_header(
                            "location", "https://forbidden.invalid/elsewhere"
                        )
                    self.send_header("content-length", str(len(body)))
                    self.end_headers()
                    try:
                        self.wfile.write(body)
                    except (BrokenPipeError, ConnectionResetError):
                        pass

            server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()

            class TLSFixture:
                def wrap_socket(self, raw, server_hostname, names=names):
                    names.append(server_hostname)
                    return raw

            try:
                address = [
                    (
                        socket.AF_INET,
                        socket.SOCK_STREAM,
                        socket.IPPROTO_TCP,
                        "",
                        ("127.0.0.1", server.server_port),
                    )
                ]
                with (
                    mock.patch(
                        "anthropic_gate.socket.getaddrinfo", return_value=address
                    ) as dns,
                    mock.patch(
                        "anthropic_gate.ssl.create_default_context",
                        return_value=TLSFixture(),
                    ),
                ):
                    if expected:
                        with self.assertRaisesRegex(GateError, expected):
                            dispatch_once(b"{}", "FAKE_KEY", 2)
                    else:
                        self.assertEqual(dispatch_once(b"{}", "FAKE_KEY", 2), b"{}")
                    dns.assert_called_once_with(
                        "api.anthropic.com", 443, type=socket.SOCK_STREAM
                    )
                self.assertEqual(names, ["api.anthropic.com"])
                self.assertEqual(len(calls), 1)
                self.assertEqual(calls[0]["path"], "/v1/messages")
                self.assertEqual(calls[0]["host"], "api.anthropic.com")
                self.assertEqual(
                    calls[0]["headers"],
                    {
                        "host",
                        "content-length",
                        "content-type",
                        "accept-encoding",
                        "x-api-key",
                        "anthropic-version",
                    },
                )
            finally:
                server.shutdown()
                thread.join(2)
                server.server_close()


if __name__ == "__main__":
    unittest.main()
