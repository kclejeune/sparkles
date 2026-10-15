"""Loopback transport with DNS/TLS doubles, never a real upstream or credential."""

import pathlib
import socket
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "eval_support"))
from anthropic_gate import GateError, dispatch_once


class Transport(unittest.TestCase):
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
