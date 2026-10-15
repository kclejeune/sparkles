"""Evaluation-only, single-owner durable aggregate liability and dispatch gate.

This module has no credential discovery. The root-admitted wrapper supplies a key in
memory. Tests inject a counted fake dispatch into the same reservation path.
"""

import contextlib
import fcntl
import hashlib
import http.client
import json
import os
import pathlib
import queue
import re
import secrets
import socket
import sqlite3
import ssl
import threading
import time
from decimal import Decimal
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from evaluator import Invalid, strict_json

CAP = 25000000000
MODELS = ("claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5")
BOUNDS = {
    model: int(
        (Decimal(1000000) * Decimal(input_rate) + Decimal(1024) * Decimal(output_rate))
        * Decimal(1000)
    )
    for model, input_rate, output_rate in zip(
        MODELS, ("1", "4", "8"), ("2.5", "10", "20")
    )
}
AUTHORITY_HASHES = {
    "models": "ecfd870df998a595046a9aa322cbfc2ded6714d0b97cc1a2bf3bce27bbaa7eea",
    "pricing": "2484a3cc0cb4d9099d9ead2a1f568a5aec4e30aa80f8b89f98747fef0138b5a7",
    "billing": "3a2fd2a46c484084552d7186721862f765f0fd1c9b2885d06dc0084d472465ef",
    "grant": "c73d33090ac3140f0bec681d678e907f2c57e22c6f441d19e607fb00110c50d5",
    "compatibility": "8e99831f9bb35034e426d8e897df8b902f3ef12fd0f29dd148596984e38a3f0b",
}
REQUEST_CAP, RESPONSE_CAP, ERROR_CAP = 128 * 1024, 1024 * 1024, 64 * 1024
RATES = {
    MODELS[0]: ((100, 125, 200, 10, 500), (500, 625, 1000, 50, 2500)),
    MODELS[1]: ((2000, 2500, 4000, 100, 10000),) * 2,
    MODELS[2]: ((4000, 5000, 8000, 200, 20000),) * 2,
}  # Integer USD nanodollars per token, exactly Decimal(USD/MTok)*1000.


class GateError(Invalid):
    def __init__(self, code):
        self.code = code
        super().__init__(code)


def integer(value, maximum=1000000):
    if type(value) is not int or not 0 <= value <= maximum:
        raise GateError("usage_integer")
    return value


def validate_request(raw):
    if not isinstance(raw, bytes) or len(raw) > REQUEST_CAP:
        raise GateError("request_cap")
    v = strict_json(raw)
    if not isinstance(v, dict) or v.keys() - {
        "model",
        "max_tokens",
        "system",
        "messages",
        "output_config",
        "stream",
        "service_tier",
        "inference_geo",
    }:
        raise GateError("request_fields")
    if (
        v.get("model") not in MODELS
        or type(v.get("max_tokens")) is not int
        or v["max_tokens"] != 1024
    ):
        raise GateError("request_model_limit")
    if (
        not isinstance(v.get("system"), str)
        or not isinstance(v.get("messages"), list)
        or not v["messages"]
    ):
        raise GateError("request_messages")
    for m in v["messages"]:
        if (
            not isinstance(m, dict)
            or set(m) != {"role", "content"}
            or m["role"] not in ("user", "assistant")
            or not isinstance(m["content"], str)
        ):
            raise GateError("request_message")
    if "output_config" in v:
        o = v["output_config"]
        if not isinstance(o, dict) or set(o) != {"format"}:
            raise GateError("request_schema")
        f = o["format"]
        if (
            not isinstance(f, dict)
            or set(f) != {"type", "schema"}
            or f["type"] != "json_schema"
            or not isinstance(f["schema"], dict)
        ):
            raise GateError("request_schema")
        # Schema is data only. Tools, references to external schemas, and billing
        # options cannot be introduced through it; the original bytes are hashed.
    for k, expected in [
        ("stream", False),
        ("service_tier", "standard_only"),
        ("inference_geo", "global"),
    ]:
        if k in v and (type(v[k]) is not type(expected) or v[k] != expected):
            raise GateError("request_control_conflict")
        v[k] = expected
    return v


def validate_usage(model, response):
    if not isinstance(response, dict) or response.get("model") != model:
        raise GateError("returned_model")
    u = response.get("usage")
    allowed = {
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "cache_creation",
        "service_tier",
        "inference_geo",
        "output_tokens_details",
    }
    if (
        not isinstance(u, dict)
        or u.keys() - allowed
        or u.get("service_tier") != "standard"
        or u.get("inference_geo") != "global"
    ):
        raise GateError("usage_controls")
    inp, out = integer(u.get("input_tokens")), integer(u.get("output_tokens"), 1024)
    creation = integer(u.get("cache_creation_input_tokens", 0))
    read = integer(u.get("cache_read_input_tokens", 0))
    split = u.get("cache_creation")
    if split is None:
        if creation:
            raise GateError("usage_cache_split_missing")
        five = hour = 0
    else:
        if not isinstance(split, dict) or set(split) != {
            "ephemeral_5m_input_tokens",
            "ephemeral_1h_input_tokens",
        }:
            raise GateError("usage_cache_split")
        five = integer(split["ephemeral_5m_input_tokens"])
        hour = integer(split["ephemeral_1h_input_tokens"])
        if five + hour != creation:
            raise GateError("usage_cache_sum")
    details = u.get("output_tokens_details")
    if details is not None and (
        not isinstance(details, dict)
        or set(details) != {"thinking_tokens"}
        or integer(details["thinking_tokens"], 1024) > out
    ):
        raise GateError("usage_thinking")
    prompt = inp + creation + read
    if prompt + out > 1000000:
        raise GateError("usage_context")
    rates = RATES[model][int(prompt > 100000)]
    actual = sum(n * rate for n, rate in zip((inp, five, hour, read, out), rates))
    if actual > BOUNDS[model]:
        raise GateError("usage_bound")
    return actual, u


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def safe_absolute(path):
    p = pathlib.Path(path)
    if not p.is_absolute() or ".." in p.parts:
        raise GateError("absolute_owned_path_required")
    for a in [*reversed(p.parents), p]:
        if a.is_symlink():
            raise GateError("symlink_refused")
    return p


class Wallet:
    """One grant, one exclusive database, no automatic resume or replacement wallet."""

    def __init__(self, path, claim_path, identity, fault=lambda phase: None):
        self.path, self.claim = safe_absolute(path), safe_absolute(claim_path)
        self.identity, self.fault = identity, fault
        self.uncertain_settlements = set()
        self.dispatch_closed = False
        self.lock_fd = None
        self.db = None
        # Claim remains even if initialization fails: root recovery must retain it.
        fd = os.open(
            self.claim, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
        )
        with os.fdopen(fd, "w") as f:
            json.dump({"schema": 1, "wallet": str(self.path), "identity": identity}, f)
            f.flush()
            os.fsync(f.fileno())
        sync_directory(self.claim.parent)
        self.path.mkdir(mode=0o700)
        sync_directory(self.path.parent)
        self.lock_fd = os.open(
            self.path / "owner.lock",
            os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o600,
        )
        fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        dbpath = self.path / "wallet.sqlite3"
        fd = os.open(dbpath, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        os.close(fd)
        self.db = sqlite3.connect(dbpath, isolation_level=None, check_same_thread=False)
        self.db.execute("PRAGMA journal_mode=DELETE")
        self.db.execute("PRAGMA synchronous=FULL")
        if (
            self.db.execute("PRAGMA synchronous").fetchone()[0] != 2
            or self.db.execute("PRAGMA journal_mode").fetchone()[0] != "delete"
        ):
            raise GateError("durability_mode")
        with self.transaction():
            self.db.execute(
                "CREATE TABLE meta(schema INTEGER, identity TEXT, cap INTEGER, stopped TEXT, highwater INTEGER, blocked INTEGER)"
            )
            self.db.execute(
                "INSERT INTO meta VALUES(2,?,?,NULL,0,0)",
                (json.dumps(identity, sort_keys=True), CAP),
            )
            self.db.execute(
                "CREATE TABLE attempts(id INTEGER PRIMARY KEY, case_id TEXT, model TEXT, payload_sha TEXT, bound INTEGER, state TEXT, actual INTEGER, usage TEXT)"
            )
            self.db.execute(
                "CREATE TABLE journal(seq INTEGER PRIMARY KEY, request_id INTEGER, transition TEXT, detail TEXT)"
            )
        sync_directory(self.path)
        self.mutex = threading.RLock()

    @contextlib.contextmanager
    def transaction(self):
        self.db.execute("BEGIN IMMEDIATE")
        try:
            yield
            self.fault("before_commit")
            self.db.execute("COMMIT")
            # FULL syncs journal/database contents. Also seal the DELETE-journal
            # namespace before a reservation can authorize any network attempt.
            self.fault("after_commit_before_directory_sync")
            sync_directory(self.path)
        except BaseException:
            if self.db.in_transaction:
                self.db.execute("ROLLBACK")
            raise

    def invariant(self):
        meta = self.db.execute(
            "SELECT schema,identity,cap,stopped,highwater,blocked FROM meta"
        ).fetchone()
        if (
            not meta
            or meta[0] != 2
            or meta[1] != json.dumps(self.identity, sort_keys=True)
            or meta[2] != CAP
        ):
            raise GateError("wallet_identity")
        settled, unresolved, count = 0, 0, 0
        for ident, model, bound, state, actual in self.db.execute(
            "SELECT id,model,bound,state,actual FROM attempts"
        ):
            count += 1
            if model not in BOUNDS or bound != BOUNDS[model]:
                raise GateError("wallet_bound")
            if state == "RESERVED_UNRESOLVED" and actual is None:
                unresolved += bound
            elif state == "SETTLED" and type(actual) is int and 0 <= actual <= bound:
                if ident in self.uncertain_settlements:
                    unresolved += bound
                else:
                    settled += actual
            else:
                raise GateError("wallet_state")
        if settled + unresolved > CAP or count > 4096:
            raise GateError("wallet_invariant")
        return settled, unresolved, count, meta

    def reserve_attempt(self, model, case_id, payload_sha, pre_control_sha=None):
        with self.mutex, self.transaction():
            settled, unresolved, count, meta = self.invariant()
            if self.dispatch_closed or meta[3] or unresolved:
                raise GateError("dispatch_stopped")
            bound = BOUNDS[model]
            if count == 4096 or settled + unresolved + bound > CAP:
                self.db.execute(
                    "UPDATE meta SET stopped=?,blocked=blocked+1", ("budget_exhausted",)
                )
                # Commit this terminal refusal before raising outside the transaction.
                refused = True
            else:
                refused = False
                c = self.db.execute(
                    "INSERT INTO attempts(case_id,model,payload_sha,bound,state) VALUES(?,?,?,?,?)",
                    (case_id, model, payload_sha, bound, "RESERVED_UNRESOLVED"),
                )
                ident = c.lastrowid
                self.db.execute(
                    "INSERT INTO journal(request_id,transition,detail) VALUES(?,?,?)",
                    (
                        ident,
                        "RESERVED_UNRESOLVED",
                        json.dumps(
                            {
                                "bound": bound,
                                "authority": AUTHORITY_HASHES["billing"],
                                "preControlSha256": pre_control_sha,
                                "wireSha256": payload_sha,
                            }
                        ),
                    ),
                )
                self.db.execute(
                    "UPDATE meta SET highwater=max(highwater,?)",
                    (settled + unresolved + bound,),
                )
        if refused:
            raise GateError("budget_exhausted")
        self.fault("after_reserve")
        return ident

    def settle_once(self, ident, actual, usage):
        # Keep the mutex through failure classification. Another caller must not
        # consume an apparent refund between COMMIT and a failed namespace sync.
        with self.mutex:
            eligible = False
            try:
                with self.transaction():
                    self.invariant()
                    row = self.db.execute(
                        "SELECT bound,state FROM attempts WHERE id=?", (ident,)
                    ).fetchone()
                    if (
                        not row
                        or row[1] != "RESERVED_UNRESOLVED"
                        or type(actual) is not int
                        or not 0 <= actual <= row[0]
                    ):
                        raise GateError("settlement_once")
                    eligible = True
                    self.fault("before_settlement")
                    self.db.execute(
                        "UPDATE attempts SET state=?,actual=?,usage=? WHERE id=?",
                        ("SETTLED", actual, json.dumps(usage, sort_keys=True), ident),
                    )
                    self.db.execute(
                        "INSERT INTO journal(request_id,transition,detail) VALUES(?,?,?)",
                        (
                            ident,
                            "SETTLED",
                            json.dumps(
                                {"actual": actual, "usage": usage}, sort_keys=True
                            ),
                        ),
                    )
                    self.invariant()
            except BaseException:
                if eligible:
                    # The real SQLite COMMIT may already have returned before a
                    # directory-sync error. Retain the original bound in the live
                    # report, and close admission even if writing stopped fails.
                    # This set is bounded by the 4096 admitted request IDs. It is
                    # not recovery state: every existing grant refuses startup.
                    self.uncertain_settlements.add(ident)
                    self.dispatch_closed = True
                raise

    def stop_dispatch(self, code):
        self.dispatch_closed = True
        with self.mutex, self.transaction():
            self.db.execute("UPDATE meta SET stopped=coalesce(stopped,?)", (code,))

    def snapshot_report(self):
        with self.mutex:
            settled, unresolved, count, meta = self.invariant()
            return {
                "schema": 2,
                "identity": self.identity,
                "capNanodollars": CAP,
                "knownSettledNanodollars": settled,
                "unresolvedUpperNanodollars": unresolved,
                "upperLiabilityNanodollars": settled + unresolved,
                "acceptedRequests": count,
                "budgetRefusals": meta[5],
                "highWaterNanodollars": meta[4],
                "stopped": meta[3],
                "settlementDurabilityUncertainIds": sorted(self.uncertain_settlements),
                "dispatchClosed": self.dispatch_closed,
                "attempts": [
                    dict(
                        zip(
                            (
                                "id",
                                "case",
                                "model",
                                "payloadSha256",
                                "bound",
                                "state",
                                "actual",
                                "usage",
                            ),
                            r,
                        )
                    )
                    for r in self.db.execute("SELECT * FROM attempts ORDER BY id")
                ],
            }

    def close(self):
        if self.db:
            self.db.close()
            self.db = None
        if self.lock_fd is not None:
            os.close(self.lock_fd)
            self.lock_fd = None


def read_upstream_response(stream):
    """Accept only complete Content-Length framing with one raw header budget.

    Every interim/final status line, header line and terminating CRLF consumes
    the same 64 KiB budget. Read at most the remainder plus one overflow byte,
    before parsing or accumulating headers. Chunked and close-delimited replies
    are unsupported and cannot release a reservation.
    """
    remaining = ERROR_CAP

    def line():
        nonlocal remaining
        # Keep the standard-library per-line bound as well as the aggregate cap.
        raw = stream.readline(min(http.client._MAXLINE + 1, remaining + 1))
        remaining -= len(raw)
        if remaining < 0 or len(raw) > http.client._MAXLINE:
            raise GateError("upstream_headers")
        if not raw.endswith(b"\r\n"):
            raise GateError("upstream_framing")
        return raw

    while True:
        status_line = line()
        status_match = re.fullmatch(
            rb"HTTP/1\.[01] ([0-9]{3}) [^\r\n]*\r\n", status_line
        )
        if not status_match:
            raise GateError("upstream_framing")
        status = int(status_match[1])
        if not 100 <= status <= 599 or status == 101:
            raise GateError("upstream_framing")
        headers = {}
        count = 0
        while True:
            raw = line()
            count += 1
            if count > http.client._MAXHEADERS:
                raise GateError("upstream_headers")
            if raw == b"\r\n":
                break
            name, separator, value = raw[:-2].partition(b":")
            if (
                not separator
                or not re.fullmatch(rb"[!#$%&'*+.^_`|~0-9A-Za-z-]+", name)
                or any(c < 32 and c != 9 or c == 127 for c in value)
            ):
                raise GateError("upstream_framing")
            headers.setdefault(name.lower(), []).append(value.strip(b" \t"))
        # An informational response cannot define the final body's framing. No
        # interim block resets the cumulative budget or authorizes settlement.
        if status < 200:
            if b"content-length" in headers or b"transfer-encoding" in headers:
                raise GateError("upstream_framing")
            continue
        break

    if not 200 <= status < 300:
        raise GateError("upstream_status")
    lengths = headers.get(b"content-length", [])
    if b"transfer-encoding" in headers or len(lengths) != 1:
        raise GateError("upstream_framing")
    length = lengths[0]
    if not re.fullmatch(rb"[0-9]+", length):
        raise GateError("upstream_framing")
    # Avoid converting unbounded decimal strings, including long leading zeros.
    significant = length.lstrip(b"0") or b"0"
    if len(significant) > len(str(RESPONSE_CAP)) or int(significant) > RESPONSE_CAP:
        raise GateError("upstream_cap")
    length = int(significant)
    if headers.get(b"content-encoding", [b"identity"]) != [b"identity"]:
        raise GateError("upstream_encoding")
    body = stream.read(length)
    if len(body) != length:
        raise GateError("upstream_framing")
    return body


def dispatch_once(payload, key, timeout):
    """Exactly one standard-library TLS request; no redirects/proxies/retries."""
    deadline = time.monotonic() + timeout
    resolved = queue.Queue(maxsize=1)

    def resolve():
        try:
            resolved.put(
                socket.getaddrinfo("api.anthropic.com", 443, type=socket.SOCK_STREAM)
            )
        except OSError:
            resolved.put(None)

    # Only fixed-host DNS happens in this daemon. A late answer never opens a
    # socket or sends a request; no key is passed to it. This bounds DNS too.
    resolver = threading.Thread(target=resolve, daemon=True)
    resolver.start()
    try:
        addresses = resolved.get(timeout=max(0, deadline - time.monotonic()))
    except queue.Empty:
        raise GateError("upstream_dns_timeout") from None
    if not addresses:
        raise GateError("upstream_dns")
    family, socktype, proto, _, address = addresses[0]

    def remaining():
        value = deadline - time.monotonic()
        if value <= 0:
            raise GateError("upstream_timeout")
        return value

    class FixedHTTPS(http.client.HTTPSConnection):
        def connect(self):
            nonlocal connection_socket
            raw = socket.socket(family, socktype, proto)
            connection_socket = raw
            try:
                raw.settimeout(remaining())
                raw.connect(address)
                raw.settimeout(remaining())
                self.sock = self._context.wrap_socket(
                    raw, server_hostname="api.anthropic.com"
                )
                connection_socket = self.sock
                self.sock.settimeout(remaining())
            except BaseException:
                raw.close()
                raise

    conn = FixedHTTPS(
        "api.anthropic.com", 443, timeout=timeout, context=ssl.create_default_context()
    )
    connection_socket = None

    def expire():
        if connection_socket is not None:
            try:
                connection_socket.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    timer = threading.Timer(remaining(), expire)
    timer.daemon = True
    try:
        # A socket timeout alone permits an endless trickle. The absolute watchdog
        # covers connect/send/headers/body together; no internal retry is made.
        timer.start()
        conn.connect()
        connection_socket = conn.sock
        conn.request(
            "POST",
            "/v1/messages",
            body=payload,
            headers={
                "content-type": "application/json",
                "anthropic-version": "2023-06-01",
                "x-api-key": key,
            },
        )
        with conn.sock.makefile("rb") as stream:
            return read_upstream_response(stream)
    finally:
        timer.cancel()
        timer.join(timeout=1)
        conn.close()


class Gate:
    def __init__(self, wallet, key, dispatch=dispatch_once, phase_deadline=None):
        self.wallet, self._key, self.dispatch = wallet, key, dispatch
        self.deadline = phase_deadline or time.monotonic() + 900
        self.token = secrets.token_urlsafe(32)
        self.route = "/" + secrets.token_urlsafe(24) + "/v1/messages"
        self.stop_persistence_failed = False
        self.serial = threading.Lock()
        self.case = None
        self.attempts = 0
        self.stopped = False
        self.received = 0

    def process(self, raw):
        received_case = self.case
        with self.serial:
            self.received += 1
            if self.stopped:
                raise GateError("dispatch_stopped")
            try:
                if (
                    not received_case
                    or received_case != self.case
                    or time.monotonic() >= self.deadline
                ):
                    raise GateError("case_or_deadline")
                v = validate_request(raw)
                before_sha = hashlib.sha256(raw).hexdigest()
                payload = json.dumps(
                    v, separators=(",", ":"), ensure_ascii=False
                ).encode()
                if len(payload) > REQUEST_CAP:
                    raise GateError("wire_request_cap")
                ident = self.wallet.reserve_attempt(
                    v["model"],
                    received_case,
                    hashlib.sha256(payload).hexdigest(),
                    before_sha,
                )
                # Independent test dispatch inspects the durable SQLite row here.
                self.attempts += 1
                body = self.dispatch(
                    payload, self._key, min(15, self.deadline - time.monotonic())
                )
                self.wallet.fault("after_response")
                if not isinstance(body, bytes) or len(body) > RESPONSE_CAP:
                    raise GateError("upstream_cap")
                response = strict_json(body)
                actual, usage = validate_usage(v["model"], response)
                self.wallet.settle_once(ident, actual, usage)
                # Parse first, then redact the exact key in every JSON string. JSON
                # escaping (including Unicode/control) cannot conceal it from this pass.
                sanitized, redacted = redact(response, self._key)
                out = json.dumps(sanitized, ensure_ascii=False).encode()
                if len(out) > RESPONSE_CAP:
                    raise GateError("sanitized_cap")
                return out, {
                    "requestId": ident,
                    "preControlSha256": before_sha,
                    "wireSha256": hashlib.sha256(payload).hexdigest(),
                    "redacted": redacted,
                }
            except Exception as error:  # noqa: BLE001 -- sanitize every error boundary
                self.stopped = True
                code = error.code if isinstance(error, GateError) else "gate_internal"
                try:
                    self.wallet.stop_dispatch(code)
                except Exception:  # noqa: BLE001 -- retain liability, never log raw errors
                    self.stop_persistence_failed = True
                raise GateError(code) from None

    def set_case(self, case):
        with self.serial:
            self.case = case


def redact(value, key):
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


def serve_gate(gate):
    class HeaderReader:
        def __init__(self, wrapped):
            self.wrapped, self.remaining = wrapped, ERROR_CAP

        def readline(self, limit=-1):
            cap = min(limit, self.remaining + 1) if limit >= 0 else self.remaining + 1
            value = self.wrapped.readline(cap)
            self.remaining -= len(value)
            if self.remaining < 0:
                raise http.client.LineTooLong("header byte cap")
            return value

        def __getattr__(self, name):
            return getattr(self.wrapped, name)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def log_message(self, *args):
            pass

        def handle(self):
            self.rfile = HeaderReader(self.rfile)

            # Bound header/body trickles as well as ordinary socket inactivity.
            def expire():
                try:
                    self.connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

            timer = threading.Timer(15, expire)
            timer.daemon = True
            timer.start()
            try:
                super().handle()
            finally:
                timer.cancel()
                timer.join(timeout=1)

        def send_error(self, code, message=None, explain=None):
            body = b'{"error":{"type":"evaluation_gate","message":"http_refused"}}'
            self.send_response(code)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def do_POST(self):
            self.connection.settimeout(15)
            try:
                # Duplicate framing/authority/auth headers are ambiguous, including
                # identical duplicate lengths. Incoming billing headers are not forwarded.
                allowed_headers = {
                    "content-length",
                    "content-type",
                    "host",
                    "x-api-key",
                    "anthropic-version",
                    "accept",
                    "accept-encoding",
                    "connection",
                    "user-agent",
                }
                if any(h.lower() not in allowed_headers for h in self.headers):
                    raise GateError("http_unknown_header")
                if self.headers.get("anthropic-version", "2023-06-01") != "2023-06-01":
                    raise GateError("http_version")
                for h in ("Content-Length", "Content-Type", "Host", "x-api-key"):
                    if len(self.headers.get_all(h, [])) != 1:
                        raise GateError("http_headers")
                if (
                    self.path != gate.route
                    or self.headers["Host"] != f"127.0.0.1:{self.server.server_port}"
                    or not secrets.compare_digest(self.headers["x-api-key"], gate.token)
                ):
                    raise GateError("http_auth_route")
                if (
                    self.headers.get("Transfer-Encoding") is not None
                    or self.headers["Content-Type"] != "application/json"
                ):
                    raise GateError("http_framing")
                length = self.headers["Content-Length"]
                if (
                    not length.isascii()
                    or not length.isdecimal()
                    or len(length) > 8
                    or not 0 < int(length) <= REQUEST_CAP
                ):
                    raise GateError("request_cap")
                raw = self.rfile.read(int(length))
                if len(raw) != int(length):
                    raise GateError("request_short")
                body, _ = gate.process(raw)
                status = 200
            except Exception as error:  # noqa: BLE001 -- sanitize every error boundary
                code = error.code if isinstance(error, GateError) else "gate_internal"
                body = json.dumps(
                    {
                        "type": "error",
                        "error": {"type": "evaluation_gate", "message": code},
                    }
                ).encode()
                status = (
                    402 if code in ("budget_exhausted", "dispatch_stopped") else 400
                )
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = False
    return server


def effective_config(gate):
    return {
        "models": {
            "providers": {
                "evaluation": {
                    "kind": "anthropic",
                    "endpoint": f"http://127.0.0.1:{gate.server_port}"
                    + gate.route.removesuffix("/v1/messages"),
                    "apiKey": {"secret": "evalgate"},
                    "allowedModels": list(MODELS),
                    "models": {m: {} for m in MODELS},
                    "maxOutputTokens": 1024,
                    "requestTimeoutSecs": 15,
                    "structuredOutput": "json-schema",
                    "concurrency": 1,
                }
            },
            "roles": {
                r: [{"provider": "evaluation", "model": m} for m in MODELS]
                for r in ("draft", "repair", "summarize")
            },
        }
    }
