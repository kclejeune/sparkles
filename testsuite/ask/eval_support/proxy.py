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
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from evaluator import Invalid, strict_json

LEDGER_VERSION = 2
# The most one ledger may hold, its cap and carryover together, in nanodollars. The
# person who pays sets each campaign's total, and a campaign may use several ledgers,
# each in its own directory. USD 25 bounded the first campaign of 2026-10-15 to
# 2026-10-19. The campaign of 2026-10-24 has USD 90 over ledgers of USD 50 and 30.
HARD_CAP = 50 * 10**9
MODELS = ("claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5")
REQUEST_CAP, RESPONSE_CAP = 128 * 1024, 1024 * 1024
# Integer USD nanodollars per token as input, 5 minute cache write, 1 hour cache
# write, cache read and output. A second row applies above 100000 prompt tokens.
# Source: Anthropic's model and pricing documentation of October 2026, read from the
# copy bundled with Claude Code 2.1.296 in its claude-api skill, `shared/models.md` and
# `shared/prompt-caching.md`, on 2026-10-18, not from the live pricing page. Haiku 5.5 is
# $0.10 and $0.50 per million up to 100K prompt tokens and $0.50 and $2.50 above.
# Sonnet 5.5 is $2 and $10 and Opus 5.5 is $4 and $20, both with cache reads at $0.20
# and no long-context premium. Cache writes are 1.25 times input for five minutes and
# 2 times for one hour, and a Haiku cache read is 0.1 times input. The Haiku long-context
# cache columns apply those multipliers to the higher base. A test pins this table.
RATES = {
    MODELS[0]: ((100, 125, 200, 10, 500), (500, 625, 1000, 50, 2500)),
    MODELS[1]: ((2000, 2500, 4000, 200, 10000),) * 2,
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


def amount(value):
    """A ledger amount: a nonnegative integer of nanodollars, never a bool."""
    if type(value) is not int or value < 0:
        raise Invalid("a ledger amount is a nonnegative integer")
    return value


def request_id(value):
    if type(value) is not int or value <= 0:
        raise Invalid("a request id is a positive integer")
    return value


EVIDENCE_LENGTH = (10, 1000)


def check_evidence(evidence):
    """Evidence names where a billed amount comes from, in printable text."""
    low, high = EVIDENCE_LENGTH
    if (
        not isinstance(evidence, str)
        or evidence != evidence.strip()
        or not low <= len(evidence) <= high
        or not evidence.isprintable()
    ):
        raise Invalid(
            f"a reconcile line needs evidence of {low} to {high} printable characters"
        )
    return evidence


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
    counts at its full bound. A reconcile line replaces a charge that stands at an
    upper bound, the carryover or one request, with the amount the provider billed,
    and names the evidence for that amount.
    """

    def __init__(self, path, cap, carryover=None):
        self.path = pathlib.Path(path)
        self.cap = cap
        self.mutex = threading.Lock()
        self.reserved, self.settled, self.next_id = {}, 0, 1
        self.carryover = 0
        self.carryover_name = None
        self.requests = 0
        self.over_bound = 0
        # Every settled request by id, with its case, model, bound, status and charge.
        self.charges = {}
        self.reconciled = {}  # target -> its reconcile line
        if not self.path.is_absolute() or ".." in self.path.parts:
            raise Invalid("absolute ledger path required")
        if not 0 < cap or cap + (carryover["cost"] if carryover else 0) > HARD_CAP:
            raise Invalid("the cap and the carryover together pass the hard cap")
        self.lock = self.path.with_name(self.path.name + ".lock")
        self.lock_fd = self.acquire_lock()
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
                    self.carryover = carryover["cost"]
                    self.carryover_name = carryover["name"]
            else:
                self.replay(carryover)
        except BaseException:
            self.close()
            raise

    def acquire_lock(self):
        """Take the run lock. A lock whose owner process is gone is stale and is
        replaced. A live, unreadable or foreign owner keeps it.

        Two starters that both find the same stale lock at once can each remove it and
        take a fresh one. That needs a crash and two simultaneous restarts, and is a
        known limitation."""
        for _ in range(2):
            try:
                fd = os.open(
                    self.lock,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                    0o600,
                )
            except FileExistsError:
                if not self.stale_lock():
                    raise Invalid("ledger is locked by a running evaluation") from None
                with contextlib.suppress(FileNotFoundError):
                    self.lock.unlink()
                continue
            os.write(fd, str(os.getpid()).encode())
            os.fsync(fd)
            return fd
        raise Invalid("ledger lock could not be taken")

    def stale_lock(self):
        try:
            pid = int(self.lock.read_text())
            if pid <= 0:
                return False
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        except (OSError, ValueError):
            return False
        return False

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
        if not isinstance(head, dict) or head.get("ledger") != LEDGER_VERSION:
            # Version 1 had no reconcile lines and no times, and no paid run wrote
            # one, so this reader refuses it like any other unknown version.
            raise Invalid("unknown ledger version")
        if head.get("capNanodollars") != self.cap:
            raise Invalid("the ledger has a different cap")
        seen = None
        for n, text in enumerate(lines[1:], start=1):
            line = strict_json(text)
            if not isinstance(line, dict):
                raise Invalid("unknown ledger line")
            if "carryover" in line:
                if seen is not None:
                    raise Invalid("a second carryover line")
                if n != 1:
                    raise Invalid("the carryover line must follow the header")
                if not isinstance(line["carryover"], str):
                    raise Invalid("a carryover needs a name")
                seen = {"name": line["carryover"], "cost": amount(line["cost"])}
                self.carryover += seen["cost"]
                self.carryover_name = line["carryover"]
            elif line.get("op") == "reserve":
                ident = request_id(line["id"])
                amount(line["bound"])
                if line["model"] not in RATES:
                    raise Invalid("a reserve names an unknown model")
                if ident in self.reserved or ident in self.charges:
                    raise Invalid("a reserve id repeats")
                self.reserved[ident] = line
                self.next_id = max(self.next_id, ident + 1)
            elif line.get("op") == "settle":
                ident = request_id(line["id"])
                amount(line["cost"])
                if ident not in self.reserved:
                    raise Invalid("a settle line has no reserve")
                if not isinstance(line["status"], str):
                    raise Invalid("a settle line needs a status")
                self.book(self.reserved.pop(ident), line)
            elif line.get("op") == "reconcile":
                self.apply_reconcile(line)
            else:
                raise Invalid("unknown ledger line")
        if seen != carryover:
            raise Invalid("the ledger's carryover differs from the run's")

    def book(self, reserve, settle):
        self.settled += settle["cost"]
        self.charges[reserve["id"]] = {
            "case": reserve.get("case"),
            "model": reserve["model"],
            "bound": reserve["bound"],
            "at": reserve.get("at"),
            "status": settle["status"],
            "requestId": settle.get("requestId"),
            "cost": settle["cost"],
        }

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
                "at": round(time.time(), 3),
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
            line = {
                "op": "settle",
                "overBound": over,
                "id": ident,
                "cost": amount,
                "status": status,
                "usage": usage,
                "requestId": request_id,
                "at": round(time.time(), 3),
            }
            self.write(line)
            del self.reserved[ident]
            self.book(reserve, line)

    def held_at_bound(self):
        """The charges that stand at an upper bound rather than a billed amount.

        These are the carryover, every reserve without a settle line, and every
        request that was settled at its bound because the reply was lost or its usage
        could not be read. A reconcile line takes one off this list.
        """
        out = []
        if self.carryover_name is not None and "carryover" not in self.reconciled:
            out.append(
                {
                    "target": "carryover",
                    "name": self.carryover_name,
                    "cost": self.carryover,
                }
            )
        for ident, r in sorted(self.reserved.items()):
            out.append(
                {
                    "target": ident,
                    "model": r["model"],
                    "case": r.get("case"),
                    "at": r.get("at"),
                    "status": "unsettled",
                    "requestId": None,
                    "cost": r["bound"],
                }
            )
        for ident, c in sorted(self.charges.items()):
            bounded = c["status"] == "usage-unknown" or c["status"].startswith(
                "failure:"
            )
            if bounded and c["cost"] and ident not in self.reconciled:
                out.append(
                    {
                        "target": ident,
                        "model": c["model"],
                        "case": c["case"],
                        "at": c["at"],
                        "status": c["status"],
                        "requestId": c["requestId"],
                        "cost": c["cost"],
                    }
                )
        return out

    def apply_reconcile(self, line):
        target, billed = line["target"], amount(line["cost"])
        check_evidence(line["evidence"])
        raised = line.get("raise", False)
        if type(raised) is not bool:
            raise Invalid("a reconcile line's raise flag is a boolean")
        held = {h["target"]: h for h in self.held_at_bound()}
        if target not in held:
            raise Invalid("the target is not a charge held at its bound")
        previous = held[target]["cost"]
        if line.get("previous") != previous:
            raise Invalid("the reconcile line names a different previous charge")
        if billed > previous and not raised:
            # The held charge is an upper bound, so a higher bill means the bound was
            # wrong. Recording it is allowed, but only when asked for explicitly.
            raise Invalid(
                "the billed amount is above the held charge, see --allow-raise"
            )
        if self.spent() + billed - previous > HARD_CAP:
            raise Invalid("the reconciled ledger would pass the hard cap")
        if target == "carryover":
            self.carryover += billed - previous
        elif target in self.reserved:
            self.book(
                self.reserved.pop(target), {"cost": billed, "status": "reconciled"}
            )
        else:
            self.settled += billed - previous
            self.charges[target].update(cost=billed, status="reconciled")
        self.reconciled[target] = line

    def reconcile(self, target, billed, evidence, allow_raise=False):
        """Replace a charge held at its bound with the amount the provider billed.

        The target is "carryover" or a request id from `held_at_bound()`. The
        evidence says where the amount comes from, such as the Console's cost for
        the day of an API key that only this run used. An amount above the held
        charge is refused unless `allow_raise` is set, and the line then records it.
        """
        with self.mutex:
            held = {h["target"]: h for h in self.held_at_bound()}
            if target not in held:
                raise Invalid("the target is not a charge held at its bound")
            line = {
                "op": "reconcile",
                "target": target,
                "previous": held[target]["cost"],
                "cost": billed,
                "evidence": evidence,
                "at": round(time.time(), 3),
            }
            if allow_raise:
                line["raise"] = True
            self.apply_reconcile(line)  # Every check runs before the line is written.
            self.write(line)
            return line

    def case_costs(self):
        """Nanodollars per (case, model) over every settled request of a case."""
        totals = {}
        with self.mutex:
            for c in self.charges.values():
                if c["case"]:
                    key = (c["case"], c["model"])
                    totals[key] = totals.get(key, 0) + c["cost"]
        return totals

    def report(self):
        with self.mutex:
            unresolved = sum(r["bound"] for r in self.reserved.values())
            return {
                "ledger": str(self.path),
                "ledgerVersion": LEDGER_VERSION,
                "capNanodollars": self.cap,
                "carryoverNanodollars": self.carryover,
                "settledNanodollars": self.settled,
                "unresolvedNanodollars": unresolved,
                "upperLiabilityNanodollars": self.carryover + self.settled + unresolved,
                "heldAtBound": self.held_at_bound(),
                "reconciled": list(self.reconciled.values()),
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


def require_single_ledger(path):
    """A real run keeps one ledger per directory, so a second file cannot reset the
    cap. The ledger file is the account of record for the whole spend."""
    path = pathlib.Path(path)
    others = [p for p in path.parent.glob("*.jsonl") if p != path]
    if others:
        raise Invalid("another ledger file is in the ledger directory")


def decode_body(headers, data):
    """Undo a gzip or deflate content coding, within the reply cap.

    The request asks for the identity coding, but a reply that comes back compressed
    anyway still carries the usage that settles its reserve. Another coding is left
    as it is, and the reply then settles at its bound like any unreadable reply.
    """
    codings = [v.strip().lower() for k, v in headers if k.lower() == "content-encoding"]
    if codings not in (["gzip"], ["x-gzip"], ["deflate"]):
        return data
    wbits = 16 + zlib.MAX_WBITS if codings[0] != "deflate" else zlib.MAX_WBITS
    try:
        inflater = zlib.decompressobj(wbits)
        out = inflater.decompress(data, RESPONSE_CAP + 1)
        if len(out) > RESPONSE_CAP or inflater.unconsumed_tail:
            raise UpstreamFailure("reply too large", True)
        return out
    except zlib.error:
        return data


def clean_key(raw):
    """Strip the key once at read time and refuse one that cannot be a header value.

    A secret manager can leave a trailing newline. A control character inside the key
    would make http.client raise an error that quotes the header, key included, so
    such a key is refused here with a message that does not hold it.
    """
    key = (raw or "").strip()
    if not key:
        raise Invalid("ANTHROPIC_API_KEY is not set")
    if not key.isascii() or not key.isprintable() or any(c.isspace() for c in key):
        raise Invalid("ANTHROPIC_API_KEY holds whitespace or control characters")
    return key


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
            headers = reply.getheaders()
            return reply.status, headers, decode_body(headers, data)
        except UpstreamFailure:
            raise
        except (OSError, http.client.HTTPException, ValueError, UnicodeError) as error:
            # Only the class is kept. A ValueError from a header check can quote the
            # key, so its message never leaves this function.
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
        replies=None,
    ):
        self.ledger, self.upstream, self.key = ledger, upstream, key
        # A JSON-lines file of every forwarded request and its reply, with the key
        # redacted, so a run's answers can be diagnosed without calling a model again.
        self.replies = pathlib.Path(replies) if replies else None
        self.models, self.max_output_tokens = tuple(models), max_output_tokens
        self.effort, self.timeout = effort, timeout
        self.token = secrets.token_urlsafe(32)
        self.case = None
        self.cap_refusals = 0
        self.last_headers = []
        self.request_bytes = []
        self.mutex = threading.Lock()
        self.idle = threading.Condition(self.mutex)
        self.in_flight = 0
        self.server_port = None

    def set_case(self, case):
        self.case = case

    def wait_idle(self, timeout=None):
        """Wait until no request is in flight. A child killed on a timeout can leave
        one behind, and its cost belongs to the case that sent it."""
        with self.idle:
            return self.idle.wait_for(
                lambda: self.in_flight == 0,
                self.timeout + 5 if timeout is None else timeout,
            )

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
        with self.mutex:
            self.in_flight += 1
        try:
            return self.handle_one(raw)
        finally:
            with self.idle:
                self.in_flight -= 1
                self.idle.notify_all()

    def handle_one(self, raw):
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
        return self.forward(ident, model, limit, payload, self.case)

    def forward(self, ident, model, limit, payload, case=None):
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
        except Exception as error:  # noqa: BLE001 -- the reserve must never stay open
            # Any other fault may come after the body left, so it costs the bound.
            self.ledger.settle(ident, limit, "failure:internal-" + type(error).__name__)
            return 502, [], error_body("api_error", "the proxy failed")
        self.last_headers = headers
        passed = [(k, v) for k, v in headers if k.lower() in REPLY_HEADERS]
        request_id = next((v for k, v in headers if k.lower() == "request-id"), None)
        if not 200 <= status < 300:
            # The API does not bill an error reply, which is an assumption. A gateway
            # error after generation could be billed. The cap's margin absorbs it.
            # The child's own retries see the reply.
            self.ledger.settle(ident, 0, f"http-{status}", request_id=request_id)
            self.record(
                ident, case, model, payload, status, self.clean_bytes(data), request_id
            )
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
        self.record(
            ident, case, model, payload, status, self.clean_bytes(data), request_id
        )
        if reply is None:
            return 502, [], error_body("api_error", "the upstream reply is not JSON")
        clean, _ = redact(reply, self.key)
        return status, passed, json.dumps(clean, ensure_ascii=False).encode()

    def record(self, ident, case, model, payload, status, data, request_id):
        """Append the request and its reply to the replies file, key redacted.

        The file holds whole prompts, with the dataset's grounding, and whole replies.
        It is created readable by its owner only. Any fault while recording is
        swallowed, because the request is already settled and answered.
        """
        if self.replies is None:
            return
        try:
            try:
                reply = strict_json(data)
            except Invalid:
                reply = data.decode("utf-8", "replace")[:65536]
            request, _ = redact(strict_json(payload), self.key)
            reply, _ = redact(reply, self.key)
            line = {
                "id": ident,
                "requestId": request_id,
                "case": case,
                "model": model,
                "status": status,
                "request": request,
                "reply": reply,
            }
            text = json.dumps(line, ensure_ascii=False, separators=(",", ":")) + "\n"
            with self.mutex:
                fd = os.open(
                    self.replies,
                    os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW,
                    0o600,
                )
                try:
                    os.write(fd, text.encode())
                finally:
                    os.close(fd)
        except Exception:  # noqa: BLE001, S110 -- a diagnosis record never fails a request
            pass

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
        dropped = (
            "anthropic-ratelimit",
            "anthropic-organization",
            "anthropic-workspace",
            "request-id",
            "x-api-key",
            "set-cookie",
        )
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
