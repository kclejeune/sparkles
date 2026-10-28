#!/usr/bin/env python3
"""Test an extracted candidate in a pinned clean runtime container; never build."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

SPEC = importlib.util.spec_from_file_location("server_artifact", Path(__file__).with_name("server-artifact.py"))
artifact = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(artifact)
PREDICATE = "urn:sparkles:smoke:value"
XSD = "http://www.w3.org/2001/XMLSchema#integer"
SELECT = f"SELECT ?s ?value WHERE {{ ?s <{PREDICATE}> ?value }} ORDER BY ?s"


def expected_bindings(extra=False):
    values = [(f"urn:sparkles:smoke:{i:02}", str(i)) for i in range(10)]
    if extra:
        values.append(("urn:sparkles:smoke:extra", "10"))
    return [{"s": {"type": "uri", "value": subject},
             "value": {"type": "literal", "value": value, "datatype": XSD}}
            for subject, value in values]


class Runtime:
    def __init__(self, image, root, data, logs, execute=None):
        self.image, self.root, self.data, self.logs = image, root, data, logs
        self.execute = execute or subprocess.run
        self.name = "sparkles-installed-" + uuid.uuid4().hex
        self.created = False
        self.base = None

    def docker(self, *args, check=True, timeout=45):
        result = self.execute(["docker", *map(str, args)], capture_output=True, text=True,
                              timeout=timeout)
        if check and result.returncode:
            raise RuntimeError(f"docker {args[0]} failed: {result.stderr.strip()}")
        return result

    def binary(self, *args):
        return self.docker("exec", self.name, "/opt/sparkles/bin/sparkles", *args).stdout

    def start(self):
        # No image is built and no runtime packages or libraries are installed.
        # Even a timed-out create may leave the uniquely named container behind.
        self.created = True
        self.docker("create", "--name", self.name, "--pull=never", "--platform=linux/amd64",
                    "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
                    "--user", f"{os.getuid()}:{os.getgid()}",
                    "--tmpfs", "/tmp:rw,nosuid,nodev,mode=1777",
                    "--mount", f"type=bind,source={self.root},target=/opt/sparkles,readonly",
                    "--mount", f"type=bind,source={self.data},target=/data",
                    "--env", "XDG_CONFIG_HOME=/tmp/config", "--env", "XDG_CACHE_HOME=/tmp/cache",
                    "--env", "XDG_STATE_HOME=/tmp/state", "--env", "SPARKLES_UI_DIR=",
                    "--env", "LD_LIBRARY_PATH=", "--publish", "127.0.0.1::3030",
                    "--entrypoint", "/opt/sparkles/bin/sparkles", self.image,
                    "--memory-profile", "small", "serve", "--data", "/data",
                    "--host", "0.0.0.0", "--port", "3030", "--allow-open-network")
        self.docker("start", self.name)
        address = self.docker("port", self.name, "3030/tcp").stdout.strip()
        artifact.require(address.startswith("127.0.0.1:") and address.count("\n") == 0,
                         "runtime must publish only one loopback port")
        self.base = "http://" + address
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            try:
                self.request("/$/ready")
                self.binary("ping", "--quiet", "--timeout", "4", "127.0.0.1:3030")
                return
            except (urllib.error.URLError, TimeoutError):
                state = self.docker("inspect", "--format={{.State.Running}}", self.name).stdout.strip()
                artifact.require(state == "true", "installed server exited during startup")
                time.sleep(0.2)
        raise RuntimeError("installed server did not become ready within 120 seconds")

    def request(self, path, data=None, content_type=None):
        headers = {"Accept": "application/sparql-results+json"}
        if content_type:
            headers["Content-Type"] = content_type
        request = urllib.request.Request(self.base + path, data=data, headers=headers)
        with urllib.request.urlopen(request, timeout=10) as response:
            artifact.require(200 <= response.status < 300, "unexpected HTTP status")
            return response.read(), response.headers.get_content_type()

    def query(self, dataset, query):
        body = urllib.parse.urlencode({"query": query}).encode()
        result, _ = self.request(f"/{dataset}/sparql", body, "application/x-www-form-urlencoded")
        return json.loads(result)

    def answers(self, dataset, extra=False):
        artifact.require(self.query(dataset, SELECT)["results"]["bindings"] == expected_bindings(extra),
                         f"{dataset}: wrong ordered SELECT bindings")
        present = f'ASK {{ <urn:sparkles:smoke:00> <{PREDICATE}> "0"^^<{XSD}> }}'
        absent = f'ASK {{ <urn:sparkles:smoke:missing> <{PREDICATE}> ?o }}'
        artifact.require(self.query(dataset, present).get("boolean") is True, "present ASK is not true")
        artifact.require(self.query(dataset, absent).get("boolean") is False, "absent ASK is not false")
        count = self.query(dataset, "SELECT (COUNT(*) AS ?count) WHERE { ?s ?p ?o }")
        artifact.require(count["results"]["bindings"][0]["count"]["value"] == str(11 if extra else 10),
                         "wrong dataset count")

    def stop(self):
        self.docker("stop", "--time", "30", self.name, timeout=40)
        state = json.loads(self.docker("inspect", "--format={{json .State}}", self.name).stdout)
        artifact.require(state["ExitCode"] == 0 and not state.get("OOMKilled"),
                         f"server did not exit gracefully: {state}")

    def cleanup(self):
        if self.created:
            try:
                result = self.docker("logs", self.name, check=False)
                self.logs.write_text(result.stdout + result.stderr)
            finally:
                removal = self.docker("rm", "--force", self.name, check=False)
                self.created = removal.returncode != 0 and "No such container" not in removal.stderr
                artifact.require(not self.created, "failed to remove installed-smoke container")


def smoke(archive, checksums, version, runtime, output):
    artifact.require(runtime in artifact.IMAGES["runtimes"], "unsupported runtime image")
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    image = artifact.IMAGES["runtimes"][runtime]
    receipt = {"passed": False, "runtime": runtime, "image": image,
               "archive_sha256": artifact.sha256(archive), "version": version}
    try:
        with tempfile.TemporaryDirectory(prefix="sparkles-installed-") as temporary:
            work = Path(temporary)
            root, record = artifact.verify(archive, checksums, work / "installed", version)
            artifact.require(record.get("builder") == artifact.IMAGES["builder"], "wrong builder identity")
            artifact.require(record.get("default_features_requested") is True, "defaults were not requested")
            receipt["commit"] = record["commit"]
            # The pinned runtime must already exist locally; pulling is a separate explicit step.
            data = work / "data"
            data.mkdir()
            for generation in range(2):
                process = Runtime(image, root, data, output / f"server-{generation}.log")
                try:
                    process.start()
                    artifact.require(process.binary("--version").strip() == f"sparkles {version}",
                                     "installed version mismatch")
                    artifact.require(process.binary("--help").strip(), "missing installed help")
                    artifact.require(".TH" in process.binary("man"), "missing installed man page")
                    for shell in ("bash", "zsh", "fish"):
                        artifact.require(process.binary("completions", shell).strip(), "missing completion")
                    artifact.require(json.loads(process.binary("openapi")) ==
                                     json.loads((root / artifact.DOC / "openapi.json").read_text()),
                                     "installed OpenAPI differs from archive")
                    info, _ = process.request("/$/server")
                    info = json.loads(info)
                    artifact.require(info["version"] == version and info["formats"], "missing server identity")
                    receipt[f"server_{generation}"] = {"version": info["version"], "formats": info["formats"]}
                    for asset, types in (("index.html", {"text/html"}),
                                         (record["ui"]["script"], {"application/javascript", "text/javascript"}),
                                         (record["ui"]["wasm"], {"application/wasm"})):
                        payload, mime = process.request("/ui/" if asset == "index.html" else "/ui/" + asset)
                        artifact.require(mime in types and hashlib.sha256(payload).hexdigest() ==
                                         record["ui"]["files"][asset], "wrong embedded UI asset")
                    if generation == 0:
                        triples = "".join(f'<urn:sparkles:smoke:{i:02}> <{PREDICATE}> "{i}"^^<{XSD}> .\n'
                                          for i in range(10)).encode()
                        for dataset, kind in (("ephemeral", "mem"), ("durable", "persistent")):
                            process.request("/$/datasets", json.dumps({"dbName": dataset, "dbType": kind}).encode(),
                                            "application/json")
                            process.request(f"/{dataset}/data", triples, "application/n-triples")
                            process.answers(dataset)
                        update = f'INSERT DATA {{ <urn:sparkles:smoke:extra> <{PREDICATE}> "10"^^<{XSD}> }}'
                        process.request("/durable/update", update.encode(), "application/sparql-update")
                        process.answers("durable", extra=True)
                    else:
                        process.answers("durable", extra=True)
                        try:
                            result = process.query("ephemeral", "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }")
                            artifact.require(result["results"]["bindings"][0]["n"]["value"] == "0",
                                             "memory dataset unexpectedly persisted data")
                        except urllib.error.HTTPError as error:
                            artifact.require(error.code == 404, "unexpected memory dataset reopen response")
                    process.stop()
                finally:
                    process.cleanup()
            receipt["passed"] = True
    finally:
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--checksums", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--runtime", choices=artifact.IMAGES["runtimes"], required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    smoke(args.archive, args.checksums, args.version, args.runtime, args.out)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        raise SystemExit(f"server-installed-smoke: {error}")
