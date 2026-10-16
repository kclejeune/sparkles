"""A loopback proxy and a cost ledger for paid Anthropic evaluation runs.

The proxy alone holds the API key. Children of the evaluator hold a random token
and reach this proxy, which forwards each request once to the Messages API over
the standard library's HTTP client. Every upstream request first writes a reserve
line to an append-only ledger, and the reply settles it. The ledger is the
authority for the local cost cap, and a restart replays it, so a crash cannot
lower the recorded liability.
"""

import contextlib
import hmac
import http.client
import json
import os
import pathlib
import secrets
import socket
import ssl
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from evaluator import Invalid, strict_json

LEDGER_VERSION = 1
MODELS = ("claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5")
REQUEST_CAP, RESPONSE_CAP = 128 * 1024, 1024 * 1024
# Integer USD nanodollars per token as input, 5 minute cache write, 1 hour cache
# write, cache read and output. A second row applies above 100000 prompt tokens.
RATES = {
    MODELS[0]: ((100, 125, 200, 10, 500), (500, 625, 1000, 50, 2500)),
    MODELS[1]: ((2000, 2500, 4000, 100, 10000),) * 2,
    MODELS[2]: ((4000, 5000, 8000, 200, 20000),) * 2,
}
LONG_CONTEXT = 100000
ALLOWED_FIELDS = {
    "model",
    "max_tokens",
    "system",
    "messages",
    "output_config",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "stream",
    "metadata",
    "thinking",
}
REPLY_HEADERS = ("content-type", "retry-after", "request-id")


class CapExceeded(Invalid):
    pass


class Refused(Exception):
    """A request the proxy turns away with a 400 before it costs anything."""


class UpstreamFailure(Exception):
    """The request did not produce a complete reply. `sent` says whether the body left."""

    def __init__(self, reason, sent):
        super().__init__(reason)
        self.reason, self.sent = reason, sent


def bound(model, body_len, max_tokens):
    """An upper bound on one request's cost from its size alone.

    A token never covers less than one byte, so doubling the body and adding room
    for the system prompt and schema overhead bounds the input tokens.
    """
    rates = RATES[model]
    input_rate = max(r[0] for r in rates)
    output_rate = max(r[4] for r in rates)
    return (2 * body_len + 8192) * input_rate + max_tokens * output_rate


def count(value):
    if type(value) is not int or value < 0 or value > 10**9:
        raise ValueError("token count")
    return value


GEO_PREMIUM = (
    5,
    4,
)  # US-only inference. The published premium is 1.1, and this adds margin.
UNKNOWN_PREMIUM = (
    6  # The dearest premium of any priced mode, for values not recognised.
)


def cost(model, usage, limit=0):
    """The cost in nanodollars of one reply, or None when the usage cannot be read.

    Unknown usage keys are ignored. A US-only reply is charged its premium. A tier,
    geography or speed that is not recognised is charged at least `limit`, the
    request's bound, and at least the largest premium over the standard price, so a
    priced mode can never be settled low.
    """
    if not isinstance(usage, dict) or model not in RATES:
        return None
    try:
        inp, out = count(usage.get("input_tokens")), count(usage.get("output_tokens"))
        creation = count(usage.get("cache_creation_input_tokens", 0))
        read = count(usage.get("cache_read_input_tokens", 0))
        split = usage.get("cache_creation")
        if isinstance(split, dict):
            five = count(split.get("ephemeral_5m_input_tokens", 0))
            hour = count(split.get("ephemeral_1h_input_tokens", 0))
            if five + hour != creation:
                return None
        else:
            five, hour = 0, creation  # Unsplit writes are charged at the dearer rate.
    except ValueError:
        return None
    rates = RATES[model][int(inp + creation + read > LONG_CONTEXT)]
    exact = sum(n * r for n, r in zip((inp, five, hour, read, out), rates, strict=True))
    geo = usage.get("inference_geo")
    if (
        usage.get("service_tier") in (None, "standard")
        and usage.get("speed") in (None, "standard")
        and geo in (None, "global", "not_available", "us")
    ):
        if geo == "us":
            num, den = GEO_PREMIUM
            return (exact * num + den - 1) // den
        return exact
    return max(limit, exact * UNKNOWN_PREMIUM)


def has_key(value, name):
    if isinstance(value, dict):
        return name in value or any(has_key(v, name) for v in value.values())
    if isinstance(value, list):
        return any(has_key(v, name) for v in value)
    return False


def redact(value, key):
    """Return `value` with the key removed from every string, and whether any was."""
    changed = False

    def walk(v):
        nonlocal changed
        if isinstance(v, str):
            if key and key in v:
                changed = True
                return v.replace(key, "[REDACTED]")
            return v
        if isinstance(v, list):
            return [walk(x) for x in v]
        if isinstance(v, dict):
            return {walk(k): walk(x) for k, x in v.items()}
        return v

    return walk(value), changed


class Ledger:
    """Append-only JSON lines. Each line is flushed and fsynced before it is used.

    The first line holds the format version and the cap. Reopening an existing
    ledger resumes it, and nothing resets it. A reserve line without a settle line
    counts at its full bound.
    """

    def __init__(self, path, cap, carryover=None):
        self.path = pathlib.Path(path)
        self.cap = cap
        self.mutex = threading.Lock()
        self.reserved, self.settled, self.next_id = {}, 0, 1
        self.carryover = 0
        self.requests = 0
        self.over_bound = 0
        self.costs = []  # (case, model, cost) for every settled request
        if not self.path.is_absolute() or ".." in self.path.parts:
            raise Invalid("absolute ledger path required")
        self.lock = self.path.with_name(self.path.name + ".lock")
        try:
            self.lock_fd = os.open(
                self.lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
            )
        except FileExistsError:
            raise Invalid(
                "ledger is locked by another run, or by one that did not exit cleanly"
            ) from None
        try:
            fresh = not self.path.exists()
            self.fd = os.open(
                self.path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600
            )
            if fresh:
                self.write({"ledger": LEDGER_VERSION, "capNanodollars": cap})
                self.sync_directory()
                if carryover:
                    self.write(
                        {"carryover": carryover["name"], "cost": carryover["cost"]}
                    )
                self.carryover = carryover["cost"] if carryover else 0
            else:
                self.replay(carryover)
        except BaseException:
            self.close()
            raise

    def write(self, line):
        os.write(self.fd, (json.dumps(line, separators=(",", ":")) + "\n").encode())
        os.fsync(self.fd)

    def sync_directory(self):
        fd = os.open(self.path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)

    def replay(self, carryover):
        try:
            self.apply(self.path.read_bytes().decode().splitlines(), carryover)
        except (KeyError, TypeError, AttributeError, UnicodeError):
            raise Invalid("ledger is unreadable") from None

    def apply(self, lines, carryover):
        head = strict_json(lines[0]) if lines else None
        if (
            not isinstance(head, dict)
            or head.get("ledger") != LEDGER_VERSION
            or head.get("capNanodollars") != self.cap
        ):
            raise Invalid("unknown ledger version or a different cap")
        seen = None
        for text in lines[1:]:
            line = strict_json(text)
            if "carryover" in line:
                seen = {"name": line["carryover"], "cost": line["cost"]}
                self.carryover += line["cost"]
            elif line.get("op") == "reserve":
                self.reserved[line["id"]] = line
                self.next_id = max(self.next_id, line["id"] + 1)
            elif line.get("op") == "settle":
                reserve = self.reserved.pop(line["id"])
                self.settled += line["cost"]
                self.costs.append((reserve.get("case"), reserve["model"], line["cost"]))
            else:
                raise Invalid("unknown ledger line")
        if seen != carryover:
            raise Invalid("the ledger's carryover differs from the run's")

    def spent(self):
        return (
            self.carryover
            + self.settled
            + sum(r["bound"] for r in self.reserved.values())
        )

    def reserve(self, model, request_bound, case=None):
        with self.mutex:
            if self.spent() + request_bound > self.cap:
                raise CapExceeded("cost cap")
            ident = self.next_id
            line = {
                "op": "reserve",
                "id": ident,
                "model": model,
                "bound": request_bound,
                "case": case,
            }
            self.write(line)  # Durable before any request leaves.
            self.next_id += 1
            self.reserved[ident] = line
            self.requests += 1
            return ident

    def settle(self, ident, amount, status, usage=None, request_id=None):
        with self.mutex:
            reserve = self.reserved[ident]
            over = amount > reserve["bound"]
            self.over_bound += int(over)
            self.write(
                {
                    "op": "settle",
                    "overBound": over,
                    "id": ident,
                    "cost": amount,
                    "status": status,
                    "usage": usage,
                    "requestId": request_id,
                }
            )
            del self.reserved[ident]
            self.settled += amount
            self.costs.append((reserve.get("case"), reserve["model"], amount))

    def case_costs(self):
        """Nanodollars per (case, model) over every settled request of a case."""
        totals = {}
        with self.mutex:
            for case, model, amount in self.costs:
                if case:
                    totals[(case, model)] = totals.get((case, model), 0) + amount
        return totals

    def report(self):
        with self.mutex:
            unresolved = sum(r["bound"] for r in self.reserved.values())
            return {
                "ledger": str(self.path),
                "capNanodollars": self.cap,
                "carryoverNanodollars": self.carryover,
                "settledNanodollars": self.settled,
                "unresolvedNanodollars": unresolved,
                "upperLiabilityNanodollars": self.carryover + self.settled + unresolved,
                "requests": self.requests,
                "settledAboveBound": self.over_bound,
            }

    def close(self):
        for name in ("fd", "lock_fd"):
            fd = getattr(self, name, None)
            if fd is not None:
                os.close(fd)
                setattr(self, name, None)
        with contextlib.suppress(OSError):
            self.lock.unlink()


class HTTPUpstream:
    """One standard-library request. It never retries, and http.client decodes the
    Content-Length, chunked and close-delimited framings alike."""

    def __init__(self, key, host="api.anthropic.com", port=443, tls=True):
        self.key, self.host, self.port, self.tls = key, host, port, tls

    def send(self, path, body, timeout):
        """Return (status, headers, body). Raise UpstreamFailure without a reply."""
        if self.tls:
            conn = http.client.HTTPSConnection(
                self.host,
                self.port,
                timeout=timeout,
                context=ssl.create_default_context(),
            )
        else:
            conn = http.client.HTTPConnection(self.host, self.port, timeout=timeout)
        expired = threading.Event()

        def expire():
            expired.set()
            if conn.sock is not None:
                with contextlib.suppress(OSError):
                    conn.sock.shutdown(socket.SHUT_RDWR)

        # A socket timeout alone permits an endless trickle. This watchdog covers
        # connecting, sending and reading together.
        timer = threading.Timer(timeout, expire)
        timer.daemon = True
        sent = False
        try:
            timer.start()
            conn.connect()
            sent = True  # From here the body may reach the server.
            conn.request(
                "POST",
                path,
                body=body,
                headers={
                    "content-type": "application/json",
                    "anthropic-version": "2023-06-01",
                    "accept-encoding": "identity",
                    "x-api-key": self.key,
                },
            )
            reply = conn.getresponse()
            data = reply.read(RESPONSE_CAP + 1)
            if expired.is_set():
                raise UpstreamFailure("timeout", True)
            if len(data) > RESPONSE_CAP:
                raise UpstreamFailure("reply too large", True)
            return reply.status, reply.getheaders(), data
        except UpstreamFailure:
            raise
        except (OSError, http.client.HTTPException) as error:
            reason = "timeout" if expired.is_set() else type(error).__name__
            raise UpstreamFailure(reason, sent) from None
        finally:
            timer.cancel()
            conn.close()


def error_body(kind, message):
    return json.dumps(
        {"type": "error", "error": {"type": kind, "message": message}}
    ).encode()


class Proxy:
    def __init__(
        self,
        ledger,
        upstream,
        key,
        models=MODELS,
        max_output_tokens=4096,
        effort="medium",
        timeout=120,
    ):
        self.ledger, self.upstream, self.key = ledger, upstream, key
        self.models, self.max_output_tokens = tuple(models), max_output_tokens
        self.effort, self.timeout = effort, timeout
        self.token = secrets.token_urlsafe(32)
        self.case = None
        self.cap_refusals = 0
        self.last_headers = []
        self.request_bytes = []
        self.mutex = threading.Lock()
        self.server_port = None

    def set_case(self, case):
        self.case = case

    def case_cost(self, case):
        """Nanodollars settled so far for one case, over every model it called."""
        return sum(v for (c, _), v in self.ledger.case_costs().items() if c == case)

    def report(self):
        out = self.ledger.report()
        out.update(
            capRefusals=self.cap_refusals,
            effort=self.effort,
            maxOutputTokens=self.max_output_tokens,
            requestTimeoutSecs=self.timeout,
        )
        return out

    def prepare(self, raw):
        """Validate a request and return the body to send. Raise Refused otherwise."""
        if len(raw) > REQUEST_CAP:
            raise Refused("request too large")
        try:
            body = strict_json(raw)
        except Invalid:
            raise Refused("request is not JSON") from None
        if not isinstance(body, dict) or body.keys() - ALLOWED_FIELDS:
            raise Refused("unsupported request field, tools are not allowed")
        if has_key(body, "cache_control"):
            raise Refused("cache control is not allowed")
        max_tokens = body.get("max_tokens")
        if body.get("model") not in self.models:
            raise Refused("model not allowed")
        if type(max_tokens) is not int or not 0 < max_tokens <= self.max_output_tokens:
            raise Refused("max_tokens out of range")
        if not isinstance(body.get("system", ""), str) or any(
            not isinstance(m, dict) or not isinstance(m.get("content"), str)
            for m in body.get("messages") or [None]
        ):
            # Only text bodies keep the byte-based bound valid. An image can cost
            # more tokens than its bytes.
            raise Refused("only text content is allowed")
        body["stream"] = False
        if self.effort:
            config = body.setdefault("output_config", {})
            if not isinstance(config, dict):
                raise Refused("output_config must be an object")
            config.setdefault("effort", self.effort)
        return body

    def handle(self, raw):
        """Forward one request. Return (status, headers, body) for the child."""
        try:
            body = self.prepare(raw)
        except Refused as refusal:
            return 400, [], error_body("invalid_request_error", str(refusal))
        model = body["model"]
        payload = json.dumps(body, separators=(",", ":"), ensure_ascii=False).encode()
        limit = bound(model, len(payload), body["max_tokens"])
        try:
            ident = self.ledger.reserve(model, limit, self.case)
        except CapExceeded:
            with self.mutex:
                self.cap_refusals += 1
            return 402, [], error_body("evaluation_budget", "the cost cap is reached")
        with self.mutex:
            self.request_bytes.append(len(payload))
        return self.forward(ident, model, limit, payload)

    def forward(self, ident, model, limit, payload):
        try:
            status, headers, data = self.upstream.send(
                "/v1/messages", payload, self.timeout
            )
        except UpstreamFailure as failure:
            # A request that may have reached the server is charged at its bound.
            self.ledger.settle(
                ident, limit if failure.sent else 0, "failure:" + failure.reason
            )
            return 502, [], error_body("api_error", "the upstream did not answer")
        self.last_headers = headers
        passed = [(k, v) for k, v in headers if k.lower() in REPLY_HEADERS]
        request_id = next((v for k, v in headers if k.lower() == "request-id"), None)
        if not 200 <= status < 300:
            # The API does not bill an error reply, which is an assumption. A gateway
            # error after generation could be billed. The cap's margin absorbs it.
            # The child's own retries see the reply.
            self.ledger.settle(ident, 0, f"http-{status}", request_id=request_id)
            return status, passed, self.clean_bytes(data)
        try:
            reply = strict_json(data)
        except Invalid:
            reply = None
        usage = reply.get("usage") if isinstance(reply, dict) else None
        amount = cost(model, usage, limit)
        self.ledger.settle(
            ident,
            limit if amount is None else amount,
            "ok" if amount is not None else "usage-unknown",
            usage=usage if isinstance(usage, dict) else None,
            request_id=request_id,
        )
        if reply is None:
            return 502, [], error_body("api_error", "the upstream reply is not JSON")
        clean, _ = redact(reply, self.key)
        return status, passed, json.dumps(clean, ensure_ascii=False).encode()

    def clean_bytes(self, data):
        if self.key and self.key.encode() in data:
            data = data.replace(self.key.encode(), b"[REDACTED]")
        return data

    def serve(self):
        proxy = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, *args):
                pass

            def reply(self, status, headers, body):
                self.send_response(status)
                names = {k.lower() for k, _ in headers}
                if "content-type" not in names:
                    self.send_header("content-type", "application/json")
                for k, v in headers:
                    self.send_header(k, v)
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    self.wfile.write(body)

            def do_POST(self):
                self.connection.settimeout(proxy.timeout + 30)
                token = self.headers.get("x-api-key", "")
                length = self.headers.get("Content-Length", "")
                if self.path != "/v1/messages" or not hmac.compare_digest(
                    token.encode(), proxy.token.encode()
                ):
                    return self.reply(
                        401, [], error_body("authentication_error", "bad route")
                    )
                if not length.isascii() or not length.isdecimal() or len(length) > 8:
                    return self.reply(
                        400, [], error_body("invalid_request_error", "length")
                    )
                if not 0 < int(length) <= REQUEST_CAP:
                    return self.reply(
                        413, [], error_body("invalid_request_error", "too large")
                    )
                raw = self.rfile.read(int(length))
                if len(raw) != int(length):
                    return self.reply(
                        400, [], error_body("invalid_request_error", "short body")
                    )
                self.reply(*proxy.handle(raw))

            def do_GET(self):
                self.reply(405, [], error_body("invalid_request_error", "method"))

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.daemon_threads = False
        self.server_port = server.server_port
        return server

    def config(self):
        """The model configuration of the children: one loopback provider."""
        provider = {
            "kind": "anthropic",
            "endpoint": f"http://127.0.0.1:{self.server_port}",
            "apiKey": {"secret": "evalgate"},
            "allowedModels": list(self.models),
            "models": {m: {} for m in self.models},
            "maxOutputTokens": self.max_output_tokens,
            "requestTimeoutSecs": self.timeout,
            "structuredOutput": "json-schema",
            "concurrency": 1,
        }
        return {
            "models": {
                "providers": {"evaluation": provider},
                "roles": {
                    r: [{"provider": "evaluation", "model": m} for m in self.models]
                    for r in ("draft", "repair", "summarize")
                },
            }
        }


SCHEMA = {
    "type": "object",
    "properties": {"ok": {"type": "boolean"}},
    "required": ["ok"],
    "additionalProperties": False,
}


def preflight(proxy, upstream, timeout=30):
    """Check framing, authentication and every model before any evaluation spend.

    Counting tokens costs nothing. Each configured model then gets one small request
    with the same structured-output shape and effort that the run uses, through the
    ledger like every other request. A model that the account cannot use, a parameter
    it rejects or a reply that is not the requested JSON stops the run here, for a
    fraction of a cent each. Returns the replies' status lines and header names and
    values for the record, without the rate-limit, request and key-like headers.
    """
    model = proxy.models[0]
    counted = upstream.send(
        "/v1/messages/count_tokens",
        json.dumps(
            {"model": model, "messages": [{"role": "user", "content": "ping"}]}
        ).encode(),
        timeout,
    )
    if counted[0] != 200 or not isinstance(strict_json(counted[2]), dict):
        raise Invalid("preflight token count failed")

    def keep(headers):
        dropped = ("anthropic-ratelimit", "request-id", "x-api-key", "set-cookie")
        return [[k, v] for k, v in headers if not k.lower().startswith(dropped)]

    models = {}
    for name in proxy.models:
        request = {
            "model": name,
            "max_tokens": 256,
            "messages": [{"role": "user", "content": "Reply with ok true."}],
            "output_config": {"format": {"type": "json_schema", "schema": SCHEMA}},
        }
        start = time.monotonic()
        status, _, body = proxy.handle(json.dumps(request).encode())
        if status != 200:
            raise Invalid(f"preflight failed for {name} with status {status}")
        try:
            text = "".join(
                b.get("text", "")
                for b in strict_json(body)["content"]
                if b.get("type") == "text"
            )
            valid = isinstance(strict_json(text), dict)
        except (Invalid, KeyError, TypeError, AttributeError):
            valid = False
        if not valid:
            raise Invalid(f"preflight reply of {name} is not the requested JSON")
        models[name] = {
            "status": status,
            "headers": keep(proxy.last_headers),
            "seconds": time.monotonic() - start,
        }
    first = models[proxy.models[0]]
    return {
        "countTokens": {"status": counted[0], "headers": keep(counted[1])},
        "message": first,
        "models": models,
    }
