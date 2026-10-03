#!/usr/bin/env bash
# The container image's health check (the Dockerfile's HEALTHCHECK and compose.yaml): asks
# the server in the container for GET /$/ready with `sparkles ping` and succeeds on 200.
# /$/ready answers 503 while the server starts and once it shuts down, and it needs no
# credentials with --auth-config. `sparkles ping` tries plain HTTP and then HTTPS, so the
# check works with --tls-cert too. It accepts any certificate on the loopback address,
# which the server's certificate does not name. SPARKLES_HEALTHCHECK_PORT names the port
# when `serve` listens on another one than 3030, and SPARKLES_HEALTHCHECK_URL replaces the
# whole target (a URL, or HOST:PORT).
set -euo pipefail

port=${SPARKLES_HEALTHCHECK_PORT:-3030}
exec sparkles ping --quiet --timeout 4 "${SPARKLES_HEALTHCHECK_URL:-127.0.0.1:$port}"
