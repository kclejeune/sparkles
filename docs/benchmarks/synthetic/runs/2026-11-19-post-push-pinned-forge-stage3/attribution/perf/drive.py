import sys, http.client, urllib.parse
port, qfile, n = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
q = open(qfile).read()
body = urllib.parse.urlencode({"query": q, "execution": "streaming"})
for _ in range(n):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
    c.request("POST", "/bench/sparql", body, {"Accept": "application/x-sparkles+json", "Content-Type": "application/x-www-form-urlencoded"})
    r = c.getresponse(); r.read(); c.close()
    if r.status != 200: sys.exit(f"status {r.status}")
