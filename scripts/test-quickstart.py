#!/usr/bin/env python3
"""Run the README's data, local-query and HTTP-query examples on a temporary dataset."""

import os
from pathlib import Path
import re
import subprocess
import tempfile
import time

from sparkles_test_server import stop_server, wait_for_listener

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
    log = Path(work) / 'server.log'
    with log.open('w') as stderr:
        process = subprocess.Popen(server.split() + ['--host', '127.0.0.1', '--port', '0'],
                                   cwd=work, env={**env, 'RUST_LOG': 'info', 'NO_COLOR': '1'},
                                   stdout=subprocess.DEVNULL, stderr=stderr)
    try:
        deadline = time.monotonic() + 30
        base = wait_for_listener(process, log, deadline)
        command = http.replace('localhost:3030', base.removeprefix('http://'))
        query_env = {**env, 'no_proxy': '127.0.0.1', 'NO_PROXY': '127.0.0.1'}
        while process.poll() is None:
            result = subprocess.run(command,
                                    shell=True, cwd=work, env=query_env, text=True,
                                    capture_output=True, timeout=10)
            if result.returncode == 0 and process.poll() is None:
                break
            if process.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError(f'Server did not answer: {result.stderr}')
            time.sleep(0.1)
        else:
            raise RuntimeError(f'sparkles serve exited; see {log}')
        if result.stdout.replace('\r', '').strip() != 'n\n1':
            raise RuntimeError(f'Unexpected HTTP query response: {result.stdout!r}')
    finally:
        stop_server(process, timeout=10)
print('README quick start returned one local and one HTTP result.')
