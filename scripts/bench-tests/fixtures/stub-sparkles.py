#!/usr/bin/env python3
"""A stand-in for the sparkles binary in tests of the benchmark runners.

It answers `--version`, `load --loc DIR FILE` and `serve ... --loc NAME=DIR --port P`. The
server answers /$/ready and POST /NAME/sparql with one row, as TSV or as the native JSON
format with `meta.timing.execMs`, and appends one line per query request to requests.tsv
in its working directory: the binary's IDENTITY, the store it serves, the execution field
(or "-") and the Accept header. It speaks HTTP/1.1 with kept-alive connections and appends
the client's port and the raw form body of each query request to connections.tsv, so that
a test can count the connections a client opened and compare the bodies it sent. STUB_EXEC_MS in the file below, not the environment, sets
the reported execution time, since the runners clear the environment.
"""

import http.server
import json
import os
import sys
import time
import urllib.parse

IDENTITY = "stub"
EXEC_MS = 1.0
# with CLOSE_EVERY n > 0, every n-th query request closes its connection after the answer,
# without saying so, as a server does with an idle kept-alive connection
CLOSE_EVERY = 0


def main():
    args = sys.argv[1:]
    if args == ["--version"]:
        print(f"sparkles {IDENTITY}")
        return
    while args and args[0].startswith("--") and args[0] != "--version":
        args = args[2:]
    if args[0] == "load":
        loc = args[args.index("--loc") + 1]
        os.makedirs(loc, exist_ok=True)
        with open(os.path.join(loc, "owner"), "w") as f:
            f.write(IDENTITY)
        return
    if args[0] != "serve":
        sys.exit(f"stub: unknown command {args}")
    port = int(args[args.index("--port") + 1])
    name, _, store = args[args.index("--loc") + 1].partition("=")
    owner = open(os.path.join(store, "owner")).read()
    log = os.path.abspath("requests.tsv")

    conns = os.path.abspath("connections.tsv")

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        served = 0

        def log_message(self, *a):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
            # blank values are kept, so that an empty execution field differs from none
            form = urllib.parse.parse_qs(body, keep_blank_values=True)
            accept = self.headers.get("Accept", "")
            mode = form.get("execution", ["-"])[0]
            with open(log, "a") as f:
                f.write(f"{IDENTITY}\t{owner}\t{store}\t{mode}\t{accept}\n")
            with open(conns, "a") as f:
                f.write(f"{self.client_address[1]}\t{body}\n")
            if "#sleep" in form.get("query", [""])[0]:
                # a query with this comment takes two seconds, for tests of request timeouts,
                # and is logged when it arrives, so that the log does not depend on whether
                # the server is stopped before the two seconds pass
                time.sleep(2)
            ask = "ASK" in form.get("query", [""])[0].upper().split("{")[0]
            if ask and "tab-separated" in accept:
                out = b"?_askResult\ntrue\n"
            elif ask:
                out = json.dumps({"queryType": "ASK", "boolean": True,
                                  "meta": {"sentRows": 1, "totalRows": 1, "execMs": EXEC_MS}},
                                 separators=(",", ":")).encode()
            elif "tab-separated" in accept:
                out = b"?c\n1\n"
            else:
                out = json.dumps({"queryType": "SELECT", "vars": ["c"],
                                  "rows": [[{"type": "literal", "value": "1"}]],
                                  "meta": {"sentRows": 1, "totalRows": 1, "timing": {"execMs": EXEC_MS},
                                           "execMs": EXEC_MS}}, separators=(",", ":")).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(out)))
            self.end_headers()
            self.wfile.write(out)
            Handler.served += 1
            if CLOSE_EVERY and Handler.served % CLOSE_EVERY == 0:
                self.close_connection = True

    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
