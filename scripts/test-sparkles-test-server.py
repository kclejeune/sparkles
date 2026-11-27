#!/usr/bin/env python3
"""Run the subprocess harness regressions without building Sparkles."""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest

import sparkles_test_server


ROOT = Path(os.environ.get("SPARKLES_TEST_SOURCE", Path(__file__).resolve().parent.parent))
STAMP = "2026-01-01T00:00:00Z  INFO "


def record(port, target="sparkles"):
    return f"{STAMP}{target}: Sparkles test listening on http://127.0.0.1:{port}/ (UI at /ui/), opening 0 datasets\n"


def executable(path, body):
    path.write_text("#!/usr/bin/env bash\n" + body)
    path.chmod(0o755)
    return path


@contextmanager
def http_server(body=b"ok", *, status=200, before_reply=lambda: None, port=0):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            before_reply()
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        do_POST = do_GET

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", port), Handler) as http:
        thread = threading.Thread(target=http.serve_forever, daemon=True)
        thread.start()
        try:
            yield http.server_port, requests
        finally:
            http.shutdown()
            thread.join()


def proxy_environment(port):
    address = f"http://127.0.0.1:{port}"
    return {**os.environ, "http_proxy": address, "HTTP_PROXY": address,
            "no_proxy": "localhost", "NO_PROXY": "localhost"}


class HarnessTests(unittest.TestCase):
    def test_crash_driver_never_contacts_an_occupied_port(self):
        with tempfile.TemporaryDirectory() as tmp, http_server() as (port, requests):
            root = Path(tmp)
            server = executable(root / "sparkles", '''
if [[ $1 == load ]]; then exit 0; fi
printf 'binding 127.0.0.1:%s failed\\n' "$PORT" >&2
sleep 0.5
exit 1
''')
            marker = root / "called"
            driver = executable(root / "drive", 'touch "$DRIVER_MARKER"\n')
            result = subprocess.run(
                ["bash", ROOT / "scripts/power-loss.sh", "selftest", "--fs", root,
                 "--sparkles", server, "--harness", driver, "--work", root / "work"],
                env={**os.environ, "PORT": str(port), "DRIVER_MARKER": str(marker),
                     "no_proxy": "*", "NO_PROXY": "*"},
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertEqual(requests, [])
            self.assertFalse(marker.exists(), result.stderr)

    def test_quickstart_bypasses_an_inherited_proxy(self):
        with tempfile.TemporaryDirectory() as tmp, http_server(b"wrong\n") as (proxy, requests):
            root = Path(tmp)
            for subdir in ("scripts", "target/debug"):
                (root / subdir).mkdir(parents=True)
            for name in ("README.md", "scripts/test-quickstart.py", "scripts/sparkles_test_server.py"):
                shutil.copyfile(ROOT / name, root / name)
            binary = root / "target/debug/sparkles"
            binary.write_text(f"#!{sys.executable}\n" + """
from http.server import BaseHTTPRequestHandler, HTTPServer
import sys

if sys.argv[1] != 'serve':
    print('n\\n1')
    raise SystemExit(0)

class Server(BaseHTTPRequestHandler):
    def do_POST(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'n\\n1\\n')
    def log_message(self, *_):
        pass

with HTTPServer(('127.0.0.1', 0), Server) as server:
    print(f'2026-01-01T00:00:00Z  INFO sparkles: Sparkles test listening on http://127.0.0.1:{server.server_port}/ (UI at /ui/), opening 1 datasets', file=sys.stderr, flush=True)
    server.serve_forever()
""")
            binary.chmod(0o755)
            result = subprocess.run(
                [sys.executable, root / "scripts/test-quickstart.py"],
                env=proxy_environment(proxy), capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(requests, [])
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_jena_waits_for_the_child_and_bypasses_the_proxy(self):
        with (tempfile.TemporaryDirectory() as tmp, http_server() as (port, requests),
              http_server() as (proxy, proxy_requests)):
            root = Path(tmp)
            server = executable(root / "sparkles", '''
sleep 0.2
printf '%s' "$FAKE_DIAGNOSTIC" "$FAKE_BIND" >&2
exec sleep 30
''')
            java = executable(root / "java", '[[ ${!#} == "http://127.0.0.1:$EXPECTED_PORT" ]]\n')
            result = subprocess.run(
                ["bash", ROOT / "scripts/test-jena-clients.sh"],
                env={**proxy_environment(proxy), "SPARKLES_BIN": str(server), "JAVA": str(java),
                     "JENA_HOME": str(root), "TMPDIR": str(root), "EXPECTED_PORT": str(port),
                     "FAKE_DIAGNOSTIC": record(proxy, target="wrapper"), "FAKE_BIND": record(port)},
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(requests, ["/$/ready"])
            self.assertEqual(proxy_requests, [])

    def test_ui_seeds_never_contact_an_occupied_port(self):
        with tempfile.TemporaryDirectory() as tmp, http_server() as (port, requests):
            root = Path(tmp)
            server = executable(root / "sparkles", '''
if [[ $1 == load ]]; then exit 0; fi
sleep 0.5
exit 1
''')
            executable(root / "which", "exit 1\n")  # Exercise the unscoped seed launch.
            env = {
                **os.environ, "PATH": f"{root}{os.pathsep}{os.environ['PATH']}",
                "SPARKLES_BIN": str(server), "SPARKLES_SEED_PORT": str(port),
                "SPARKLES_SEED_SKIP": "go,unesco", "SPARKLES_SEED_ASK": "0",
                "SPARKLES_PERF_DIR": str(root / "perf"), "SPARKLES_PERF_DATASETS": "",
            }
            for script in ("seed-design.mjs", "perf-seed.mjs"):
                with self.subTest(script=script):
                    result = subprocess.run(
                        ["node", ROOT / "ui/scripts" / script], env=env,
                        capture_output=True, text=True, timeout=10,
                    )
                    self.assertEqual(requests, [])
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn("ready http", result.stdout)


class ListenerTests(unittest.TestCase):
    def test_parser_requires_a_complete_loopback_startup_event(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "server.log"
            log.write_text(
                record(4000, target="wrapper") + record(65536) + record(4001) + record(4002).rstrip()
            )
            with subprocess.Popen(["sleep", "30"]) as child:
                try:
                    url = sparkles_test_server.wait_for_listener(child, log, time.monotonic() + 1)
                    self.assertEqual(url, "http://127.0.0.1:4001")
                    log.write_text(record(4002).rstrip())
                    with self.assertRaisesRegex(RuntimeError, "did not report"):
                        sparkles_test_server.wait_for_listener(child, log, time.monotonic())
                finally:
                    child.terminate()

    def test_direct_readiness_ignores_proxy_and_has_a_deadline(self):
        with (subprocess.Popen(["sleep", "30"]) as child,
              http_server(status=503) as (port, requests),
              http_server() as (proxy, proxy_requests)):
            try:
                from unittest.mock import patch
                with patch.dict(os.environ, proxy_environment(proxy)):
                    with self.assertRaisesRegex(RuntimeError, "did not become ready"):
                        sparkles_test_server.wait_for_ready(
                            child, f"http://127.0.0.1:{port}", "server.log", time.monotonic() + 0.3,
                        )
                self.assertTrue(requests)
                self.assertEqual(proxy_requests, [])
            finally:
                child.terminate()

    def test_replacement_readiness_response_after_child_exit_is_rejected(self):
        program = """
import socket, sys
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    listener.listen()
    print(listener.getsockname()[1], flush=True)
    sys.stdin.readline()
print('released', flush=True)
sys.stdin.readline()
"""
        with subprocess.Popen([sys.executable, "-c", program], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, text=True) as child:
            try:
                port = int(child.stdout.readline())
                child.stdin.write("release\n")
                child.stdin.flush()
                self.assertEqual(child.stdout.readline().strip(), "released")

                def exit_child():
                    child.terminate()
                    child.wait(timeout=2)

                with http_server(before_reply=exit_child, port=port) as (_, requests):
                    with self.assertRaisesRegex(RuntimeError, "sparkles serve exited"):
                        sparkles_test_server.wait_for_ready(
                            child, f"http://127.0.0.1:{port}", "server.log", time.monotonic() + 2,
                        )
                    self.assertEqual(requests, ["/$/ready"])
            finally:
                if child.poll() is None:
                    child.terminate()


if __name__ == "__main__":
    unittest.main()
