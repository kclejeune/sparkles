#!/usr/bin/env bash
# Transport for explicit SSH hosts and admitted boat:bx_ID workers. No lifecycle calls.
RX_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
rx_exec() { python3 "$RX_DIR/remote-executor.py" exec "$@"; }
rx_sync() { python3 "$RX_DIR/remote-executor.py" sync "$@"; }
rx_admit() { python3 "$RX_DIR/remote-executor.py" admit "$@"; }
rx_bundle() { python3 "$RX_DIR/remote-executor.py" bundle "$@"; }
