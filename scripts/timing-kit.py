#!/usr/bin/env python3
"""A kit for interleaved A/B timing runs on a quiet Intel host.

    scripts/timing-kit.py stage --out DIR --base LABEL=BIN --tip LABEL=BIN [--tip ...] [options]
    scripts/timing-kit.py preflight [--host-side] STAGE...
    scripts/timing-kit.py launch --host HOST --unit NAME [--dry-run] STAGE...
    scripts/timing-kit.py run STAGE...          (the unit's driver, on the timing host)
    scripts/timing-kit.py summarize STAGE
    scripts/timing-kit.py shadow [--pair X,Y] [--controls Q,...] [--flag-ratio R] [--rules R,...] PASS_DIR

`stage` assembles one run directory for scripts/nsc-bench-remote.sh. The base binary is
variant A and each tip is B, C and so on, so that several tips that share a base, data,
queries and settings are timed in one interleaved run and the base's server processes
serve every pair. Each round runs the palindrome A B ... ... B A, so that a steady drift
of the host's speed falls evenly on every variant. The directory holds hard links to the
binaries and data, the queries, run.env, kit.json, binaries.sha256, the Nix runtime paths
of the binaries and copies of the harness, the summary and this kit.

`preflight` refuses a stage whose setup cannot give a decision, before the host is booked.
It checks that the server and the client have their own explicit CPUs and that the busy
loops that keep those CPUs awake are on, that every round runs every variant at least
twice so that the same-binary spread can be judged, that the controls (queries of at least
0.3 ms that carry the validity check) are present, that both variants of each pair use the
same request client, and that the binaries match their hashes. With --host-side it also
checks the host: the CPUs are online, they are not SMT siblings of each other, they are of
the host's fastest core type, the port is free, the lock files exist, the runtime paths
exist, python3 exists when a variant uses the persistent client, and MBX_REMOTE_URL is
empty.

`launch` runs the stage's preflight, copies each stage to HOST under ~/lanes/NAME while it
holds the host's locks, runs the host-side preflight there, and queues the systemd user
unit NAME. The unit takes ~/lanes/.bench.lock and then ~/lanes/.heavy.lock, runs under a
12 GiB memory cap with MBX_REMOTE_URL cleared, and runs `timing-kit.py run` on the stages
in order. --dry-run prints the commands instead of running them.

`run` times each stage in turn. A first pass runs every query. The queries whose ratio of
execution times is at FLAG_RATIO or more in any pair then run again with RERUN_ROUNDS
rounds of RERUN_REPS requests. Each pair gets its own summary and validity from
nsc-bench-summary.py --pair, and verdict.md lists them. A pass is invalid when its host
was not quiet, when a query lacks timings, or when the two arms of a pair answer a query
differently, fail its answer check or lack an answer to it, or when any arm of the run
answers a query differently from the others, and verdict.md names the queries whose
answers stopped it. EXPECTED_ANSWER_CHANGE in run.env, a comma-separated list of queries
that stage --expected-answer-change writes with the reason that EXPECTED_ANSWER_CHANGE_REASON
holds, lets the binaries answer those queries differently, and verdict.md names the change
and its reason in place of the refusal. DONE gets the exit status, which is 3 when a pass that
decides is invalid.

`shadow` replays the early-stopping rule on a finished pass, whose every round ran, and
records what the rule would have decided beside the verdict of all rounds. A query that is
not a control stops only once every question the stop must answer has stopped on both its
execution time and its wall time, since the gates score both, and each decision is then
taken on the rounds the query keeps. For the flag at the bound, a metric stops once a t
interval at 99.5% of its per-round log ratios, after at least three rounds, lies below the
bound with the pooled ratio so far below it too, or lies at or above the bound. With
EARLY_STOP_RULES=every-round, or bound,every-round, the stop also answers whether every round
is above 1 with more than 0.5 us added, which an interval cannot answer, so that metric stops
exactly, at its first round at or below 1, or runs every round. The controls run every round and carry the host's validity check,
the pass's checks of answers, timings and server processes apply to both alike, and
when their same-binary spread is 2.2% or more but within the 3% bound, the stopped queries
would run their remaining rounds and the rule of all rounds would decide. PASS_DIR/shadow-X-Y.json
gets each query's stop round, each metric's and question's stop and decision beside that of
all rounds, the validity of both, the share of requests the rule would have sent and a hash
of samples.tsv. A run agrees when every decision and the validity are the same. With EARLY_STOP=shadow in run.env, which `stage`
writes by default, `run` does this for each pair of the first pass and writes shadow.md.
Shadow mode never changes verdict.md, flagged.txt, the rerun or the exit status, and it
cannot show what live stopping would change, since stopped queries would no longer sit
between the others and the remaining rounds of a fallback would run in new server processes.
"""

import argparse
import csv
import importlib.util
import json
import math
import os
import re
import shlex
import shutil
import signal
import socket
import statistics
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
LETTERS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
# Queries of the screen that take at least 0.3 ms on the Intel hosts and keep their rounds
# whatever a selection or a stopping rule does, so that they carry the validity check.
CONTROLS = ("expr-bind-group range-topk not-exists class-predicates exists-join star-join optional-chain "
            "two-hop-count group-avg knows-reach").split()
MIN_CONTROLS = 3
KIT_FILES = ("nsc-bench-remote.sh", "nsc-bench-summary.py", "timing-kit.py")
# the early-stopping rule that the replay of 75 Intel runs chose, which shadow mode records
EARLY_KMIN = 3
EARLY_CONF = 0.995
FALLBACK_SPREAD = 0.022
# the metrics a stop must cover, since the gates score both, and the questions it can answer:
# "bound" is the flag at FLAG_RATIO, and "every-round" is a cost in every round, above 1 in
# each and adding more than EVERY_ROUND_ADDED_US, which only the exact stop answers
STOP_METRICS = ("exec_ms", "wall_ms")
STOP_RULES = ("bound", "every-round")
EVERY_ROUND_ADDED_US = 0.5
# two-sided 99.5% t quantiles for 1 to 30 degrees of freedom; more degrees use the last,
# which is above the true quantile and so stops later
T995 = [127.3, 14.09, 7.453, 5.598, 4.773, 4.317, 4.029, 3.833, 3.690, 3.581, 3.497, 3.428, 3.372, 3.326, 3.286,
        3.252, 3.222, 3.197, 3.174, 3.153, 3.135, 3.119, 3.104, 3.091, 3.078, 3.067, 3.057, 3.047, 3.038, 3.030]


def die(msg):
    sys.exit(f"timing-kit: {msg}")


# The harness sources run.env with bash. The kit reads only plain assignments whose value
# is a bare word or one quoted string without expansions, so that what it checks is what
# bash sets, and it refuses any other line.
ASSIGN = re.compile(r"""([A-Z_][A-Z0-9_]*)=(?:([^\s'"$`\\;&|<>()~]*)|'([^']*)'|"([^"$`\\]*)")""")


def read_env(path, problems=None):
    """The assignments of a run.env. A line the kit cannot read exactly goes to problems,
    or ends the program when no list is given."""
    env = {}
    for n, line in enumerate(Path(path).read_text().splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        m = ASSIGN.fullmatch(line)
        if not m:
            msg = f"{path}:{n} is not a plain KEY=VALUE assignment: {line}"
            if problems is None:
                die(msg)
            problems.append(msg)
            continue
        env[m.group(1)] = next(g for g in m.groups()[1:] if g is not None)
    return env


def runtime_paths(binaries):
    """The Nix store paths of each binary's interpreter and run paths."""
    seen = []
    for b in binaries:
        try:
            out = subprocess.run(["readelf", "-lW", "-d", str(b)], capture_output=True, text=True).stdout
        except FileNotFoundError:
            die("readelf is missing, and the stage needs it to find the binaries' runtime paths")
        interp = re.search(r"Requesting program interpreter: (\S+)\]", out)
        dirs = [interp.group(1)] if interp else []
        for m in re.finditer(r"\((?:RUNPATH|RPATH)\)\s+Library r(?:un)?path: \[([^\]]*)\]", out):
            dirs += m.group(1).split(":")
        for d in dirs:
            m = re.match(r"(/nix/store/[a-z0-9]{32}-[^/]+)", d)
            if m and m.group(1) not in seen:
                seen.append(m.group(1))
    return seen


def needs_loader(binary):
    """Whether a binary names a loader outside the Nix store, which the harness then runs it
    under (its DYNAMIC setting). A binary with its own Nix loader, a static one and a script
    run as they are."""
    with open(binary, "rb") as f:
        if f.read(4) != b"\x7fELF":
            return False
    out = subprocess.run(["readelf", "-lW", str(binary)], capture_output=True, text=True).stdout
    m = re.search(r"Requesting program interpreter: (\S+)\]", out)
    return bool(m and not m.group(1).startswith("/nix/store/"))


def bench_queries(bench_sh, skip=("export-500k",)):
    """The screen's queries, as scripts/bench.sh defines them, with its prefix."""
    text = Path(bench_sh).read_text()
    m = re.search(r"^P='(.*)'$", text, re.M)
    prefix = m.group(1) if m else ""
    out = {}
    for m in re.finditer(r"^add ([a-z0-9-]*) '(.*)'$", text, re.M):
        if m.group(1) not in skip:
            out[m.group(1)] = prefix + m.group(2)
    return out


def link(src, dst):
    try:
        os.link(src, dst)
    except OSError:
        shutil.copy2(src, dst)


def sha256(path):
    import hashlib
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------------------- stage

def cmd_stage(a):
    out = Path(a.out)
    if out.exists():
        die(f"{out} exists")
    arms = [a.base] + a.tip
    if not a.tip:
        die("name at least one --tip")
    if len(arms) > len(LETTERS):
        die("too many tips")
    if a.expected_answer_change and not a.expected_answer_change_reason.strip():
        die("--expected-answer-change needs --expected-answer-change-reason, which says why the answers change")
    if re.search(r"['\n\t\\]", a.expected_answer_change_reason):
        die("the expected-answer-change reason must be one line without quotes, tabs or backslashes")
    labels, bins = [], []
    for spec in arms:
        label, _, path = spec.partition("=")
        if not label or not path or not Path(path).is_file():
            die(f"an arm is LABEL=BINARY with an existing binary, not {spec}")
        if label in labels:
            die(f"the label {label} is named twice")
        labels.append(label)
        bins.append(Path(path))
    out.mkdir(parents=True)
    (out / "queries").mkdir()
    if a.queries:
        src = sorted(Path(a.queries).glob("*.rq"))
        for f in src:
            shutil.copy(f, out / "queries" / f.name)
    else:
        for name, text in bench_queries(a.bench_sh).items():
            (out / "queries" / f"{name}.rq").write_text(text)
    if a.only:
        keep = set(a.only.split(","))
        for f in (out / "queries").glob("*.rq"):
            if f.stem not in keep:
                f.unlink()
    configs = []
    for c in (a.clients.split(",") if a.clients else [a.client + (f":{a.gap_ms}" if a.client == "persistent" else "")]):
        name, _, gap = c.partition(":")
        if name not in ("curl", "persistent") or (gap and not re.fullmatch(r"[0-9]+(\.[0-9]+)?", gap)):
            die(f"a client is curl or persistent[:GAP_MS], not {c}")
        configs.append((name, gap or "0"))
    if len(arms) * len(configs) > len(LETTERS):
        die("too many tips and clients")
    own = LETTERS[:len(arms)]
    for v, b in zip(own, bins):
        link(b, out / f"bin-{v}")
    # with several clients, each client times every arm, and the arms of the first client own
    # the binaries and stores
    variants = LETTERS[:len(arms) * len(configs)]
    group = [variants[i * len(arms):(i + 1) * len(arms)] for i in range(len(configs))]
    link(a.data, out / "data.nt.zst")
    for f in KIT_FILES:
        shutil.copy(HERE / f, out / f)
    (out / "binaries.sha256").write_text("".join(f"{sha256(out / f'bin-{v}')}  bin-{v}\n" for v in own))
    (out / "runtime-paths.txt").write_text("\n".join(runtime_paths(out / f"bin-{v}" for v in own)) + "\n")
    seq = list(variants)
    orders = [" ".join(seq + seq[::-1])]
    if a.rotate:
        orders = [" ".join(seq[i:] + seq[:i] + (seq[i:] + seq[:i])[::-1]) for i in range(len(seq))]
    controls = [q for q in (a.controls.split(",") if a.controls else CONTROLS) if (out / "queries" / f"{q}.rq").exists()]
    mapping, pairs, xpairs = {}, [], []
    for i, (g, (name, gap)) in enumerate(zip(group, configs)):
        for j, v in enumerate(g):
            mapping[f"REQUEST_CLIENT_{v}"] = name
            mapping[f"REQUEST_GAP_MS_{v}"] = gap
            if i:
                mapping[f"BINARY_OF_{v}"] = own[j]
                mapping[f"STORE_OF_{v}"] = own[j]
                xpairs.append(f"{own[j]},{v}")
        pairs += [f"{g[0]},{v}" for v in g[1:]]
    env = {
        **{f"DYNAMIC_{v}": "1" if needs_loader(out / f"bin-{v}") else "0" for v in own},
        **mapping,
        "VARIANTS": " ".join(variants), "ORDERS": ";".join(orders),
        "ROUNDS": a.rounds, "REPS": a.reps, "WARMUP": a.warmup, "RAYON_THREADS": a.rayon_threads,
        "SERVER_CPU": a.server_cpu, "CLIENT_CPU": a.client_cpu, "PORT": a.port, "SPIN": 1, "SPIN_LOOP": a.spin_loop,
        "SERVE_ARGS": a.serve_args, "EXECUTION": a.execution,
        "PAIRS": " ".join(pairs), "XPAIRS": " ".join(xpairs), "CONTROLS": " ".join(controls),
        "FLAG_RATIO": a.flag_ratio, "RERUN_ROUNDS": a.rerun_rounds, "RERUN_REPS": a.rerun_reps,
        "EARLY_STOP": a.early_stop, "EARLY_STOP_RULES": a.early_stop_rules,
    }
    if a.expected_answer_change:
        env["EXPECTED_ANSWER_CHANGE"] = a.expected_answer_change
        env["EXPECTED_ANSWER_CHANGE_REASON"] = a.expected_answer_change_reason
    (out / "run.env").write_text("".join(f"{k}={shlex.quote(str(v))}\n" for k, v in env.items()))
    kit = {"arms": [{"variant": v, "label": l + (f" [{name}{'' if name == 'curl' else ' gap ' + gap + ' ms'}]" if len(configs) > 1 else ""),
                     "source": str(b.resolve()), "sha256": sha256(out / f"bin-{own[j]}")}
                    for g, (name, gap) in zip(group, configs) for j, (v, l, b) in enumerate(zip(g, labels, bins))],
           "data": str(Path(a.data).resolve()), "note": a.note}
    (out / "kit.json").write_text(json.dumps(kit, indent=1) + "\n")
    problems = static_checks(out)
    for p in problems:
        print(f"refused: {p}", file=sys.stderr)
    print(f"staged {out}: {len(list((out / 'queries').glob('*.rq')))} queries, variants "
          + ", ".join(f"{arm['variant']}={arm['label']}" for arm in kit["arms"]))
    return 1 if problems else 0


# ------------------------------------------------------------------------ preflight

def cpu_list(text):
    out = set()
    for part in text.strip().split(","):
        if not part:
            continue
        lo, _, hi = part.partition("-")
        out.update(range(int(lo), int(hi or lo) + 1))
    return out


def static_checks(stage):
    """What is wrong with a stage, judged from its files alone."""
    stage = Path(stage)
    p = []
    if not (stage / "run.env").is_file():
        return [f"{stage} has no run.env"]
    env = read_env(stage / "run.env", p)
    for k in ("SERVER_CPU", "CLIENT_CPU"):
        if not re.fullmatch(r"[0-9]+", env.get(k, "")):
            p.append(f"{k} must name one CPU explicitly, not {env.get(k, 'nothing')}")
    if env.get("SERVER_CPU") and env.get("SERVER_CPU") == env.get("CLIENT_CPU"):
        p.append("the server and the client share a CPU")
    if env.get("SPIN", "1") != "1":
        p.append("SPIN must be 1, so that the timing CPUs do not sleep between requests")
    if env.get("SPIN_LOOP", "user") not in ("user", "syscall"):
        p.append(f"SPIN_LOOP must be user or syscall, not {env.get('SPIN_LOOP')}")
    variants = env.get("VARIANTS", "A B").split()
    orders = [o.split() for o in env.get("ORDERS", "A B B A").split(";")]
    for o in orders:
        for v in variants:
            if o.count(v) < 2:
                p.append(f"the order '{' '.join(o)}' runs {v} {o.count(v)} time(s) a round, "
                         "and the same-binary spread needs two server processes of each variant a round")
        if o != o[::-1]:
            p.append(f"the order '{' '.join(o)}' is not a palindrome, so a steady drift favours one variant")
    for k, default, least in (("ROUNDS", "4", 2), ("REPS", "20", 1), ("WARMUP", "3", 0),
                              ("RERUN_ROUNDS", "16", 2), ("RERUN_REPS", "40", 1)):
        v = env.get(k, default)
        if not re.fullmatch(r"[0-9]+", v) or int(v) < least:
            p.append(f"{k} must be a whole number of at least {least}, not {v}")
    if env.get("EARLY_STOP", "off") not in ("off", "shadow"):
        p.append(f"EARLY_STOP must be off or shadow, not {env.get('EARLY_STOP')}, since stopping that acts is not qualified")
    rules = env.get("EARLY_STOP_RULES", "bound").split(",")
    if not all(r in STOP_RULES for r in rules):
        p.append(f"EARLY_STOP_RULES must be a comma-separated list of {', '.join(STOP_RULES)}, not {env.get('EARLY_STOP_RULES')}")
    if not re.fullmatch(r"[0-9]+(\.[0-9]+)?", env.get("FLAG_RATIO", "1.10")) or float(env.get("FLAG_RATIO", "1.10")) <= 1:
        p.append(f"FLAG_RATIO must be a ratio above 1, not {env.get('FLAG_RATIO')}")
    queries = {f.stem for f in (stage / "queries").glob("*.rq")} if (stage / "queries").is_dir() else set()
    if not queries:
        p.append("the stage has no queries")
    if "EXPECTED_ANSWER_CHANGE" in env or "EXPECTED_ANSWER_CHANGE_REASON" in env:
        change = env.get("EXPECTED_ANSWER_CHANGE", "")
        if not change:
            p.append("EXPECTED_ANSWER_CHANGE names no queries")
        elif not re.fullmatch(r"[A-Za-z0-9_.-]+(,[A-Za-z0-9_.-]+)*", change):
            p.append(f"EXPECTED_ANSWER_CHANGE must be a comma-separated list of query names, not {change}")
        else:
            p += [f"{q} is not a staged query, so EXPECTED_ANSWER_CHANGE cannot name it"
                  for q in change.split(",") if q not in queries]
        if change and not env.get("EXPECTED_ANSWER_CHANGE_REASON", "").strip():
            p.append("EXPECTED_ANSWER_CHANGE gives no reason in EXPECTED_ANSWER_CHANGE_REASON")
    controls = env.get("CONTROLS", " ".join(CONTROLS)).split()
    present = [q for q in controls if q in queries]
    if len(present) < MIN_CONTROLS:
        p.append(f"only {len(present)} control queries of at least 0.3 ms are staged ({', '.join(present) or 'none'}), "
                 f"and validity needs {MIN_CONTROLS}")
    clients = {}
    for v in variants:
        clients[v] = (env.get(f"REQUEST_CLIENT_{v}") or env.get("REQUEST_CLIENT") or "curl",
                      env.get(f"REQUEST_GAP_MS_{v}") or env.get("REQUEST_GAP_MS") or "0")
        if clients[v][0] not in ("curl", "persistent"):
            p.append(f"{v}'s request client is {clients[v][0]}")
    for pair in env.get("PAIRS", "A,B").split():
        x, _, y = pair.partition(",")
        if x not in variants or y not in variants or x == y:
            p.append(f"the pair {pair} does not name two variants")
        elif clients[x] != clients[y]:
            p.append(f"the pair {pair} mixes request clients {clients[x]} and {clients[y]}")
    listed = {}
    if not (stage / "binaries.sha256").is_file():
        p.append("binaries.sha256 is missing")
    else:
        for line in (stage / "binaries.sha256").read_text().splitlines():
            m = re.fullmatch(r"([0-9a-f]{64})  (bin-[A-Z](?:\.zst)?)", line)
            if not m:
                p.append(f"binaries.sha256 has a line that is not HASH  bin-V: {line}")
                continue
            listed[m.group(2)] = m.group(1)
    for v in variants:
        # an empty mapping is the variant's own binary, as the harness reads it
        if (env.get(f"BINARY_OF_{v}") or v) != v:
            continue
        name = next((n for n in (f"bin-{v}", f"bin-{v}.zst") if (stage / n).is_file()), None)
        if name is None:
            p.append(f"bin-{v} is missing")
        elif name not in listed:
            p.append(f"binaries.sha256 has no hash of {name}")
        elif sha256(stage / name) != listed[name]:
            p.append(f"{name} does not match binaries.sha256")
    if not (stage / "data.nt.zst").is_file():
        p.append("data.nt.zst is missing")
    for f in ("nsc-bench-remote.sh", "nsc-bench-summary.py"):
        if not (stage / f).is_file():
            p.append(f"{f} is missing")
    return p


def host_checks(stage):
    """What is wrong with this host for the stage."""
    stage = Path(stage)
    env = read_env(stage / "run.env")
    p = []
    sysfs = Path(os.environ.get("TIMING_KIT_SYSFS", "/sys"))
    online = cpu_list((sysfs / "devices/system/cpu/online").read_text())
    cpus = []
    for k in ("SERVER_CPU", "CLIENT_CPU"):
        try:
            c = int(env[k])
        except (KeyError, ValueError):
            continue
        cpus.append(c)
        if c not in online:
            p.append(f"{k} {c} is not online here")
    if len(cpus) == 2 and all(c in online for c in cpus):
        sib = cpu_list((sysfs / f"devices/system/cpu/cpu{cpus[0]}/topology/thread_siblings_list").read_text())
        if cpus[1] in sib:
            p.append(f"CPUs {cpus[0]} and {cpus[1]} are SMT siblings")
        core = sysfs / "devices/cpu_core/cpus"
        if core.exists():
            fast = cpu_list(core.read_text())
            for c in cpus:
                if c not in fast:
                    p.append(f"CPU {c} is not one of the host's performance cores ({core.read_text().strip()})")
    port = int(env.get("PORT", "3999"))
    with socket.socket() as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("127.0.0.1", port))
        except OSError:
            p.append(f"port {port} is in use")
    locks = Path(os.environ.get("TIMING_KIT_LOCKS", Path.home() / "lanes"))
    for lock in (".bench.lock", ".heavy.lock"):
        if not (locks / lock).exists():
            p.append(f"{locks / lock} does not exist")
    rp = stage / "runtime-paths.txt"
    if rp.exists():
        for path in rp.read_text().split():
            if not Path(path).exists() and not shutil.which("nix-store"):
                p.append(f"{path} is missing and there is no nix-store to realise it")
    variants = env.get("VARIANTS", "A B").split()
    if any((env.get(f"REQUEST_CLIENT_{v}") or env.get("REQUEST_CLIENT")) == "persistent" for v in variants) \
            and not shutil.which("python3"):
        p.append("a variant uses the persistent client and python3 is missing")
    for tool in ("taskset", "chrt", "curl", "zstd"):
        if not shutil.which(tool):
            p.append(f"{tool} is missing")
    if os.environ.get("MBX_REMOTE_URL"):
        p.append("MBX_REMOTE_URL is set")
    return p


def cmd_preflight(a):
    bad = 0
    for stage in a.stages:
        problems = static_checks(stage) + (host_checks(stage) if a.host_side else [])
        for x in problems:
            print(f"{stage}: refused: {x}", file=sys.stderr)
        if not problems:
            print(f"{stage}: preflight passed{' on ' + socket.gethostname() if a.host_side else ''}")
        bad |= bool(problems)
    return 1 if bad else 0


# --------------------------------------------------------------------------- launch

SAFE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*")


def cmd_launch(a):
    if not SAFE.fullmatch(a.unit):
        die(f"the unit name {a.unit} is not a plain name")
    if not re.fullmatch(r"[0-9]+[smhd]?", a.timeout):
        die(f"the timeout is a number with s, m, h or d, not {a.timeout}")
    if not SAFE.fullmatch(a.host):
        die(f"the host {a.host} is not a plain name")
    stages = [Path(s).resolve() for s in a.stages]
    names = [s.name for s in stages]
    if len(set(names)) != len(names):
        die("two stages share a directory name")
    for n in names:
        if not SAFE.fullmatch(n) or n == "timing-kit.py":
            die(f"the stage directory name {n} is not a plain name")
    for s in stages:
        problems = static_checks(s)
        if problems:
            for x in problems:
                print(f"{s}: refused: {x}", file=sys.stderr)
            return 1
    home = f"/home/{os.environ.get('USER', 'kclejeune')}"
    remote = a.remote_dir or f"{home}/lanes/{a.unit}"
    if not re.fullmatch(r"/[A-Za-z0-9_./-]+", remote) or "/../" in remote + "/":
        die(f"the remote directory {remote} is not a plain absolute path")
    q = shlex.quote
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20", a.host]
    locks = f"flock -w 28800 {q(home + '/lanes/.bench.lock')} flock -w 28800 {q(home + '/lanes/.heavy.lock')}"
    steps = [ssh + [f"test ! -e {q(remote)} && ! systemctl --user cat {q(a.unit + '.service')} >/dev/null 2>&1"]]
    # the kit's own driver goes beside the stages, so that a stage need not carry it
    sources = [(str(HERE), "timing-kit.py")] + [(str(s.parent), s.name) for s in stages]
    for parent, name in sources:
        # one transfer each, niced, under the host's locks, so that it never disturbs a timing
        receive = f"mkdir -p {q(remote)} && {locks} nice -n 19 sh -c {q('zstd -q -dc | tar -x -C ' + q(remote))}"
        steps.append(["sh", "-c", f"tar -C {q(parent)} -cf - {q(name)} | zstd -q -T0 -3 | {shlex.join(ssh)} {q(receive)}"])
    steps.append(ssh + [f"cd {q(remote)} && env -u MBX_REMOTE_URL python3 timing-kit.py preflight --host-side "
                        + " ".join(map(q, names))])
    inner = (f"rc=0; {locks} timeout -k 60 {a.timeout} python3 timing-kit.py run {' '.join(map(q, names))} || rc=$?; "
             f"printf 'exit %s %s\\n' \"$rc\" \"$(date -u +%FT%TZ)\" > {q(remote + '/DONE')}; exit \"$rc\"")
    steps.append(ssh + [f"systemd-run --user --unit={q(a.unit)} -p MemoryMax=12G -p MemorySwapMax=2G -p OOMPolicy=continue "
                        f"--setenv=PATH=\"$PATH\" --setenv=MBX_REMOTE_URL= -p WorkingDirectory={q(remote)} "
                        f"/bin/bash -c {q(inner)}"])
    for step in steps:
        print("+ " + shlex.join(step))
        if not a.dry_run:
            r = subprocess.run(step)
            if r.returncode:
                die(f"step failed with {r.returncode}")
    if a.dry_run:
        print("dry run: nothing was sent")
    else:
        print(f"queued {a.unit} on {a.host}; {remote}/DONE marks the end")
    return 0


# ------------------------------------------------------------------------------ run

CHILD = None


def stop_child():
    """End the harness and everything it started, the servers, the busy loops and the
    clients, which share the harness's process group."""
    if CHILD is None:
        return
    for sig, wait in ((signal.SIGTERM, 60), (signal.SIGKILL, 30)):
        try:
            os.killpg(CHILD.pid, sig)
        except ProcessLookupError:
            break
        try:
            CHILD.wait(wait)
        except subprocess.TimeoutExpired:
            continue
        # the harness is gone, and its group may still hold children it left
        try:
            os.killpg(CHILD.pid, 0)
        except ProcessLookupError:
            break


def on_term(signum, _frame):
    stop_child()
    sys.exit(128 + signum)


def log(stage, msg):
    import datetime
    line = f"{datetime.datetime.now(datetime.timezone.utc).strftime('%FT%TZ')} {msg}"
    print(line, flush=True)
    with open(Path(stage) / "run.log", "a") as f:
        f.write(line + "\n")


def run_pass(stage, name, rounds, reps, queries=None):
    global CHILD
    stage = Path(stage)
    d = stage / name
    shutil.rmtree(d, ignore_errors=True)
    (d / "queries").mkdir(parents=True)
    env = read_env(stage / "run.env")
    for v in env.get("VARIANTS", "A B").split():
        for f in (f"bin-{v}", f"bin-{v}.zst"):
            if (stage / f).exists():
                link(stage / f, d / f)
    link(stage / "data.nt.zst", d / "data.nt.zst")
    shutil.copy(stage / "nsc-bench-remote.sh", d)
    text = (stage / "run.env").read_text()
    text = re.sub(r"^ROUNDS=.*$", f"ROUNDS={rounds}", text, flags=re.M)
    text = re.sub(r"^REPS=.*$", f"REPS={reps}", text, flags=re.M)
    (d / "run.env").write_text(text)
    for f in sorted((stage / "queries").glob("*.rq")):
        if queries is None or f.stem in queries:
            shutil.copy(f, d / "queries" / f.name)
    log(stage, f"{name}: {rounds} rounds, {reps} reps, {len(list((d / 'queries').glob('*.rq')))} queries")
    with open(d / "nohup.log", "w") as out:
        CHILD = subprocess.Popen(["bash", str(d / "nsc-bench-remote.sh")], stdout=out, stderr=subprocess.STDOUT,
                                 start_new_session=True)
        CHILD.wait()
        stop_child()
        CHILD = None
    done = (d / "results" / "done").read_text().strip() if (d / "results" / "done").exists() else "no done marker"
    for f in list(d.glob("bin-*")) + [d / "data.nt", d / "data.nt.zst"] + list(d.glob("*/sparkles")):
        if f.is_file():
            f.unlink()
    for sub in ("stores", "srv"):
        shutil.rmtree(d / sub, ignore_errors=True)
    if done != "ok":
        log(stage, f"{name} failed: {done}")
        return False
    summarize_pass(stage, d)
    return True


def pairs_of(stage):
    return [tuple(p.split(",")) for p in read_env(Path(stage) / "run.env").get("PAIRS", "A,B").split()]


def summarize_pass(stage, d):
    extra = [tuple(p.split(",")) for p in read_env(Path(stage) / "run.env").get("XPAIRS", "").split()]
    for x, y in pairs_of(stage) + extra:
        with open(d / f"summary-{x}-{y}.md", "w") as out:
            subprocess.run([sys.executable, str(Path(stage) / "nsc-bench-summary.py"), "--pair", f"{x},{y}", str(d)],
                           stdout=out, check=True)


def pair_json(d, x, y):
    return json.loads((d / f"summary-{x}-{y}.json").read_text())


def cmd_run(a):
    signal.signal(signal.SIGTERM, on_term)
    signal.signal(signal.SIGINT, on_term)
    rc = 0
    for stage in map(Path, a.stages):
        log(stage, f"stage {stage.name} on {socket.gethostname()}, load {open('/proc/loadavg').read().split()[:3]}")
        problems = static_checks(stage) + host_checks(stage)
        if problems:
            for x in problems:
                log(stage, f"refused: {x}")
            rc = max(rc, 1)
            continue
        rp = stage / "runtime-paths.txt"
        paths = rp.read_text().split() if rp.exists() else []
        if paths and shutil.which("nix-store"):
            subprocess.run(["nix-store", "--realise", *paths], stdout=subprocess.DEVNULL, check=True)
        env = read_env(stage / "run.env")
        if not run_pass(stage, "pass1", env.get("ROUNDS", "8"), env.get("REPS", "20")):
            rc = max(rc, 1)
            continue
        flag = float(env.get("FLAG_RATIO", "1.10"))
        flagged = sorted({q for x, y in pairs_of(stage) for q, m in pair_json(stage / "pass1", x, y)["queries"].items()
                          if "exec_ms" in m and m["exec_ms"]["ratio"] >= flag})
        (stage / "flagged.txt").write_text(" ".join(flagged) + "\n")
        log(stage, f"flagged at B/A >= {flag}: {' '.join(flagged) or 'none'}")
        if flagged and not run_pass(stage, "rerun", env.get("RERUN_ROUNDS", "16"), env.get("RERUN_REPS", "40"), set(flagged)):
            rc = max(rc, 1)
            continue
        if write_verdict(stage, flagged):
            rc = max(rc, 3)
        if env.get("EARLY_STOP", "off") == "shadow":
            write_shadow(stage)
        log(stage, "finished")
    return rc


def answer_problems(answers, queries):
    """The queries of a pair summary whose answer check is not "same", as text such as
    "differ for p, q", or every query when the summary has no answer checks."""
    if answers is None:
        return "were not checked"
    bad = {}
    for q in queries:
        bad.setdefault(answers.get(q, "missing"), []).append(q)
    words = {"differ": "differ for", "failed": "failed for", "missing": "are missing for"}
    return "; ".join(f"{words.get(st, st)} {', '.join(qs)}" for st, qs in sorted(bad.items()) if st not in ("same", "changed"))


def expected_changes(summary):
    """The verdict's words for the queries whose answers changed as run.env expects, and for
    those run.env expects to change that did not."""
    e = summary.get("expected_answer_change")
    if not e:
        return ""
    words = []
    if e["changed"]:
        words.append(f"answers changed as run.env expects for {', '.join(e['changed'])} ({e['reason']})")
    if e["unchanged"]:
        words.append(f"run.env expects a change of {', '.join(e['unchanged'])} that did not happen")
    return ", ".join(words)


def write_verdict(stage, flagged):
    """verdict.md for each pair; True when a pass that decides is invalid."""
    stage = Path(stage)
    kit = json.loads((stage / "kit.json").read_text()) if (stage / "kit.json").exists() else {"arms": []}
    label = {arm["variant"]: arm["label"] for arm in kit["arms"]}
    summary = stage / "nsc-bench-summary.py"
    invalid = False
    lines = ["# Timing verdicts", "", f"Busy loops: {read_env(stage / 'run.env').get('SPIN_LOOP', 'user')}. "
             "Runs under different busy loops are never compared.", ""]
    for x, y in pairs_of(stage):
        first = subprocess.run([sys.executable, str(summary), "--check", str(stage / "pass1" / f"summary-{x}-{y}.json"), "first"]).returncode
        mine = [q for q in flagged if q in pair_json(stage / "pass1", x, y)["queries"]]
        p1 = pair_json(stage / "pass1", x, y)
        flag = float(read_env(stage / "run.env").get("FLAG_RATIO", "1.10"))
        own = [q for q in mine if p1["queries"][q]["exec_ms"]["ratio"] >= flag]
        slower = []
        rerun_ok = 0
        rerun_answers = ""
        if own:
            j = stage / "rerun" / f"summary-{x}-{y}.json"
            rerun_ok = subprocess.run([sys.executable, str(summary), "--check", str(j), "rerun", " ".join(own)]).returncode
            r = json.loads(j.read_text())
            slower = [(q, r["queries"][q]["exec_ms"]["ratio"]) for q in own if r["queries"].get(q, {}).get("exec_ms", {}).get("ratio", 0) >= flag]
            rerun_answers = "; ".join(x for x in (answer_problems(r.get("answers"), own),
                                                  "of the run's arms " + answer_problems(r.get("answers_all_arms"), own)
                                                  if answer_problems(r.get("answers_all_arms"), own) else "") if x)
        invalid |= bool(first or rerun_ok)
        v = p1.get("validity") or {}
        first_answers = answer_problems(p1.get("answers"), list(p1.get("answers") or p1["queries"]))
        arms = answer_problems(p1.get("answers_all_arms"), list(p1.get("answers") or p1["queries"]))
        if arms:
            first_answers = "; ".join(x for x in (first_answers, f"of the run's arms {arms}") if x)
        lines.append(f"- {label.get(y, y)} ({y}) against {label.get(x, x)} ({x}): first pass "
                     f"{'valid' if not first else 'INVALID'} (spread {v.get('spread', float('nan')) * 100:.1f}%"
                     + (f", answers {first_answers}" if first_answers else "") + "), "
                     + (expected_changes(p1) + ", " if expected_changes(p1) else "")
                     + f"flagged {', '.join(own) or 'none'}, "
                     + (f"rerun {'valid' if not rerun_ok else 'INVALID'}"
                        + (f" (answers {rerun_answers})" if rerun_answers else "") + ", " if own else "")
                     + (f"slower after the rerun: {', '.join(f'{q} {r:.3f}' for q, r in slower)}" if slower else "none slower after the rerun"))
    lines += ["", "Pairs that share the base's server processes are not independent of each other."]
    (stage / "verdict.md").write_text("\n".join(lines) + "\n")
    return invalid


def stop_rule(rounds, bound, kmin=EARLY_KMIN):
    """Where the early-stopping rule ends one query and what it decides.

    rounds maps each round to the variant's times, {"A": [...], "B": [...]}. The result is
    (stop round, flagged, how), with how "inside" or "outside" for an early stop, "all" when
    the query ran every round, and "unjudged" when a round lacks a variant."""
    last = max(rounds)

    def pooled(upto):
        a = [x for k, v in rounds.items() if k <= upto for x in v.get("A", [])]
        b = [x for k, v in rounds.items() if k <= upto for x in v.get("B", [])]
        return statistics.median(b) / statistics.median(a) if a and b else float("nan")

    logs = []
    for k in sorted(rounds):
        v = rounds[k]
        if not v.get("A") or not v.get("B") or min(v["A"] + v["B"]) <= 0:
            return last, pooled(last) >= bound, "unjudged"
        logs.append(math.log(statistics.median(v["B"]) / statistics.median(v["A"])))
        n = len(logs)
        if n < kmin or k == last:
            continue
        m, h = statistics.mean(logs), T995[min(n - 2, len(T995) - 1)] * statistics.stdev(logs) / math.sqrt(n)
        if m + h < math.log(bound) and pooled(k) < bound:
            return k, False, "inside"
        if m - h >= math.log(bound):
            return k, True, "outside"
    return last, pooled(last) >= bound, "all"


def exact_stop(rounds):
    """Where the every-round question ends one query: at the first round whose ratio is at or
    below 1, which answers no whatever the other rounds hold, or after all rounds. The result
    is (stop round, how), with how "exact" for an early stop, "all" or "unjudged"."""
    last = max(rounds)
    for k in sorted(rounds):
        v = rounds[k]
        if not v.get("A") or not v.get("B") or min(v["A"] + v["B"]) <= 0:
            return last, "unjudged"
        if k < last and statistics.median(v["B"]) / statistics.median(v["A"]) <= 1:
            return k, "exact"
    return last, "all"


def every_round(rounds, upto, complete=True):
    """The every-round question on rounds 1..upto: "yes", "no" or "undecidable".

    A round at or below 1 answers no whatever the other rounds hold. Otherwise every round
    must have a usable pair, timings of both variants above zero, before a cost can be
    claimed, and a round without one leaves the question undecidable. Without complete,
    which says that rounds holds every scheduled round, a round may be missing altogether,
    so the question is undecidable unless a round answers no. A cost is every round above 1
    with the pooled median adding more than EVERY_ROUND_ADDED_US."""
    kept = {k: v for k, v in rounds.items() if k <= upto}
    per, unpaired = [], False
    for v in kept.values():
        if not v.get("A") or not v.get("B") or min(v["A"] + v["B"]) <= 0:
            unpaired = True
        else:
            per.append(statistics.median(v["B"]) / statistics.median(v["A"]))
    if any(r <= 1 for r in per):
        return "no"
    if unpaired or not per or not complete:
        return "undecidable"
    a = [x for v in kept.values() for x in v["A"]]
    b = [x for v in kept.values() for x in v["B"]]
    return "yes" if (statistics.median(b) - statistics.median(a)) * 1000 > EVERY_ROUND_ADDED_US else "no"


def decide(rounds, rule, bound, upto, complete=True):
    """The rule's flag on rounds 1..upto, as a gate would take it from those samples alone.
    An every-round question that cannot be decided does not flag (see every_round)."""
    if rule != "bound":
        return every_round(rounds, upto, complete) == "yes"
    kept = {k: v for k, v in rounds.items() if k <= upto}
    a = [x for v in kept.values() for x in v.get("A", [])]
    b = [x for v in kept.values() for x in v.get("B", [])]
    if not a or not b or min(a + b) <= 0:
        return False
    return statistics.median(b) / statistics.median(a) >= bound


def undecidable(rounds, rule, upto, complete=True):
    """Whether the rule cannot answer on rounds 1..upto, which only the every-round question
    can be, when a round lacks a usable pair, or the schedule is not known to be complete,
    and no round answers no."""
    return rule == "every-round" and every_round(rounds, upto, complete) == "undecidable"


def scheduled_rounds(rounds, scheduled):
    """rounds with an empty entry for each scheduled round that has none, so that a round
    whose rows are missing altogether counts as a round without a pair."""
    return {k: rounds.get(k, {"A": [], "B": []}) for k in sorted(set(rounds) | set(scheduled))}


def combined_stop(per_metric, bound, rules=("bound",), scheduled=(), complete=True):
    """Where one query stops when the stop must answer every rule on every metric.

    per_metric maps each metric to its rounds, as stop_rule takes them. Each rule and metric
    stops on its own, "bound" by stop_rule and "every-round" by exact_stop, and the query
    stops at the latest of them, where each is decided again on the rounds the query keeps.
    The every-round question also judges each of the scheduled rounds that a metric has no
    entry for, as a round without a pair, while the bound's stop and pooled flag take the
    rounds as given. complete=False says the schedule is unknown, so that the every-round
    question cannot claim a cost (see every_round). The result is (stop round, {"METRIC RULE": {"stop_round", "how",
    "flagged", "flagged_all_rounds", "undecidable", "undecidable_all_rounds"}})."""
    bad = [r for r in rules if r not in STOP_RULES]
    if bad or not rules:
        raise ValueError(f"unknown early-stopping rules {bad or 'none'}, which must be some of {', '.join(STOP_RULES)}")
    out = {}
    of = {}
    for m, rounds in per_metric.items():
        for r in rules:
            of[f"{m} {r}"] = scheduled_rounds(rounds, scheduled) if r == "every-round" else rounds
            k, how = exact_stop(of[f"{m} {r}"]) if r == "every-round" else stop_rule(rounds, bound)[::2]
            out[f"{m} {r}"] = {"stop_round": k, "how": how}
    stop = max(x["stop_round"] for x in out.values())
    for key, x in out.items():
        r = key.split(" ")[1]
        rounds = of[key]
        x["flagged"] = decide(rounds, r, bound, stop, complete)
        x["flagged_all_rounds"] = decide(rounds, r, bound, max(rounds), complete)
        x["undecidable"] = undecidable(rounds, r, stop, complete)
        x["undecidable_all_rounds"] = undecidable(rounds, r, max(rounds), complete)
    return stop, out


def load_summary_module():
    spec = importlib.util.spec_from_file_location("nsc_bench_summary", HERE / "nsc-bench-summary.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def pass_check(d, pair):
    """The full-pass check of the pair on the finished pass d: whether nsc-bench-summary.py
    --check lets it decide, and what stops it apart from the host's spread and drift, which
    early stopping judges by the controls instead. The pair's summary is written to a
    scratch directory, so that the pass is left as it was."""
    import contextlib
    import io
    import tempfile
    summary = load_summary_module()
    x, y = pair
    with tempfile.TemporaryDirectory() as tmp:
        for f in ("results", "run.env", "queries", "manifest.txt"):
            if (d / f).exists():
                os.symlink((d / f).resolve(), Path(tmp) / f)
        subprocess.run([sys.executable, str(HERE / "nsc-bench-summary.py"), "--pair", f"{x},{y}", tmp],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        j = Path(tmp) / f"summary-{x}-{y}.json"
        with contextlib.redirect_stderr(io.StringIO()):
            ok = summary.check(str(j), "first", "") == 0
        s = json.loads(j.read_text())
    problems = []
    if "answers" not in s or "answers_all_arms" not in s:
        problems.append("no answer checks")
    else:
        for name, key in (("the pair's", "answers"), ("the run's arms'", "answers_all_arms")):
            bad = summary.answer_problems(s[key], list(s["answers"]))
            if bad:
                problems.append(f"{name} answers {bad}")
    if s.get("missing"):
        problems.append("no timings of both variants for " + ", ".join(s["missing"]))
    if s.get("thin"):
        problems.append("timings from one server process of a variant for " + ", ".join(s["thin"]))
    return ok, problems


def pass_schedule(d):
    """The number of rounds the pass d scheduled, from ROUNDS in its run.env, which run_pass
    writes, or in its parent's when the pass has none, as a replay of a stage finds it. None
    when neither file exists or the first that exists gives no plain positive ROUNDS."""
    for f in (Path(d) / "run.env", Path(d).parent / "run.env"):
        if f.is_file():
            v = read_env(f, []).get("ROUNDS", "")
            return int(v) if v.isdigit() and int(v) > 0 else None
    return None


def shadow(d, pair=("A", "B"), controls=None, bound=1.10, rules=("bound",), schedule=None):
    """The early-stopping rule's decisions on the finished pass d beside those of all rounds.

    A query stops only once every rule in rules has stopped on both execution and wall time,
    and each of those decisions is taken on the rounds it keeps (combined_stop). The validity
    of all rounds is the full-pass check. The shadow's validity is the same
    check with the host's spread and drift judged by the controls, so that answers, missing
    timings and queries with one server process stop both alike. A query stopped after its
    first rounds keeps two server processes of each variant a round, so stopping cannot
    thin it below the check's minimum.

    The every-round question judges each of rounds 1..schedule, ROUNDS of the pass's run.env
    when schedule is None (pass_schedule), so a round that left no rows counts as a round
    without a pair. When the schedule is unknown, that question cannot claim a cost."""
    d = Path(d)
    if schedule is None:
        schedule = pass_schedule(d)
    path = d / "results" / "samples.tsv"
    with path.open() as f:
        rows = list(csv.DictReader(f, delimiter="\t"))
    x, y = pair
    name = {x: "A", y: "B"}
    rules = tuple(rules)
    if not rules or any(r not in STOP_RULES for r in rules):
        raise ValueError(f"unknown early-stopping rules {list(rules)}, which must be some of {', '.join(STOP_RULES)}")
    samples = defaultdict(lambda: defaultdict(list))  # query -> variant -> (round, epoch, ms)
    wall_samples = defaultdict(lambda: defaultdict(list))  # the same with wall ms
    walls = defaultdict(float)  # (query, round) -> wall ms of both variants
    sent = defaultdict(int)  # (query, round) -> requests of both variants
    order = []
    for r in rows:
        if r["variant"] not in name:
            continue
        q, k = r["query"], int(r["round"])
        if q not in order:
            order.append(q)
        sent[(q, k)] += 1
        if r["exec_ms"] == "error":
            continue
        walls[(q, k)] += float(r["wall_ms"])
        samples[q][name[r["variant"]]].append((k, int(r["epoch"]), float(r["exec_ms"])))
        wall_samples[q][name[r["variant"]]].append((k, int(r["epoch"]), float(r["wall_ms"])))
    if not order:
        raise ValueError(f"no samples of {x} and {y} in {path}")
    controls = [q for q in (controls if controls is not None else CONTROLS) if q in order]
    # every scheduled round, and any round the pair ran past it
    scheduled = sorted({k for (_q, k) in sent} | set(range(1, (schedule or 0) + 1)))
    summary = load_summary_module()
    full = summary.validity(samples, order)
    full_ok, problems = pass_check(d, pair)
    ctl = summary.validity(samples, controls) if len(controls) >= MIN_CONTROLS else None
    queries = {}
    for q in order:
        per_metric = {}
        for metric, src in zip(STOP_METRICS, (samples, wall_samples)):
            rounds = defaultdict(lambda: {"A": [], "B": []})
            for k in {k for (qq, k) in sent if qq == q}:
                rounds[k]  # a round whose every request failed still counts, as unjudged
            for v, xs in src[q].items():
                for k, _e, ms in xs:
                    rounds[k][v].append(ms)
            per_metric[metric] = dict(rounds)
        stop, metrics = combined_stop(per_metric, bound, rules, scheduled, complete=schedule is not None)
        last = max(max(per_metric["exec_ms"]), stop, schedule or 0)
        if q in controls:  # a control runs every round, and all rounds decide
            stop = last
            for dec in metrics.values():
                dec.update(stop_round=last, how="control", flagged=dec["flagged_all_rounds"],
                           undecidable=dec["undecidable_all_rounds"])
        latest = [m["how"] for m in metrics.values() if m["stop_round"] == stop]
        how = "control" if q in controls else next((h for h in latest if h not in ("all", "unjudged")),
                                                   "unjudged" if "unjudged" in latest else "all")
        first = metrics.get("exec_ms bound") or next(iter(metrics.values()))
        queries[q] = {"stop_round": stop, "rounds": last, "how": how, "flagged": first["flagged"],
                      "flagged_all_rounds": first["flagged_all_rounds"], "metrics": metrics}
    # the controls decide validity, and a spread within the fallback band sends every stopped
    # query back to its remaining rounds, where the rule of all rounds decides
    fallback = bool(ctl and ctl["valid"] and ctl["spread"] >= FALLBACK_SPREAD)
    if fallback:
        for m in queries.values():
            m.update(stop_round=m["rounds"], flagged=m["flagged_all_rounds"])
            for dec in m["metrics"].values():
                dec.update(stop_round=m["rounds"], flagged=dec["flagged_all_rounds"],
                           undecidable=dec["undecidable_all_rounds"])
    valid_all = full_ok
    valid_shadow = valid_all if fallback else bool(ctl and ctl["valid"]) and not problems
    total = sum(sent.values()) or 1
    kept = sum(n for (q, k), n in sent.items() if k <= queries[q]["stop_round"])
    wall_all = sum(walls.values()) or 1
    wall_kept = sum(w for (q, k), w in walls.items() if k <= queries[q]["stop_round"])
    decisions = sorted(f"{q} {key}" for q, m in queries.items() for key, x in m["metrics"].items()
                       if (x["flagged"], x["undecidable"]) != (x["flagged_all_rounds"], x["undecidable_all_rounds"]))
    unknown = sorted(f"{q} {key}" for q, m in queries.items() for key, x in m["metrics"].items()
                     if x["undecidable"] or x["undecidable_all_rounds"])
    differ = sorted({d.split(" ")[0] for d in decisions})
    return {
        "pair": {"A": x, "B": y}, "samples_sha256": sha256(path),
        "rule": {"kmin": EARLY_KMIN, "conf": EARLY_CONF, "bound": bound, "fallback_spread": FALLBACK_SPREAD,
                 "max_spread": summary.MAX_SPREAD, "metrics": list(STOP_METRICS), "rules": list(rules),
                 "every_round_added_us": EVERY_ROUND_ADDED_US},
        "controls": controls, "schedule": schedule,
        "validity": {"all_rounds": valid_all, "shadow": valid_shadow, "controls": ctl, "fallback": fallback,
                     "all_rounds_spread": full and full["spread"], "pass_problems": problems},
        "queries": queries, "flags_differ": differ, "decisions_differ": decisions, "undecidable": unknown,
        "request_share": kept / total, "wall_share": wall_kept / wall_all,
        "agree": not differ and valid_all == valid_shadow,
    }


def shadow_line(label, s):
    v = s["validity"]
    stopped = sum(1 for m in s["queries"].values() if m["stop_round"] < m["rounds"])
    return (f"- {label}: {'agrees' if s['agree'] else 'DISAGREES'} with all rounds. "
            f"Validity {'valid' if v['shadow'] else 'INVALID'} against {'valid' if v['all_rounds'] else 'INVALID'}"
            + (", with the controls' spread in the fallback band" if v["fallback"] else "")
            + (f", since the pass cannot decide: {'; '.join(v['pass_problems'])}" if v.get("pass_problems") else "") + ". "
            f"{stopped} of {len(s['queries'])} queries would stop early, "
            + (f"flags differ for {', '.join(s['decisions_differ'])}, " if s["decisions_differ"] else "")
            + (f"the every-round question cannot decide {', '.join(s['undecidable'])}, since "
               + ("a round has no usable pair" if s.get("schedule") else "the pass does not say how many rounds it scheduled")
               + ", and flags no cost there, " if s.get("undecidable") else "")
            + f"{s['request_share']:.0%} of requests and {s['wall_share']:.0%} of client wall time would run.")


def write_shadow(stage):
    """shadow-X-Y.json in the first pass and shadow.md for each pair; a failure is logged and
    changes nothing else."""
    stage = Path(stage)
    try:
        env = read_env(stage / "run.env")
        kit = json.loads((stage / "kit.json").read_text()) if (stage / "kit.json").exists() else {"arms": []}
        label = {arm["variant"]: arm["label"] for arm in kit["arms"]}
        lines = ["# Early stopping in shadow mode", "",
                 "The rule did not act. Every round ran, and verdict.md comes from all of them.", ""]
        for x, y in pairs_of(stage):
            s = shadow(stage / "pass1", (x, y), env.get("CONTROLS", " ".join(CONTROLS)).split(),
                       float(env.get("FLAG_RATIO", "1.10")), env.get("EARLY_STOP_RULES", "bound").split(","))
            (stage / "pass1" / f"shadow-{x}-{y}.json").write_text(json.dumps(s, indent=1) + "\n")
            lines.append(shadow_line(f"{label.get(y, y)} ({y}) against {label.get(x, x)} ({x})", s))
        (stage / "shadow.md").write_text("\n".join(lines) + "\n")
    except Exception as e:  # noqa: BLE001 - shadow mode must not change the run's result
        log(stage, f"shadow mode failed, which changes nothing else: {e!r}")


def cmd_shadow(a):
    d = Path(a.pass_dir)
    env = {}
    for f in (d / "run.env", d.parent / "run.env"):
        if f.is_file():
            env = read_env(f)
            break
    controls = a.controls.split(",") if a.controls else env.get("CONTROLS", " ".join(CONTROLS)).split()
    bound = float(a.flag_ratio or env.get("FLAG_RATIO", "1.10"))
    rules = (a.rules or env.get("EARLY_STOP_RULES", "bound")).split(",")
    x, _, y = (a.pair or "A,B").partition(",")
    s = shadow(d, (x, y), controls, bound, rules)
    (d / f"shadow-{x}-{y}.json").write_text(json.dumps(s, indent=1) + "\n")
    print(shadow_line(f"{y} against {x}", s))
    return 0


def cmd_summarize(a):
    stage = Path(a.stage)
    for name in ("pass1", "rerun"):
        if (stage / name / "results" / "samples.tsv").exists():
            summarize_pass(stage, stage / name)
    flagged = (stage / "flagged.txt").read_text().split() if (stage / "flagged.txt").exists() else []
    write_verdict(stage, flagged)
    print((stage / "verdict.md").read_text(), end="")
    return 0


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("stage")
    s.add_argument("--out", required=True)
    s.add_argument("--base", required=True, help="LABEL=BINARY")
    s.add_argument("--tip", action="append", default=[], help="LABEL=BINARY, once per tip")
    s.add_argument("--data", required=True)
    s.add_argument("--queries", help="a directory of .rq files (default: the queries of scripts/bench.sh)")
    s.add_argument("--bench-sh", default=str(HERE / "bench.sh"))
    s.add_argument("--only", help="comma-separated query names to keep")
    s.add_argument("--controls", help="comma-separated control queries (default: the screen's ten)")
    s.add_argument("--rounds", default="8")
    s.add_argument("--reps", default="20")
    s.add_argument("--warmup", default="3")
    s.add_argument("--rayon-threads", default="1")
    s.add_argument("--server-cpu", default="2")
    s.add_argument("--client-cpu", default="4")
    s.add_argument("--port", default="3999")
    s.add_argument("--serve-args", default="")
    s.add_argument("--execution", default="")
    s.add_argument("--client", default="curl", choices=("curl", "persistent"))
    s.add_argument("--gap-ms", default="0")
    s.add_argument("--clients", help="comma-separated clients, each curl or persistent[:GAP_MS], to time every arm "
                   "under each of them, as in a qualification of the persistent client")
    s.add_argument("--rotate", action="store_true", help="rotate the palindrome across rounds")
    s.add_argument("--flag-ratio", default="1.10")
    s.add_argument("--rerun-rounds", default="16")
    s.add_argument("--rerun-reps", default="40")
    s.add_argument("--note", default="")
    s.add_argument("--spin-loop", default="user", choices=("user", "syscall"),
                   help="the busy loop on the timing CPUs: user never enters the kernel, and syscall does, "
                        "so that a wake-up under lazy preemption does not wait for the next tick (default user)")
    s.add_argument("--expected-answer-change", default="",
                   help="comma-separated queries whose answers the tips are expected to change")
    s.add_argument("--expected-answer-change-reason", default="", help="why those answers change")
    s.add_argument("--early-stop", default="shadow", choices=("off", "shadow"),
                   help="record what the early-stopping rule would decide, without acting on it (default shadow)")
    s.add_argument("--early-stop-rules", default="bound",
                   help="comma-separated questions the stop must answer: bound, the flag at --flag-ratio, and "
                        "every-round, a cost in every round, which takes the exact stop (default bound)")
    f = sub.add_parser("preflight")
    f.add_argument("--host-side", action="store_true")
    f.add_argument("stages", nargs="+")
    l = sub.add_parser("launch")
    l.add_argument("--host", required=True)
    l.add_argument("--unit", required=True)
    l.add_argument("--remote-dir")
    l.add_argument("--timeout", default="4h")
    l.add_argument("--dry-run", action="store_true")
    l.add_argument("stages", nargs="+")
    r = sub.add_parser("run")
    r.add_argument("stages", nargs="+")
    m = sub.add_parser("summarize")
    m.add_argument("stage")
    h = sub.add_parser("shadow")
    h.add_argument("--pair")
    h.add_argument("--controls")
    h.add_argument("--flag-ratio")
    h.add_argument("--rules", help="comma-separated questions the stop must answer, of bound and every-round "
                   "(default EARLY_STOP_RULES from run.env, else bound)")
    h.add_argument("pass_dir")
    a = p.parse_args()
    return {"stage": cmd_stage, "preflight": cmd_preflight, "launch": cmd_launch, "run": cmd_run,
            "summarize": cmd_summarize, "shadow": cmd_shadow}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
