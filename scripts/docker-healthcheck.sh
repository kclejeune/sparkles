#!/usr/bin/env bash
# The container image's health check (the Dockerfile's HEALTHCHECK and compose.yaml): asks
# the server in the container for GET /$/ready and succeeds on 200. /$/ready answers 503
# while the server starts and once it shuts down, and it needs no credentials with
# --auth-config. The runtime image has neither curl nor wget, so this speaks HTTP/1.1 over
# bash's /dev/tcp. SPARKLES_HEALTHCHECK_PORT names the port when `serve` listens on
# another one than 3030. A server with --tls-cert needs another probe.
set -euo pipefail

port=${SPARKLES_HEALTHCHECK_PORT:-3030}
exec 3<> "/dev/tcp/127.0.0.1/$port"
printf 'GET /$/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n' >&3
read -r _ status _ <&3
[ "$status" = 200 ]
