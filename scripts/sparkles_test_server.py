"""Discover the endpoint of a Sparkles subprocess from its startup log."""

import re
import subprocess
import time


def wait_for_listener(process, log, deadline):
    """Return the loopback URL after the child reports the bound address."""
    while True:
        output = log.read_text(errors="replace")
        status = process.poll()
        if status is not None:
            raise RuntimeError(f"sparkles serve exited with {status}; see {log}")
        listening = re.search(r" listening on (http://127\.0\.0\.1:[1-9][0-9]*)/ \(UI at /ui/\)", output)
        if listening:
            return listening.group(1)
        if time.monotonic() >= deadline:
            raise RuntimeError(f"sparkles serve did not report its listener; see {log}")
        time.sleep(0.05)


def stop_server(process, timeout):
    """Stop and reap the server, escalating if graceful shutdown times out."""
    process.terminate()
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
