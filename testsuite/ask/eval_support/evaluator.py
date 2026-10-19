"""Private initial Ask evaluator: bounded execution and strict RDF-term scoring."""

import dataclasses
import hashlib
import itertools
import json
import math
import os
import pathlib
import re
import selectors
import signal
import subprocess
import time

POLICY = "rdf-term-set-projected-columns-v2"
XSD_STRING = "http://www.w3.org/2001/XMLSchema#string"
UNBOUND = ("unbound",)
STDOUT_CAP = 4 * 1024 * 1024
STDERR_CAP = 1024 * 1024


class Invalid(ValueError):
    pass


class Unsupported(Invalid):
    pass


# A child may run this long past the timeout that it was given, so that process start,
# database open and shutdown never make the harness kill a query the server still
# allows. The evaluator and the QALD converter share the setting.
QUERY_GRACE_SECONDS = 10


class GoldFault(Invalid):
    """A gold query that failed, timed out or cannot be scored."""


class HarnessFault(Invalid):
    """A fault of the harness itself, never a measurement of the model."""


def unique_object(items):
    out = {}
    for k, v in items:
        if k in out:
            raise Invalid("duplicate JSON field")
        out[k] = v
    return out


def strict_json(raw):
    def invalid_constant(_):
        raise Invalid("nonfinite JSON number")

    def finite_float(text):
        value = float(text)
        if not math.isfinite(value):
            raise Invalid("nonfinite JSON number")
        return value

    try:
        return json.loads(
            raw,
            object_pairs_hook=unique_object,
            parse_constant=invalid_constant,
            parse_float=finite_float,
        )
    except (ValueError, UnicodeError, RecursionError) as e:
        raise Invalid("invalid JSON") from e


def digest(path):
    h = hashlib.sha256()
    with pathlib.Path(path).open("rb") as f:
        for b in iter(lambda: f.read(65536), b""):
            h.update(b)
    return h.hexdigest()


def bounded_file(path, cap):
    with pathlib.Path(path).open("rb") as f:
        b = f.read(cap + 1)
    if len(b) > cap:
        raise Invalid("file byte cap")
    return b


def finite_positive(value):
    if (
        isinstance(value, bool)
        or not isinstance(value, (float, int))
        or not math.isfinite(value)
        or value <= 0
    ):
        raise Invalid("limit must be finite and positive")
    return value


class OfflinePolicy(Invalid):
    pass


def offline_query(text):
    """Conservative lexical SERVICE refusal before the read-only CLI is spawned.

    Quoted data, comments, variables and complete prefixed names are not keywords.
    Uncertain escapes/unterminated strings are excluded rather than executed.
    """
    if not isinstance(text, str) or "\x00" in text:
        raise OfflinePolicy("unsupported offline query text")
    if re.search(r"\\[uU]", text):
        raise OfflinePolicy("Unicode escapes are excluded from offline scoring")
    i = 0
    while i < len(text):
        c = text[i]
        if c == "#":
            while i < len(text) and text[i] not in "\r\n":
                i += 1
            if text.startswith("\r\n", i):
                i += 2
            elif i < len(text):
                i += 1
            continue
        if c in ('"', "'"):
            marker = c * 3 if text.startswith(c * 3, i) else c
            i += len(marker)
            while i < len(text) and not text.startswith(marker, i):
                i += 2 if text[i] == "\\" else 1
            if i >= len(text):
                raise OfflinePolicy("uncertain offline query string")
            i += len(marker)
            continue
        if c == "<":
            iri = re.match(r'<[^<>"{}|^`\\\x00-\x20]*>', text[i:])
            if iri:
                i += len(iri[0])
                continue
        if c in ("?", "$"):
            i += 1
            while i < len(text) and (text[i].isalnum() or text[i] == "_"):
                i += 1
            continue
        if c.isalpha() or c in ("_", ":"):
            j = i + 1
            while j < len(text) and (text[j].isalnum() or text[j] in "_-.:%"):
                j += 1
            if text[i:j].upper() == "SERVICE":
                raise OfflinePolicy("SERVICE is excluded from offline scoring")
            i = j
            continue
        if c == "\\":
            raise OfflinePolicy("uncertain offline query escape")
        i += 1
    return text


def norm_term(term):
    if (
        not isinstance(term, dict)
        or not {"type", "value"} <= term.keys()
        or term.keys() - {"type", "value", "datatype", "xml:lang"}
    ):
        raise Invalid("invalid RDF term")
    kind, value = term["type"], term["value"]
    if not isinstance(value, str):
        raise Invalid("term value must be a string")
    if kind == "bnode":
        raise Unsupported("blank-node scoring is not supported by this policy")
    if kind == "uri":
        if set(term) != {"type", "value"}:
            raise Invalid("URI term attributes")
        return ("uri", value)
    if kind not in ("literal", "typed-literal"):
        raise Invalid("unknown RDF term kind")
    lang, datatype = term.get("xml:lang"), term.get("datatype")
    if lang is not None:
        if (
            datatype is not None
            or not isinstance(lang, str)
            or not re.fullmatch(r"[A-Za-z]+(?:-[A-Za-z0-9]+)*", lang)
        ):
            raise Invalid("invalid language tag")
        return ("lang", value, lang.lower())
    if datatype is not None and (not isinstance(datatype, str) or not datatype):
        raise Invalid("invalid datatype")
    if kind == "typed-literal" and datatype is None:
        raise Invalid("missing datatype")
    return ("literal", value, datatype or XSD_STRING)


@dataclasses.dataclass(eq=False)
class Relation:
    variables: tuple
    rows: frozenset
    duplicates: int

    def __eq__(self, other):
        if not isinstance(other, Relation):
            return False
        if len(self.variables) != len(other.variables) or len(self.rows) != len(
            other.rows
        ):
            return False
        return align(self, other)[0] == len(self.rows)


def answer_set(result):
    if not isinstance(result, dict) or result.keys() - {"head", "boolean", "results"}:
        raise Invalid("SPARQL JSON result required")
    head = result.get("head")
    if not isinstance(head, dict) or head.keys() - {"vars", "link"}:
        raise Invalid("result head required")
    if "link" in head and (
        not isinstance(head["link"], list)
        or any(not isinstance(v, str) for v in head["link"])
    ):
        raise Invalid("invalid head links")
    if "boolean" in result:
        if (
            type(result["boolean"]) is not bool
            or "results" in result
            or head.get("vars", []) != []
        ):
            raise Invalid("invalid ASK result")
        return result["boolean"]
    variables = head.get("vars")
    if (
        not isinstance(variables, list)
        or any(not isinstance(v, str) or not v for v in variables)
        or len(set(variables)) != len(variables)
    ):
        raise Invalid("invalid SELECT variables")
    if len(variables) > 8:
        raise Unsupported("scoring supports at most eight columns")
    results = result.get("results")
    if (
        not isinstance(results, dict)
        or set(results) != {"bindings"}
        or not isinstance(results["bindings"], list)
    ):
        raise Invalid("complete bindings required")
    if len(results["bindings"]) > 1000:
        raise Unsupported("scoring supports at most 1000 rows")
    rows = []
    for row in results["bindings"]:
        if not isinstance(row, dict) or row.keys() - set(variables):
            raise Invalid("undeclared binding")
        rows.append(
            tuple(norm_term(row[v]) if v in row else UNBOUND for v in variables)
        )
    return Relation(tuple(variables), frozenset(rows), len(rows) - len(set(rows)))


def align(gold, got, check=lambda: None):
    """One relation-wide column mapping; never a row-specific value permutation."""
    if len(gold.variables) != len(got.variables):
        return 0, None
    width = len(gold.variables)
    # Term-kind signatures bound candidates without assuming matching lexical values.
    gs = [{cell[i][0] for cell in gold.rows} for i in range(width)]
    vs = [{cell[i][0] for cell in got.rows} for i in range(width)]
    best, mapping, visited = -1, None, 0
    for p in itertools.permutations(range(width)):
        check()
        # Disjoint signatures cannot improve a positive intersection.
        if (
            gold.rows
            and got.rows
            and any(gs[i].isdisjoint(vs[p[i]]) for i in range(width))
        ):
            continue
        converted = set()
        for row in got.rows:
            visited += width or 1
            if visited > 1000000:
                raise Unsupported("global column comparison cell cap")
            check()
            converted.add(tuple(row[i] for i in p))
        common = len(gold.rows & converted)
        if common > best:
            best, mapping = common, p
        if common == min(len(gold.rows), len(got.rows)):
            break
    return max(best, 0), mapping


def project(gold, got, check=lambda: None):
    """The best injective choice of one answer column for each gold column.

    An answer may project more columns than the gold, such as a name beside each
    person. Its rows are then projected onto the chosen columns as a set, and that
    set is compared with the gold rows. A column is a candidate for a gold column only
    when they share a term, unless either side has no rows. Returns the intersection,
    the chosen columns and the projected rows.
    """
    gw, aw = len(gold.variables), len(got.variables)
    if gw > aw:
        return 0, None, frozenset()
    gold_values = [{row[i] for row in gold.rows} for i in range(gw)]
    got_values = [{row[j] for row in got.rows} for j in range(aw)]
    candidates = [
        [
            j
            for j in range(aw)
            if not gold.rows
            or not got.rows
            or not gold_values[i].isdisjoint(got_values[j])
        ]
        for i in range(gw)
    ]
    best = (-1, None, frozenset())
    visited = 0
    for cols in itertools.product(*candidates):
        check()
        if len(set(cols)) < gw:
            continue
        visited += (len(got.rows) or 1) * (gw or 1)
        if visited > 1000000:
            raise Unsupported("projected column comparison cell cap")
        projected = frozenset(tuple(row[j] for j in cols) for row in got.rows)
        common = len(gold.rows & projected)
        # Prefer more shared rows, then fewer stray rows.
        if (common, -len(projected)) > (best[0], -len(best[2])):
            best = (common, cols, projected)
        if common == len(gold.rows) == len(projected):
            break
    return max(best[0], 0), best[1], best[2]


def score_sets(gold, got, check=lambda: None):
    if type(gold) is bool:
        ok = type(gold) is bool and type(got) is bool and gold == got
        return ok, (float(ok),) * 3, {"kind": "ask", "match": ok}
    if type(got) is bool:
        return (
            False,
            (0.0, 0.0, 0.0),
            {
                "kind": "select",
                "intersection": 0,
                "goldRows": len(gold.rows),
                "gotRows": 0,
                "status": "wrong_result_kind",
            },
        )
    if len(gold.variables) > len(got.variables):
        return (
            False,
            (0.0, 0.0, 0.0),
            {
                "kind": "select",
                "intersection": 0,
                "goldRows": len(gold.rows),
                "gotRows": len(got.rows),
                "mapping": None,
                "goldDuplicates": gold.duplicates,
                "gotDuplicates": got.duplicates,
                "status": "missing_columns",
            },
        )
    if len(gold.variables) < len(got.variables):
        n, mapping, projected = project(gold, got, check)
        g, a = len(gold.rows), len(projected)
        p = n / a if a else float(g == 0)
        r = n / g if g else float(a == 0)
        f = 2 * p * r / (p + r) if p + r else 0.0
        return (
            (g == a == n),
            (p, r, f),
            {
                "kind": "select",
                "intersection": n,
                "goldRows": g,
                "gotRows": len(got.rows),
                "projectedRows": a,
                "mapping": list(mapping) if mapping is not None else None,
                "extraColumns": len(got.variables) - len(gold.variables),
                "goldDuplicates": gold.duplicates,
                "gotDuplicates": got.duplicates,
            },
        )
    n, mapping = align(gold, got, check)
    g, a = len(gold.rows), len(got.rows)
    p = n / a if a else float(g == 0)
    r = n / g if g else float(a == 0)
    f = 2 * p * r / (p + r) if p + r else 0.0
    return (
        (g == a == n),
        (p, r, f),
        {
            "kind": "select",
            "intersection": n,
            "goldRows": g,
            "gotRows": a,
            "mapping": mapping,
            "goldDuplicates": gold.duplicates,
            "gotDuplicates": got.duplicates,
        },
    )


def prf(gold, got):
    return score_sets(gold, got)[1]


def leaf_environment(extra=None):
    # Never copy ambient credential, proxy, loader or fnox variables into children.
    env = {
        "PATH": "/usr/bin:/bin",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "RAYON_NUM_THREADS": "1",
    }
    if extra:
        if set(extra) != {"SPARKLES_EVAL_GATE_TOKEN"} or not isinstance(
            extra["SPARKLES_EVAL_GATE_TOKEN"], str
        ):
            raise Invalid("invalid leaf environment")
        env.update(extra)
    return env


def validate_ask_record(value):
    if not isinstance(value, dict):
        raise Invalid("final Ask object required")
    for field in ("result", "usage", "error"):
        if value.get(field) is not None and not isinstance(value[field], dict):
            raise Invalid("invalid Ask object field")
    result = value.get("result") or {}
    if "query" in result and not isinstance(result["query"], str):
        raise Invalid("invalid Ask query")
    if "attempt" in result and (
        type(result["attempt"]) is not int or result["attempt"] < 0
    ):
        raise Invalid("invalid Ask attempt")
    if result.get("issues") is not None and (
        not isinstance(result["issues"], list)
        or any(not isinstance(i, dict) for i in result["issues"])
    ):
        raise Invalid("invalid final Ask issues")
    attempts = value.get("attempts", [])
    if not isinstance(attempts, list) or any(not isinstance(a, dict) for a in attempts):
        raise Invalid("invalid Ask attempts")
    for attempt in attempts:
        if "query" in attempt and not isinstance(attempt["query"], str):
            raise Invalid("invalid retained query")
        check = attempt.get("check")
        if check is not None and (
            not isinstance(check, dict)
            or not isinstance(check.get("issues", []), list)
            or any(not isinstance(x, dict) for x in check.get("issues", []))
        ):
            raise Invalid("invalid Ask check")
    usage = value.get("usage") or {}
    if "steps" in usage and (
        not isinstance(usage["steps"], list)
        or any(not isinstance(s, dict) for s in usage["steps"])
    ):
        raise Invalid("invalid Ask steps")
    for field in ("inputTokens", "outputTokens"):
        if field in usage and (
            type(usage[field]) is not int or not 0 <= usage[field] < 2**64
        ):
            raise Invalid("invalid reported token count")
    cost = usage.get("estimatedCost")
    if cost is not None and (
        type(cost) not in (int, float) or not math.isfinite(cost) or cost < 0
    ):
        raise Invalid("invalid estimated cost")
    return value


def group_states(pgid):
    """Only identity/state fields; unreadable census is not proof of cleanup.

    A process can exit between the open and the read of its stat file, which raises
    FileNotFoundError or ProcessLookupError, and a stat line can be torn while it
    exits. Such an entry is skipped, because the process is gone or going.
    """
    found = []
    for path in pathlib.Path("/proc").glob("[0-9]*/stat"):
        try:
            raw = path.read_text()
        except (FileNotFoundError, ProcessLookupError):
            continue
        fields = raw[raw.rfind(")") + 2 :].split()
        if len(fields) < 4:
            continue
        if int(fields[2]) == pgid:
            found.append({"pid": int(path.parent.name), "state": fields[0]})
    return found


def capture_child(
    argv, directory, deadline, env=None, stdout_cap=STDOUT_CAP, stderr_cap=STDERR_CAP
):
    """Incremental caps, actual status, and cleanup of this newly owned group only."""
    directory = pathlib.Path(directory)
    directory.mkdir(mode=0o700)
    started = time.monotonic()
    if deadline <= started:
        raise HarnessFault("phase deadline expired before spawn")
    record = {
        "schema": 2,
        "argv": argv,
        "startedMonotonic": started,
        "timeout": False,
        "byteCap": None,
        "returncode": None,
        "cleanup": False,
    }
    try:
        proc = subprocess.Popen(
            argv,
            env=env or leaf_environment(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as error:
        record["spawnError"] = type(error).__name__
        record["elapsedSeconds"] = time.monotonic() - started
        (directory / "status.json").write_text(json.dumps(record, indent=2) + "\n")
        return record
    record["pid"] = record["pgid"] = proc.pid
    sel = selectors.DefaultSelector()
    files, counts = {}, {"stdout": 0, "stderr": 0}
    caps = {"stdout": stdout_cap, "stderr": stderr_cap}
    for name, pipe in [("stdout", proc.stdout), ("stderr", proc.stderr)]:
        os.set_blocking(pipe.fileno(), False)
        sel.register(pipe, selectors.EVENT_READ, name)
        files[name] = (directory / name).open("xb")
    try:
        while sel.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                record["timeout"] = True
                break
            for key, _ in sel.select(min(remaining, 0.05)):
                name = key.data
                b = os.read(
                    key.fileobj.fileno(), min(65536, caps[name] - counts[name] + 1)
                )
                if not b:
                    sel.unregister(key.fileobj)
                    continue
                allowed = min(len(b), caps[name] - counts[name])
                files[name].write(b[:allowed])
                counts[name] += allowed
                if allowed < len(b):
                    record["byteCap"] = name
                    break
            if record["byteCap"]:
                break
        # Keep the leader unreaped until all group signals are complete.
        while not (record["timeout"] or record["byteCap"]):
            status = os.waitid(os.P_PID, proc.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if status is not None:
                break
            if time.monotonic() >= deadline:
                record["timeout"] = True
                break
            time.sleep(0.005)
    finally:
        # The leader PID is still owned (alive or zombie); its group cannot be reused.
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(proc.pid, sig)
            except ProcessLookupError:
                pass
        # Keep the unreaped anchor until the last observation and signal. Zombies
        # are dead, and are distinguished from a surviving or unknown process.
        until = time.monotonic() + 2
        record["remainingGroupStates"] = None
        try:
            while True:
                states = group_states(proc.pid)
                record["remainingGroupStates"] = states
                if not any(s["pid"] == proc.pid for s in states):
                    break  # Missing procfs/anchor is unknown, never absence proof.
                if all(s["state"] in ("Z", "X") for s in states):
                    record["cleanup"] = True
                    break
                if time.monotonic() >= until:
                    break
                time.sleep(0.005)
        except (OSError, ValueError):
            record["cleanup"] = False
        try:
            record["returncode"] = proc.wait(
                timeout=max(0.001, until - time.monotonic())
            )
        except subprocess.TimeoutExpired:
            record["cleanup"] = False
        record["elapsedSeconds"] = time.monotonic() - started
        record["bytes"] = counts
        sel.close()
        for f in files.values():
            f.close()
        proc.stdout.close()
        proc.stderr.close()
        record["sha256"] = {name: digest(directory / name) for name in files}
        (directory / "status.json").write_text(json.dumps(record, indent=2) + "\n")
    return record


def parse_child(record, directory):
    if (
        record["returncode"] != 0
        or record["timeout"]
        or record["byteCap"]
        or not record["cleanup"]
    ):
        raise Invalid("child execution incomplete")
    return strict_json(bounded_file(pathlib.Path(directory) / "stdout", STDOUT_CAP))
