#!/usr/bin/env python3
"""Native-host X09 section 17.9 gates. Run --help before allocating the quiet host.

Both alternatives run one externally built binary, with ordinary commits selected by
--no-group-commit. This script builds nothing and changes no engine configuration outside
its newly created directory. The caller holds the host's heavy lock for the whole run.
"""

import argparse
import concurrent.futures
import fcntl
import hashlib
import http.client
import http.server
import json
import math
import os
import pathlib
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time
import uuid

FOAF_NAME = "http://xmlns.com/foaf/0.1/name"
GEO = "http://www.opengis.net/ont/geosparql#"
XSD = "http://www.w3.org/2001/XMLSchema#"
SENTINELS = 17
TEXT_QUERY = (
    "PREFIX text:<http://jena.apache.org/text#> "
    'SELECT (COUNT(*) AS ?n) WHERE { ?s text:query ("fixedsentinel" 100) }'
)
GEO_QUERY = (
    "PREFIX geo:<http://www.opengis.net/ont/geosparql#> "
    "PREFIX geof:<http://www.opengis.net/def/function/geosparql/> "
    "SELECT (COUNT(*) AS ?n) WHERE { ?s geo:asWKT ?w . "
    'FILTER(geof:sfWithin(?w, "POLYGON((-1 -1,1 -1,1 1,-1 1,-1 -1))"^^geo:wktLiteral)) }'
)


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as src:
        for block in iter(lambda: src.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def save(path, value):
    pathlib.Path(path).write_text(json.dumps(value, indent=2) + "\n")


def vector(text):
    data = hashlib.sha256(text.encode()).digest()
    values = [(v - 127.5) / 127.5 for v in data[:8]]
    norm = math.sqrt(sum(v * v for v in values))
    return [v / norm for v in values]


def serve_provider(port):
    state = {"requests": 0, "inputs": 0}
    lock = threading.Lock()

    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, code, body):
            data = json.dumps(body).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            with lock:
                self.reply(200, dict(state))

        def do_POST(self):
            try:
                data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                inputs = data["input"]
                if isinstance(inputs, str):
                    inputs = [inputs]
                if not isinstance(inputs, list) or not all(
                    isinstance(s, str) for s in inputs
                ):
                    raise ValueError("string inputs required")
                result = [
                    {"index": i, "object": "embedding", "embedding": vector(s)}
                    for i, s in enumerate(inputs)
                ]
                with lock:
                    state["requests"] += 1
                    state["inputs"] += len(inputs)
                self.reply(
                    200,
                    {
                        "object": "list",
                        "model": "item86-deterministic",
                        "data": result,
                        "usage": {"prompt_tokens": 0, "total_tokens": 0},
                    },
                )
            except (ValueError, KeyError, TypeError):
                self.reply(400, {"error": "invalid embedding input"})

    http.server.ThreadingHTTPServer(("127.0.0.1", port), Provider).serve_forever()


def count_result(body):
    value = json.loads(body)
    assert value["head"]["vars"] == ["n"], "unexpected count variable"
    rows = value["results"]["bindings"]
    assert len(rows) == 1 and set(rows[0]) == {"n"}, "expected exactly one count"
    n = rows[0]["n"]
    assert n["type"] == "literal" and n.get("datatype") == XSD + "integer", (
        "integer count required"
    )
    result = int(n["value"])
    assert result >= 0, "nonnegative count required"
    return result


def query(port, text, conn=None):
    owned = conn is None
    conn = conn or http.client.HTTPConnection("127.0.0.1", port, timeout=60)
    try:
        conn.request(
            "POST",
            "/bench/sparql",
            text.encode(),
            {
                "Content-Type": "application/sparql-query",
                "Accept": "application/sparql-results+json",
            },
        )
        response = conn.getresponse()
        body = response.read()
        assert response.status == 200, (response.status, body[:200])
        return count_result(body)
    finally:
        if owned:
            conn.close()


def raw_request(port, path, body=None):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
    try:
        conn.request(
            "GET" if body is None else "POST",
            path,
            None if body is None else json.dumps(body).encode(),
            {"Content-Type": "application/json", "Accept": "application/json"},
        )
        response = conn.getresponse()
        result = response.read()
        assert response.status == 200, (path, response.status, result[:200])
        return result
    finally:
        conn.close()


def metrics(port, directory):
    text = raw_request(port, "/$/metrics").decode()
    (directory / "metrics.txt").write_text(text)
    result = {}
    for line in text.splitlines():
        if line.startswith("sparkles_"):
            key, value = line.split()
            if 'dataset="bench"' in key:
                family = key.split("{", 1)[0]
                result[family] = result.get(family, 0) + float(value)
    return result


def get_json(port, path):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        conn.request("GET", path)
        r = conn.getresponse()
        body = r.read()
        assert r.status == 200, (path, r.status, body[:200])
        return json.loads(body)
    finally:
        conn.close()


def percentile(values, fraction):
    assert values, "no latency samples"
    values = sorted(values)
    return values[math.floor((len(values) - 1) * fraction + 0.5)]


def summarize(results):
    gates = []
    keys = sorted({(r["people"], r["index"], r["scenario"]) for r in results})
    for people, index, scenario in keys:
        rows = [
            r
            for r in results
            if (r["people"], r["index"], r["scenario"]) == (people, index, scenario)
        ]
        variants = [
            [r for r in rows if r["grouped"] == grouped] for grouped in (False, True)
        ]
        assert all(variants), "both alternatives required"
        checks = {}
        if scenario.startswith("serial-"):
            for metric in ("p50_ms", "p90_ms"):
                values = [
                    statistics.median(r["writers"][metric] for r in variant)
                    for variant in variants
                ]
                ratio = values[1] / values[0]
                checks[metric] = {"ratio": ratio, "passed": 0.95 <= ratio <= 1.05}
        else:
            rss = [max(r["peak_rss_kib"] for r in variant) for variant in variants]
            checks["peak_rss"] = {
                "delta_kib": rss[1] - rss[0],
                "passed": rss[1] - rss[0] <= 65536,
            }
            if scenario == "writers":
                tps = [
                    statistics.median(r["tps"] for r in variant) for variant in variants
                ]
                checks["throughput"] = {
                    "ratio": tps[1] / tps[0],
                    "passed": tps[1] / tps[0] >= 1.5,
                }
            else:
                for metric in ("p50_ms", "p99_ms"):
                    values = [
                        statistics.median(r["readers"][metric] for r in variant)
                        for variant in variants
                    ]
                    checks[metric] = {
                        "ratio": values[1] / values[0],
                        "passed": values[1] / values[0] <= 1.10,
                    }
        gates.append(
            {
                "people": people,
                "index": index,
                "scenario": scenario,
                "processes_per_variant": len(variants[0]),
                "checks": checks,
                "passed": all(c["passed"] for c in checks.values()),
            }
        )
    return {"gates": gates, "passed": bool(gates) and all(g["passed"] for g in gates)}


def read_samples(port, text, expected, stop, samples, errors):
    due = time.monotonic()
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
    try:
        while not stop.is_set():
            if stop.wait(max(0, due - time.monotonic())):
                break
            due += 1 / 25
            start = time.monotonic()
            try:
                actual = query(port, text, conn)
                assert actual == expected, (actual, expected)
                end = time.monotonic()
                samples.append(
                    {
                        "start": start,
                        "end": end,
                        "ms": (end - start) * 1000,
                        "count": actual,
                    }
                )
            except (
                OSError,
                AssertionError,
                ValueError,
                KeyError,
                http.client.HTTPException,
            ) as exc:
                errors.append(str(exc))
                stop.set()
    finally:
        conn.close()


def memory_samples(pid, stop, samples):
    while not stop.is_set():
        text = pathlib.Path(f"/proc/{pid}/status").read_text()
        rss = int(
            next(s for s in text.splitlines() if s.startswith("VmRSS:")).split()[1]
        )
        samples.append({"time": time.monotonic(), "rss_kib": rss})
        stop.wait(0.1)


def reset_hwm(pid, directory):
    before = pathlib.Path(f"/proc/{pid}/status").read_text()
    pathlib.Path(f"/proc/{pid}/clear_refs").write_text("5\n")
    after = pathlib.Path(f"/proc/{pid}/status").read_text()
    (directory / "hwm-before-reset.txt").write_text(before)
    (directory / "hwm-after-reset.txt").write_text(after)


def probe_hwm(directory):
    # Prove reset support on an owned disposable child, including a peak above its
    # current RSS. A server whose startup peak equals RSS would not prove this.
    source = (
        'import sys; x=bytearray(b"x"*(32*1024*1024)); del x; '
        'print("ready",flush=True); sys.stdin.readline()'
    )
    child = subprocess.Popen(
        [sys.executable, "-c", source],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        assert child.stdout.readline().strip() == "ready"
        reset_hwm(child.pid, directory)
        before = (directory / "hwm-before-reset.txt").read_text()
        after = (directory / "hwm-after-reset.txt").read_text()
        old_peak = int(
            next(s for s in before.splitlines() if s.startswith("VmHWM:")).split()[1]
        )
        peak = int(
            next(s for s in after.splitlines() if s.startswith("VmHWM:")).split()[1]
        )
        current = int(
            next(s for s in after.splitlines() if s.startswith("VmRSS:")).split()[1]
        )
        assert old_peak - peak > 16384 and peak >= current, (
            "kernel did not reset peak RSS"
        )
        save(
            directory / "hwm-reset-probe.json",
            {
                "before_kib": old_peak,
                "after_kib": peak,
                "rss_kib": current,
                "passed": True,
            },
        )
    finally:
        child.stdin.close()
        child.wait(timeout=10)
        child.stdout.close()


def record_environment(directory):
    for name, command in (
        ("cpu.json", ["lscpu", "-J"]),
        ("cpu-topology.txt", ["lscpu", "-e=CPU,CORE,SOCKET,ONLINE"]),
        ("filesystem.json", ["findmnt", "-J", "-T", str(directory)]),
        (
            "devices.json",
            ["lsblk", "-J", "-o", "NAME,TYPE,FSTYPE,ROTA,MODEL,MOUNTPOINTS"],
        ),
    ):
        (directory / name).write_bytes(subprocess.check_output(command))
    (directory / "processes.txt").write_bytes(
        subprocess.check_output(["ps", "-eo", "pid,ppid,comm,pcpu,pmem"])
    )
    (directory / "meminfo.txt").write_text(pathlib.Path("/proc/meminfo").read_text())
    idle_before = pathlib.Path("/proc/diskstats").read_text()
    time.sleep(2)
    save(
        directory / "idle-diskstats.json",
        {
            "seconds": 2,
            "before": idle_before,
            "after": pathlib.Path("/proc/diskstats").read_text(),
        },
    )


def geometry_line(person):
    # All mutable/base geometries are outside the immutable sentinel window.
    return (
        f"<http://example.org/person/{person}> <{GEO}asWKT> "
        f'"POINT({100 + person % 50} {40 + person % 30})"^^<{GEO}wktLiteral> .\n'
    )


def generate(root, people, index):
    base = root / "base.nt"
    with base.open("wb") as out:
        subprocess.run(
            [
                sys.executable,
                str(pathlib.Path(__file__).with_name("gen-data.py")),
                str(people),
            ],
            stdout=out,
            check=True,
        )
    data = root / "data.nt"
    with base.open() as src, data.open("w") as out:
        for line in src:
            if index == "geo" and f"<{FOAF_NAME.removesuffix('name')}age>" in line:
                person = int(line.split("/person/", 1)[1].split(">", 1)[0])
                line = geometry_line(person)
            out.write(line)
        for i in range(SENTINELS):
            if index == "geo":
                out.write(
                    f'<urn:item86:sentinel:{i}> <{GEO}asWKT> "POINT(0 0)"^^<{GEO}wktLiteral> .\n'
                )
            else:
                out.write(
                    f'<urn:item86:sentinel:{i}> <{FOAF_NAME}> "fixedsentinel {i}" .\n'
                )
    with data.open() as rows:
        return sum(1 for _ in rows)


class Server:
    def __init__(self, args, store, directory, grouped):
        self.args, self.store, self.directory = args, store, directory
        self.unit = "sparkles-item86-" + uuid.uuid4().hex
        self.log = (directory / "server.log").open("wb")
        command = [
            "systemd-run",
            "--user",
            "--quiet",
            "--wait",
            "--pipe",
            "--service-type=exec",
            f"--unit={self.unit}",
            "-p",
            "MemoryMax=12G",
            "-p",
            "MemorySwapMax=0",
            "taskset",
            "-c",
            args.server_cpus,
            args.binary,
            "--result-cache-mb",
            "0",
        ]
        if not grouped:
            command.append("--no-group-commit")
        command += [
            "serve",
            "--host",
            "127.0.0.1",
            "--data",
            str(directory / "srv"),
            "--loc",
            f"bench={store}",
            "--port",
            str(args.port),
            "--no-access-log",
            "--outbound-allow",
            "127.0.0.1/32",
            "--timeout",
            "600",
        ]
        self.process = subprocess.Popen(
            command, stdout=self.log, stderr=subprocess.STDOUT
        )
        self.pid = None
        try:
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise RuntimeError(
                        "server exited; see " + str(directory / "server.log")
                    )
                try:
                    get_json(args.port, "/$/stats/bench")
                    break
                except (OSError, AssertionError, ValueError):
                    time.sleep(0.1)
            else:
                raise TimeoutError("server startup")
            properties = subprocess.check_output(
                [
                    "systemctl",
                    "--user",
                    "show",
                    self.unit,
                    "-p",
                    "MainPID",
                    "-p",
                    "MemoryMax",
                    "-p",
                    "MemorySwapMax",
                ],
                text=True,
            )
            (directory / "scope.txt").write_text(properties)
            props = dict(s.split("=", 1) for s in properties.splitlines())
            assert (
                int(props["MemoryMax"]) == 12 * 1024**3
                and props["MemorySwapMax"] == "0"
            )
            self.pid = int(props["MainPID"])
            assert self.pid > 0
        except BaseException:
            self.close()
            raise

    def close(self):
        subprocess.run(
            ["systemctl", "--user", "stop", self.unit],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=10)
        self.log.close()


def caught_up(args):
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        status = get_json(args.port, "/$/vector/bench/docs")
        e = status["embedding"]
        if e["state"] == "idle" and e["appliedSeq"] == e["headSeq"]:
            return status
        if e["state"] in ("failed", "refused", "auth"):
            raise RuntimeError(str(status))
        time.sleep(0.2)
    raise TimeoutError("embedding worker catch-up")


def run_command(command, path):
    with path.open("wb") as out:
        subprocess.run(command, stdout=out, stderr=subprocess.STDOUT, check=True)


def prepare(args, root, index, people):
    seed = root / "seed"
    if seed.exists():
        raise FileExistsError(seed)
    triples = generate(root, people, index)
    run_command(
        [args.binary, "load", "--loc", str(seed), str(root / "data.nt")],
        root / "load.log",
    )
    if index in ("text", "geo"):
        command = [args.binary, index + "-index", "--loc", str(seed)]
    else:
        command = [
            args.binary,
            "vector",
            "create",
            "--loc",
            str(seed),
            "--name",
            "docs",
            "--predicate",
            "urn:item86:embedding",
            "--dim",
            "8",
            "--embed-model",
            "item86-deterministic",
            "--embed-from",
            FOAF_NAME,
            "--embed-url",
            f"http://127.0.0.1:{args.provider_port}/v1/embeddings",
        ]
    run_command(command, root / "index.log")
    bootstrap = root / "bootstrap"
    bootstrap.mkdir()
    server = Server(args, seed, bootstrap, True)
    try:
        assert query(args.port, "SELECT (COUNT(*) AS ?n) WHERE {?s ?p ?o}") == triples
        if index == "embed":
            save(bootstrap / "embedding.json", caught_up(args))
        else:
            text = TEXT_QUERY if index == "text" else GEO_QUERY
            assert query(args.port, text) == SENTINELS
            (root / "reader.rq").write_text(text + "\n")
            plan = json.loads(
                raw_request(
                    args.port,
                    "/bench/sparql/explain",
                    {"query": text, "profile": "run", "describe": False},
                )
            )
            save(root / "reader-plan.json", plan)
            label = "Text" if index == "text" else "SpatialScan"
            assert label in json.dumps(plan), "indexed plan operator missing: " + label
    finally:
        server.close()
    save(
        root / "data.json",
        {
            "people": people,
            "index": index,
            "triples": triples,
            "sha256": digest(root / "data.nt"),
        },
    )
    return triples


def measure(args, cell, index, scenario, grouped, triples, run_id, write_rate):
    directory = cell / run_id
    directory.mkdir()
    store = directory / "store"
    shutil.copytree(cell / "seed", store)
    server = Server(args, store, directory, grouped)
    stop = threading.Event()
    samples, errors, futures, memory = [], [], [], []
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=3)
    namespace = "item86_" + uuid.uuid4().hex
    operation = (
        "geo"
        if index == "geo" and scenario != "serial-iri" and scenario != "serial-delete"
        else scenario.removeprefix("serial-")
    )
    if operation in ("writers", "mixed"):
        operation = "literal"
    changes = (
        args.serial_changes if scenario.startswith("serial-") else args.writer_changes
    )
    command = [
        "taskset",
        "-c",
        args.client_cpus,
        args.write_bench,
        "--addr",
        f"127.0.0.1:{args.port}",
        "--ds",
        "bench",
        "--run",
        namespace,
        "--people",
        str(args.current_people),
        "--changes",
        str(changes),
        "--op",
        operation,
        "--literal-predicate",
        FOAF_NAME,
        "--samples",
        str(directory / "writer-samples.tsv"),
    ]
    command += (
        ["serial", "--warmup", str(args.warmup)]
        if scenario.startswith("serial-")
        else ["writers", "--clients", "4"]
    )
    if scenario == "mixed":
        command += ["--write-rate", str(write_rate)]
    try:
        before_embedding = caught_up(args) if index == "embed" else None
        assert get_json(args.port, "/$/stats/bench")["groupCommit"] == grouped
        provider_before = get_json(args.provider_port, "/")
        reset_hwm(server.pid, directory)
        if scenario == "mixed":
            text = TEXT_QUERY if index == "text" else GEO_QUERY
            for _ in range(2):
                futures.append(
                    pool.submit(
                        read_samples, args.port, text, SENTINELS, stop, samples, errors
                    )
                )
        futures.append(pool.submit(memory_samples, server.pid, stop, memory))
        (directory / "disk-before.txt").write_text(
            pathlib.Path("/proc/diskstats").read_text()
        )
        start = time.monotonic()
        with (directory / "client.stderr").open("wb") as err:
            client = subprocess.run(
                command, stdout=subprocess.PIPE, stderr=err, check=True
            )
        end = time.monotonic()
        interval_status = pathlib.Path(f"/proc/{server.pid}/status").read_text()
        provider_after = get_json(args.provider_port, "/")
        stop.set()
        for future in futures:
            future.result()
        pool.shutdown(wait=True)
        assert not errors, errors
        result = json.loads(client.stdout)
        assert (
            result["changes"] == changes
            and result["errors"] == 0
            and result["missing_receipts"] == 0
        )
        assert result["receipts_contiguous"] and result["receipts_distinct"]
        overlap = [s for s in samples if s["start"] >= start and s["end"] <= end]
        save(directory / "reader-samples.json", samples)
        save(directory / "memory-samples.json", memory)
        if scenario == "mixed":
            assert len(overlap) >= 100, (
                "fewer than 100 reader observations during writes"
            )
            result["readers"] = {
                "n": len(overlap),
                "p50_ms": percentile([s["ms"] for s in overlap], 0.5),
                "p99_ms": percentile([s["ms"] for s in overlap], 0.99),
            }
        (directory / "disk-after.txt").write_text(
            pathlib.Path("/proc/diskstats").read_text()
        )
        status = interval_status
        (directory / "proc-status.txt").write_text(status)
        result["peak_rss_kib"] = int(
            next(s for s in status.splitlines() if s.startswith("VmHWM:")).split()[1]
        )
        counters = metrics(args.port, directory)
        assert counters.get("sparkles_group_commit_failures_total", 0) == 0
        assert counters.get("sparkles_group_commit_fallbacks_total", 0) == 0
        admitted = counters.get("sparkles_group_commit_size_sum", 0)
        if not grouped:
            assert admitted == 0, "ordinary run admitted grouped commits"
        elif not scenario.startswith("serial-"):
            assert admitted > 0, "four-writer run admitted no grouped commits"
        result["grouped_admissions"] = admitted
        # The exact generated subject namespace gives an independent data oracle.
        expected = changes + (args.warmup if scenario.startswith("serial-") else 0)
        if scenario == "serial-delete":
            expected = 0
        check = (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o . "
            f'FILTER(STRSTARTS(STR(?s), "urn:wb:{namespace}:")) }}'
        )
        if operation in ("iri", "delete"):
            check = (
                f"SELECT (COUNT(*) AS ?n) WHERE {{ ?s <urn:wb:link:{namespace}> ?o }}"
            )
        assert query(args.port, check) == expected
        assert (
            query(args.port, "SELECT (COUNT(*) AS ?n) WHERE {?s ?p ?o}")
            == triples + expected
        )
        result["write_namespace_count"] = expected
        index_check = None
        if index == "text" and operation == "literal":
            index_check = (
                "PREFIX text:<http://jena.apache.org/text#> "
                f'SELECT (COUNT(*) AS ?n) WHERE {{ ?s text:query ("value" {expected + 1}) }}'
            )
        elif index == "geo" and operation == "geo":
            index_check = (
                "PREFIX geo:<http://www.opengis.net/ont/geosparql#> "
                "PREFIX geof:<http://www.opengis.net/def/function/geosparql/> "
                "SELECT (COUNT(*) AS ?n) WHERE { ?s geo:asWKT ?w . "
                'FILTER(geof:sfWithin(?w, "POLYGON((90 30,160 30,160 80,90 80,90 30))"^^geo:wktLiteral)) '
                f'FILTER(STRSTARTS(STR(?s), "urn:wb:{namespace}:")) }}'
            )
        if index_check:
            (directory / "publication.rq").write_text(index_check + "\n")
            assert query(args.port, index_check) == expected
        if index == "embed":
            after_embedding = caught_up(args)
            save(directory / "embedding-before.json", before_embedding)
            save(directory / "embedding-after.json", after_embedding)
            if operation == "literal":
                assert (
                    after_embedding["embedding"]["embedded"]
                    > before_embedding["embedding"]["embedded"]
                )
                assert provider_after["inputs"] > provider_before["inputs"], (
                    "worker made no provider progress during writes"
                )
            save(
                directory / "provider-window.json",
                {"before": provider_before, "after": provider_after},
            )
        server.close()
        reopen = directory / "reopen"
        reopen.mkdir()
        server = Server(args, store, reopen, grouped)
        assert query(args.port, check) == expected
        if index_check:
            assert query(args.port, index_check) == expected
        if index in ("text", "geo"):
            assert (
                query(args.port, TEXT_QUERY if index == "text" else GEO_QUERY)
                == SENTINELS
            )
        else:
            caught_up(args)
        result.update(
            {
                "grouped": grouped,
                "scenario": scenario,
                "index": index,
                "people": args.current_people,
                "elapsed": end - start,
                "namespace": namespace,
                "reopen": "passed",
            }
        )
        save(directory / "result.json", result)
        return result
    finally:
        stop.set()
        pool.shutdown(wait=True)
        server.close()
        # Only this invocation's fresh cloned store is removed; raw receipts stay.
        shutil.rmtree(store)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--provider", type=int, help=argparse.SUPPRESS)
    p.add_argument("--binary")
    p.add_argument("--write-bench")
    p.add_argument("--out")
    p.add_argument("--source-commit")
    p.add_argument("--blocks", type=int, default=5)
    p.add_argument("--people", type=int, nargs="+", default=[100000, 1000000])
    p.add_argument(
        "--indexes",
        nargs="+",
        choices=["text", "geo", "embed"],
        default=["text", "geo", "embed"],
    )
    p.add_argument(
        "--scenarios",
        nargs="+",
        default=["serial-literal", "serial-iri", "serial-delete", "writers", "mixed"],
        choices=["serial-literal", "serial-iri", "serial-delete", "writers", "mixed"],
    )
    p.add_argument("--serial-changes", type=int, default=2000)
    p.add_argument("--warmup", type=int, default=200)
    p.add_argument("--writer-changes", type=int, default=50000)
    p.add_argument(
        "--write-rate",
        type=float,
        default=0,
        help="fixed mixed rate; derive from slow pilot when zero",
    )
    p.add_argument("--port", type=int, default=3986)
    p.add_argument("--provider-port", type=int, default=3987)
    p.add_argument("--server-cpus", default="0-11")
    p.add_argument("--client-cpus", default="12-13")
    p.add_argument("--provider-cpus", default="14")
    p.add_argument(
        "--heavy-lock", default=str(pathlib.Path.home() / "lanes/.heavy.lock")
    )
    args = p.parse_args()
    if args.provider:
        serve_provider(args.provider)
        return
    for key in ("binary", "write_bench", "out", "source_commit"):
        assert getattr(args, key), "missing --" + key.replace("_", "-")
    assert args.blocks > 0 and min(args.people) > 0
    assert args.serial_changes > 0 and args.writer_changes > 0 and args.warmup >= 0
    assert math.isfinite(args.write_rate) and args.write_rate >= 0
    # Do not wait silently behind another lane, and do not benchmark without ownership.
    heavy_lock = open(args.heavy_lock, "a+")  # noqa: SIM115 -- Held across the full campaign and explicitly closed below.
    fcntl.flock(heavy_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def terminate(_signum, _frame):
        raise KeyboardInterrupt("campaign terminated")

    signal.signal(signal.SIGTERM, terminate)
    for port in (args.port, args.provider_port):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", port))
    args.binary, args.write_bench = map(
        os.path.abspath, (args.binary, args.write_bench)
    )
    root = pathlib.Path(args.out).resolve()
    root.mkdir(parents=True, exist_ok=False)
    save(
        root / "manifest.json",
        {
            "settings": vars(args),
            "binary_sha256": digest(args.binary),
            "client_sha256": digest(args.write_bench),
            "harness_sha256": digest(__file__),
            "uname": os.uname()._asdict()
            if hasattr(os.uname(), "_asdict")
            else list(os.uname()),
            "started_unix": time.time(),
            "scope": "native storage; no injected sync delay",
        },
    )
    record_environment(root)
    probe_hwm(root)
    provider = subprocess.Popen(
        [
            "taskset",
            "-c",
            args.provider_cpus,
            sys.executable,
            os.path.abspath(__file__),
            "--provider",
            str(args.provider_port),
        ]
    )
    try:
        for _ in range(100):
            try:
                get_json(args.provider_port, "/")
                break
            except OSError:
                time.sleep(0.05)
        else:
            raise RuntimeError("provider startup")
        results = []
        for people in args.people:
            args.current_people = people
            for index in args.indexes:
                cell = root / f"{index}-{people}"
                cell.mkdir()
                triples = prepare(args, cell, index, people)
                write_rate = args.write_rate
                if not write_rate and "mixed" in args.scenarios and index != "embed":
                    pilot = [
                        measure(
                            args,
                            cell,
                            index,
                            "writers",
                            grouped,
                            triples,
                            f"pilot-{int(grouped)}",
                            0,
                        )
                        for grouped in (False, True)
                    ]
                    write_rate = min(r["tps"] for r in pilot) * 0.8
                for block in range(args.blocks):
                    for scenario in args.scenarios:
                        if index == "embed" and scenario == "mixed":
                            continue
                        for pos, grouped in enumerate((False, True, True, False)):
                            result = measure(
                                args,
                                cell,
                                index,
                                scenario,
                                grouped,
                                triples,
                                f"b{block}-{scenario}-{pos}",
                                write_rate,
                            )
                            result["block"] = block
                            results.append(result)
                            save(root / "results.json", results)
                            if pos == 3:
                                save(root / "gate-summary.json", summarize(results))
                            print(json.dumps(result), flush=True)
        summary = summarize(results)
        save(root / "gate-summary.json", summary)
        save(
            root / "completed.json",
            {
                "finished_unix": time.time(),
                "processes": len(results),
                "gates_passed": summary["passed"],
                "full_protocol": args.blocks >= 5
                and args.people == [100000, 1000000]
                and set(args.indexes) == {"text", "geo", "embed"}
                and len(args.scenarios) == 5
                and args.serial_changes >= 2000
                and args.writer_changes == 50000,
            },
        )
        if not summary["passed"]:
            raise SystemExit(3)
    finally:
        provider.terminate()
        provider.wait(timeout=10)
        heavy_lock.close()


if __name__ == "__main__":
    main()
