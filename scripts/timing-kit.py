#!/usr/bin/env python3
"""A kit for interleaved A/B timing runs on a quiet Intel host.

    scripts/timing-kit.py stage --out DIR --base LABEL=BIN --tip LABEL=BIN [--tip ...] [options]
    scripts/timing-kit.py preflight [--host-side] STAGE...
    scripts/timing-kit.py launch --host HOST --unit NAME [--dry-run] STAGE...
    scripts/timing-kit.py run STAGE...          (the unit's driver, on the timing host)
    scripts/timing-kit.py summarize STAGE

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
nsc-bench-summary.py --pair, and verdict.md lists them. DONE gets the exit status, which
is 3 when a pass that decides is invalid.
"""

import argparse
import json
import os
import re
import shlex
import shutil
import signal
import socket
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
LETTERS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
# Queries of the screen that take at least 0.3 ms on the Intel hosts and keep their rounds
# whatever a selection or a stopping rule does, so that they carry the validity check.
CONTROLS = ("expr-bind-group range-topk not-exists class-predicates exists-join star-join optional-chain "
            "two-hop-count group-avg knows-reach").split()
MIN_CONTROLS = 3
KIT_FILES = ("nsc-bench-remote.sh", "nsc-bench-summary.py", "timing-kit.py")


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
        "SERVER_CPU": a.server_cpu, "CLIENT_CPU": a.client_cpu, "PORT": a.port, "SPIN": 1,
        "SERVE_ARGS": a.serve_args, "EXECUTION": a.execution,
        "PAIRS": " ".join(pairs), "XPAIRS": " ".join(xpairs), "CONTROLS": " ".join(controls),
        "FLAG_RATIO": a.flag_ratio, "RERUN_ROUNDS": a.rerun_rounds, "RERUN_REPS": a.rerun_reps,
    }
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
    if not re.fullmatch(r"[0-9]+(\.[0-9]+)?", env.get("FLAG_RATIO", "1.10")) or float(env.get("FLAG_RATIO", "1.10")) <= 1:
        p.append(f"FLAG_RATIO must be a ratio above 1, not {env.get('FLAG_RATIO')}")
    queries = {f.stem for f in (stage / "queries").glob("*.rq")} if (stage / "queries").is_dir() else set()
    if not queries:
        p.append("the stage has no queries")
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
        if env.get(f"BINARY_OF_{v}", v) != v:
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
        log(stage, "finished")
    return rc


def write_verdict(stage, flagged):
    """verdict.md for each pair; True when a pass that decides is invalid."""
    stage = Path(stage)
    kit = json.loads((stage / "kit.json").read_text()) if (stage / "kit.json").exists() else {"arms": []}
    label = {arm["variant"]: arm["label"] for arm in kit["arms"]}
    summary = stage / "nsc-bench-summary.py"
    invalid = False
    lines = ["# Timing verdicts", ""]
    for x, y in pairs_of(stage):
        first = subprocess.run([sys.executable, str(summary), "--check", str(stage / "pass1" / f"summary-{x}-{y}.json"), "first"]).returncode
        mine = [q for q in flagged if q in pair_json(stage / "pass1", x, y)["queries"]]
        p1 = pair_json(stage / "pass1", x, y)
        flag = float(read_env(stage / "run.env").get("FLAG_RATIO", "1.10"))
        own = [q for q in mine if p1["queries"][q]["exec_ms"]["ratio"] >= flag]
        slower = []
        rerun_ok = 0
        if own:
            j = stage / "rerun" / f"summary-{x}-{y}.json"
            rerun_ok = subprocess.run([sys.executable, str(summary), "--check", str(j), "rerun", " ".join(own)]).returncode
            r = json.loads(j.read_text())
            slower = [(q, r["queries"][q]["exec_ms"]["ratio"]) for q in own if r["queries"].get(q, {}).get("exec_ms", {}).get("ratio", 0) >= flag]
        invalid |= bool(first or rerun_ok)
        v = p1.get("validity") or {}
        lines.append(f"- {label.get(y, y)} ({y}) against {label.get(x, x)} ({x}): first pass "
                     f"{'valid' if not first else 'INVALID'} (spread {v.get('spread', float('nan')) * 100:.1f}%), "
                     f"flagged {', '.join(own) or 'none'}, "
                     + (f"rerun {'valid' if not rerun_ok else 'INVALID'}, " if own else "")
                     + (f"slower after the rerun: {', '.join(f'{q} {r:.3f}' for q, r in slower)}" if slower else "none slower after the rerun"))
    lines += ["", "Pairs that share the base's server processes are not independent of each other."]
    (stage / "verdict.md").write_text("\n".join(lines) + "\n")
    return invalid


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
    a = p.parse_args()
    return {"stage": cmd_stage, "preflight": cmd_preflight, "launch": cmd_launch, "run": cmd_run,
            "summarize": cmd_summarize}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
