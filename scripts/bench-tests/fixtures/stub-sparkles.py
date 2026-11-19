#!/usr/bin/env python3
"""A stand-in for the sparkles binary in tests of the benchmark runners.

It answers `--version`, `load --loc DIR FILE` and `serve ... --loc NAME=DIR --port P`. The
server answers /$/ready and POST /NAME/sparql with one row, as TSV or as the native JSON
format with `meta.timing.execMs`, and appends one line per query request to requests.tsv
in its working directory: the binary's IDENTITY, the store it serves, the execution field
(or "-") and the Accept header. STUB_EXEC_MS in the file below, not the environment, sets
the reported execution time, since the runners clear the environment.
"""

import http.server
import json
import os
import sys
import urllib.parse

IDENTITY = "stub"
EXEC_MS = 1.0


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

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            self.send_response(200)
            self.end_headers()

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
            form = urllib.parse.parse_qs(body)
            accept = self.headers.get("Accept", "")
            mode = form.get("execution", ["-"])[0]
            with open(log, "a") as f:
                f.write(f"{IDENTITY}\t{owner}\t{store}\t{mode}\t{accept}\n")
            if "tab-separated" in accept:
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

    http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
