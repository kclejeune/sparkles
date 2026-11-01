#!/usr/bin/env python3
"""Run the README's data, local-query and HTTP-query examples on a temporary dataset."""

import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parent.parent
readme = (root / 'README.md').read_text()
blocks = re.findall(r'```sh\n(.*?)\n```', readme, re.S)
local = next(block for block in blocks if 'cat > books.ttl' in block)
server = next(block for block in blocks if block.startswith('sparkles serve --data ./data'))
http = next(block for block in blocks if 'localhost:3030/books/sparql' in block)
env = os.environ.copy()
env['PATH'] = str(root / 'target/debug') + os.pathsep + env['PATH']

with tempfile.TemporaryDirectory(prefix='sparkles-quickstart-') as work:
    result = subprocess.run('set -eu\n' + local, shell=True, cwd=work, env=env, text=True,
                            capture_output=True, check=True, timeout=60)
    if not result.stdout.replace('\r', '').strip().endswith('n\n1'):
        raise SystemExit(f'Unexpected local query result: {result.stdout}')
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    process = subprocess.Popen(server.split() + ['--port', str(port)], cwd=work, env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.monotonic() + 30
        while True:
            result = subprocess.run(http.replace('localhost:3030', f'localhost:{port}'),
                                    shell=True, cwd=work, env=env, text=True,
                                    capture_output=True, timeout=10)
            if result.returncode == 0:
                break
            if process.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError(f'Server did not answer: {result.stderr}')
            time.sleep(0.1)
        if result.stdout.replace('\r', '').strip() != 'n\n1':
            raise RuntimeError(f'Unexpected HTTP query response: {result.stdout!r}')
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
print('README quick start returned one local and one HTTP result.')
