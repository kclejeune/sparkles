"""Discover the endpoint of a Sparkles subprocess from its startup log."""

import re
import subprocess
import time
import urllib.request


DIRECT_HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))
LISTENING = re.compile(
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z +INFO sparkles: Sparkles \S+"
    r" listening on (http://127\.0\.0\.1:([1-9][0-9]{0,4}))/ \(UI at /ui/\),"
    r" opening [0-9]+ datasets\r?\n",
    re.MULTILINE,
)


def wait_for_listener(process, log, deadline):
    """Return the loopback URL after the child reports the bound address."""
    while True:
        output = log.read_text(errors="replace")
        status = process.poll()
        if status is not None:
            raise RuntimeError(f"sparkles serve exited with {status}; see {log}")
        for listening in LISTENING.finditer(output):
            if int(listening[2]) <= 65535:
                return listening[1]
        if time.monotonic() >= deadline:
            raise RuntimeError(f"sparkles serve did not report its listener; see {log}")
        time.sleep(0.05)


def wait_for_ready(process, base, log, deadline):
    """Poll the local child until it answers readiness or reaches the deadline."""
    while process.poll() is None:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(f"sparkles serve did not become ready; see {log}")
        try:
            with DIRECT_HTTP.open(f"{base}/$/ready", timeout=min(remaining, 1)) as response:
                if response.status == 200 and process.poll() is None:
                    return
        except OSError:
            pass
        time.sleep(min(remaining, 0.2))
    raise RuntimeError(f"sparkles serve exited; see {log}")


def stop_server(process, timeout):
    """Stop and reap the server, escalating if graceful shutdown times out."""
    process.terminate()
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
