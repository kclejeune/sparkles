"""Where scripts/nsc-bench.sh and scripts/nsc-writes.sh build a revision (--build-host,
scripts/nsc-build-host.sh): the choice of host, the remote build and its cleanup.

A fake ssh runs each remote command in its own session with HOME set to the host's
directory under hosts/, so the "remote" processes outlive an interrupted ssh as they would
on a real host. A fake systemd-run makes each scope its own process group, which the fake
systemctl stop kills."""

import json
import ctypes
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ["remote-executor.sh", "remote-executor.py", "remote-screen.sh", "nsc-bench.sh", "nsc-writes.sh", "nsc-build.sh", "nsc-build-host.sh",
           "nsc-build-remote.sh", "nsc-writes-remote.sh", "cargo-sparkles-task", "gate/config.sh"]

TOOLS = {
    "ssh": """#!/usr/bin/env python3
import os,pathlib,signal,subprocess,sys,time
a=sys.argv[1:]
while a and a[0].startswith('-'):
    a=a[2:] if a[0]=='-o' else a[1:]
host,cmd=a[0],' '.join(a[1:])
home=pathlib.Path(os.environ['FAKE_HOSTS'])/host
with open(os.environ['FAKE_SSH_LOG'],'a') as s:s.write(host+'\\t'+cmd+'\\n')
if not home.is_dir() or (home/'unreachable').exists():
    sys.stderr.write('ssh: connect to host '+host+': unreachable\\n');raise SystemExit(255)
env=dict(os.environ,HOME=str(home))
# FAKE_SSH_DROP=TEXT:N drops the connection of the first N commands that contain TEXT,
# after the remote command has started and has received at most 4 KiB of input. The
# remote command keeps running, as on a real host. With FAKE_SSH_DROP_BYTES=K the first K
# bytes of its output reach the caller before the drop.
drop=os.environ.get('FAKE_SSH_DROP','')
if drop:
    text,count=drop.rsplit(':',1)
    counter=pathlib.Path(os.environ['FAKE_HOSTS'])/'drops'
    done=int(counter.read_text()) if counter.exists() else 0
    if text in cmd and done<int(count):
        counter.write_text(str(done+1))
        keep=int(os.environ.get('FAKE_SSH_DROP_BYTES','0'))
        p=subprocess.Popen(['bash','-c',cmd],cwd=home,env=env,start_new_session=True,stdin=subprocess.PIPE,
                           stdout=subprocess.PIPE if keep else subprocess.DEVNULL)
        try:p.stdin.write(sys.stdin.buffer.read(4096));p.stdin.close()
        except BrokenPipeError:pass
        if keep:sys.stdout.buffer.write(p.stdout.read(keep));sys.stdout.flush()
        time.sleep(0.3)
        sys.stderr.write('Connection to '+host+' closed by remote host.\\n');os._exit(255)
p=subprocess.Popen(['bash','-c',cmd],cwd=home,env=env,start_new_session=True)
signal.signal(signal.SIGINT,lambda *_:os._exit(255))
raise SystemExit(p.wait())
""",
    "systemd-run": """#!/usr/bin/env python3
import os,pathlib,sys,time
a=sys.argv[1:];unit=None;props=[]
while a and a[0].startswith('-'):
    o=a.pop(0)
    if o=='--':break
    if o in('--unit','-p'):
        v=a.pop(0)
        if o=='--unit':unit=v
        else:props.append(v)
state=pathlib.Path(os.environ['FAKE_HOSTS'])/'units'
state.mkdir(exist_ok=True)
if unit:
    # FAKE_SCOPE_DELAY stalls the creation of a named scope, and FAKE_SCOPE_FAIL refuses it.
    time.sleep(float(os.environ.get('FAKE_SCOPE_DELAY','0')))
    if os.environ.get('FAKE_SCOPE_FAIL'):
        sys.stderr.write('Failed to start transient scope unit\\n');raise SystemExit(1)
    if '.' not in unit:unit+='.scope'
    # A detached build is already a session leader, whose group the stop can kill.
    if os.getsid(0)!=os.getpid():os.setsid()
    (state/unit).write_text(str(os.getpid())+'\\n'+' '.join(props)+'\\n')
os.execvp(a[0],a)
""",
    "systemctl": """#!/usr/bin/env python3
import os,pathlib,signal,sys
a=sys.argv[1:]
state=pathlib.Path(os.environ['FAKE_HOSTS'])
with open(state/'systemctl.log','a') as s:s.write(os.environ['HOME']+'\\t'+' '.join(a)+'\\n')
if 'list-units' in a:
    n=int((pathlib.Path(os.environ['HOME'])/'gate-units').read_text()) if (pathlib.Path(os.environ['HOME'])/'gate-units').exists() else 0
    for i in range(n):print(f'gate-{i}.service loaded active running gate')
    raise SystemExit(0)
if 'stop' in a:
    name=a[-1] if '.' in a[-1] else a[-1]+'.service'
    f=state/'units'/name
    if f.exists():
        try:os.killpg(int(f.read_text().split()[0]),signal.SIGKILL)
        except ProcessLookupError:pass
        (state/(name+'.stopped')).touch()
if 'is-active' in a:
    name=a[-1] if '.' in a[-1] else a[-1]+'.service'
    f=state/'units'/name
    if not f.exists() or (state/(name+'.stopped')).exists():raise SystemExit(3)
    try:os.kill(int(f.read_text().split()[0]),0)
    except ProcessLookupError:raise SystemExit(3)
""",
    "mbx": """#!/usr/bin/env python3
import json,os,pathlib,subprocess,sys,time
a=sys.argv[1:]
if a==['--version']:print('mbx 9.9.9');raise SystemExit(0)
if a[:2]==['sparkles-task','--']:raise SystemExit(subprocess.run(a[2:]).returncode)
with open(os.environ['FAKE_BUILDS'],'a') as s:s.write(json.dumps({'home':os.environ['HOME'],'cwd':os.getcwd(),'args':a,'jobs':os.environ.get('CARGO_BUILD_JOBS'),'target_override':os.environ.get('CARGO_TARGET_DIR'),'cache_override':os.environ.get('MBX_CACHE_DIR')})+'\\n')
pathlib.Path(os.environ['FAKE_HOSTS'],'build-started').touch()
for i in range(int(os.environ.get('FAKE_BUILD_LOG_LINES','0'))):print(f'log line {i:07d}')
sys.stdout.flush()
time.sleep(float(os.environ.get('FAKE_BUILD_DELAY','0')))
if os.environ.get('FAKE_BUILD_FAIL'):raise SystemExit(17)
assert pathlib.Path('ui/build/index.html').read_text().startswith('<!doctype html>')
pathlib.Path('target/release').mkdir(parents=True,exist_ok=True)
pathlib.Path('target/release/sparkles').write_text('built in '+os.environ['HOME']+' with '+' '.join(a[4:]))
pathlib.Path('target/release/sparkles').chmod(0o755)
""",
    "readelf": "#!/bin/sh\nexit 0\n",
    "nsc": "#!/bin/sh\necho 'nsc must not run' >&2\nexit 99\n",
    "cargo": "#!/bin/sh\necho 'cargo test'\n",
    "rustc": "#!/bin/sh\necho 'rustc test'\n",
    "cc": "#!/bin/sh\necho 'cc test'\n",
    "c++": "#!/bin/sh\necho 'c++ test'\n",
    "zstd": """#!/usr/bin/env python3
import shutil,sys
a=sys.argv[1:];shutil.copyfile([x for x in a if not x.startswith('-') and x!=a[a.index('-o')+1]][0],a[a.index('-o')+1])
""",
}


class BuildHost(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="sparkles-nsc-host-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "repo"
        for name in SCRIPTS:
            (self.repo / "scripts" / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / "scripts" / name, self.repo / "scripts" / name)
        shutil.copytree(ROOT / "scripts/boat", self.repo / "scripts/boat", ignore=shutil.ignore_patterns("__pycache__"))
        (self.repo / "mise.toml").write_text('[tools]\nmr-boxington = "9.9.9"\n')
        git = ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t",
               "-c", "commit.gpgsign=false"]
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        subprocess.run([*git, "add", "."], check=True)
        subprocess.run([*git, "commit", "-q", "-m", "fixture"], check=True)
        self.sha = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True,
                                  text=True).stdout.strip()
        self.hosts = self.root / "hosts"
        for host in ("h1", "h2", "bench"):
            (self.hosts / host).mkdir(parents=True)
        self.home = self.root / "home"
        self.home.mkdir()
        self.cache = self.root / "cache"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, text in TOOLS.items():
            (self.bin / name).write_text(text)
            (self.bin / name).chmod(0o755)
        self.builds_log = self.root / "builds.jsonl"
        self.ssh_log = self.root / "ssh.log"
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        HOME=str(self.home), SPARKLES_NSC_CACHE=str(self.cache),
                        FAKE_HOSTS=str(self.hosts), FAKE_BUILDS=str(self.builds_log),
                        FAKE_SSH_LOG=str(self.ssh_log), SPARKLES_BENCH_HOST="bench",
                        SPARKLES_GATE_CONFIG=str(self.root / "no-gate.env"),
                        SPARKLES_NSC_BUILD_MIN_FREE_GB="0", SPARKLES_NSC_BUILD_POLL_SECONDS="0.1",
                        SPARKLES_NSC_BUILD_RETRY_SECONDS="0.1")
        for name in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "SPARKLES_NSC_BUILD_HOSTS",
                     "SPARKLES_NSC_BUILD_CONFIG", "CARGO_BUILD_JOBS", "SPARKLES_NSC_BUILD_RETRIES",
                     "SPARKLES_NSC_BUILD_MEMORY_MAX", "FAKE_SSH_DROP", "FAKE_SSH_DROP_BYTES", "FAKE_SCOPE_DELAY", "FAKE_SCOPE_FAIL",
                     "FAKE_BUILD_LOG_LINES", "SPARKLES_NSC_BUILD_DEADLINE", "SPARKLES_NSC_BUILD_RUNTIME"):
            self.env.pop(name, None)
        self.runs = 0
        self.holders = []
        self.fixture_libc=ctypes.CDLL(None);self.fixture_previous=ctypes.c_int()
        self.assertEqual(self.fixture_libc.prctl(37,ctypes.byref(self.fixture_previous),0,0,0),0)
        self.assertEqual(self.fixture_libc.prctl(36,1,0,0,0),0)
        self.reaper_stop=threading.Event()
        def reap_owned_mock_scopes():
            while not self.reaper_stop.wait(0.01):
                for path in (self.hosts/'units').glob('*.scope'):
                    try:
                        group=int(path.read_text().split()[0])
                        while os.waitpid(-group,os.WNOHANG)[0]>0:pass
                    except (ChildProcessError,FileNotFoundError,ProcessLookupError,ValueError):pass
        self.fixture_reaper=threading.Thread(target=reap_owned_mock_scopes,daemon=True)
        self.fixture_reaper.start()

    def tearDown(self):
        for holder in self.holders:
            try:os.killpg(holder.pid,signal.SIGTERM)
            except ProcessLookupError:pass
            holder.wait(timeout=3)
            end=time.monotonic()+3
            while True:
                try:
                    while os.waitpid(-holder.pid,os.WNOHANG)[0]>0:pass
                except ChildProcessError:pass
                try:os.killpg(holder.pid,0)
                except ProcessLookupError:break
                self.assertLess(time.monotonic(),end,'owned lock fixture did not close')
                time.sleep(0.01)
        self.reaper_stop.set();self.fixture_reaper.join(timeout=1)
        self.fixture_libc.prctl(36,self.fixture_previous.value,0,0,0)

    def hold_lock(self, host, seconds=60):
        lanes = self.hosts / host / "lanes"
        lanes.mkdir(exist_ok=True)
        holder = subprocess.Popen(["flock", str(lanes / ".heavy.lock"), "sleep", str(seconds)],start_new_session=True)
        self.holders.append(holder)
        deadline = time.monotonic() + 5
        while subprocess.run(["flock", "-n", str(lanes / ".heavy.lock"), "true"]).returncode == 0:
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.05)
        return holder

    def bench(self, *args, env=None, ok_stop=True):
        self.runs += 1
        out = self.root / f"out-{self.runs}"
        result = subprocess.run(["bash", str(self.repo / "scripts/nsc-bench.sh"), "--out", str(out),
                                 *args, self.sha, self.sha], env=env or self.env,
                                capture_output=True, text=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        if ok_stop:
            # Both binaries are staged and the run stops at the compiler check.
            self.assertIn("not built with the same known compilers", result.stderr, result.stderr)
        return result, out

    def builds(self):
        if not self.builds_log.exists():
            return []
        return [json.loads(line) for line in self.builds_log.read_text().splitlines()]

    def entry(self):
        return self.cache / "bin" / self.sha

    def scratch(self, host):
        d = self.hosts / host / "lanes" / "nsc-build"
        return sorted(p.name for p in d.iterdir()) if d.exists() else []

    def test_without_configuration_the_build_stays_local(self):
        result, out = self.bench()
        [build] = self.builds()
        self.assertEqual(build["home"], str(self.home))
        self.assertFalse(self.ssh_log.exists(), "a local build must not use ssh")
        record = (self.entry() / "build.txt").read_text()
        self.assertIn("build host: local (", record)
        self.assertIn("cc: cc test", record)
        self.assertIn("built on local (", (out / "manifest.txt").read_text())
        # auto with no hosts also builds here.
        (self.entry() / "build.txt").unlink()
        result, _ = self.bench("--build-host", "auto")
        self.assertIn("none configured", result.stdout)
        self.assertEqual(self.builds()[-1]["home"], str(self.home))

    def test_auto_skips_a_held_host_and_the_entry_is_reused(self):
        self.hold_lock("h1")
        env = dict(self.env, SPARKLES_NSC_BUILD_HOSTS="h1 h2")
        result, out = self.bench(env=env)
        self.assertIn("build host h1: busy (heavy lock held", result.stdout)
        self.assertIn("build host: h2", result.stdout)
        [build] = self.builds()
        self.assertEqual(Path(build["home"]), self.hosts / "h2")
        # The build runs at the stable path of its commit, and its tree returns to the scratch.
        self.assertEqual(build["cwd"], str(self.hosts / "h2" / "lanes" / "nsc-build-work" / self.sha / "src"))
        self.assertEqual(list((self.hosts / "h2" / "lanes" / "nsc-build-work").iterdir()), [])
        self.assertEqual(build["args"], ["build", "--release", "-p", "sparkles-server"])
        self.assertEqual((self.entry() / "sparkles").read_text(), f"built in {self.hosts / 'h2'} with ")
        record = (self.entry() / "build.txt").read_text()
        self.assertIn("build host: h2\n", record)
        self.assertIn("cargo args: \n", record)
        self.assertIn("rustc test", record)
        self.assertIn("built on h2", (out / "manifest.txt").read_text())
        log = (out / "logs" / "build-A.log").read_text()
        self.assertIn(f"scope nsc-build-{self.sha[:12]}-", log)
        self.assertIn("MemoryMax=", log)
        self.assertEqual(self.scratch("h2"), [], "the remote scratch directory must go")
        self.assertEqual(list((self.cache / "src").iterdir()), [])
        # The scope has the memory cap and swap limit, and the build holds the heavy lock.
        unit = next((self.hosts / "units").iterdir())
        props = unit.read_text().splitlines()[1]
        self.assertIn("MemorySwapMax=2G", props)
        self.assertIn("OOMPolicy=continue", props)
        # A second run reuses the entry without probing or building.
        before = self.ssh_log.read_text()
        result, out = self.bench(env=env)
        self.assertEqual(len(self.builds()), 1)
        self.assertEqual(self.ssh_log.read_text(), before)
        self.assertIn("built on h2", (out / "manifest.txt").read_text())

    def test_cargo_args_reach_the_remote_build(self):
        self.bench("--build-host", "h1", "--cargo-args", "--features text,geo --locked")
        [build] = self.builds()
        self.assertEqual(build["args"], ["build", "--release", "-p", "sparkles-server",
                                         "--features", "text,geo", "--locked"])

    def test_a_gate_unit_makes_a_host_busy_and_short_disk_skips_it(self):
        (self.hosts / "h1" / "gate-units").write_text("1")
        env = dict(self.env, SPARKLES_NSC_BUILD_HOSTS="h1 h2")
        result, _ = self.bench(env=env)
        self.assertIn("build host h1: busy (heavy lock free, 1 gate units)", result.stdout)
        self.assertEqual(Path(self.builds()[0]["home"]), self.hosts / "h2")
        (self.entry() / "build.txt").unlink()
        env = dict(env, SPARKLES_NSC_BUILD_MIN_FREE_GB="999999999")
        result, _ = self.bench(env=env, ok_stop=False)
        self.assertIn("is reachable with 999999999 GiB free", result.stderr)

    def test_the_benchmark_host_never_builds(self):
        # A list of the benchmark host alone does not fall back to a local build.
        result, _ = self.bench(env=dict(self.env, SPARKLES_NSC_BUILD_HOSTS=" bench "), ok_stop=False)
        self.assertIn("lists no host but the benchmark host", result.stderr)
        self.assertEqual(self.builds(), [])
        env = dict(self.env, SPARKLES_NSC_BUILD_HOSTS="bench h1")
        result, _ = self.bench(env=env)
        self.assertIn("build host bench: skipped", result.stdout)
        self.assertEqual(Path(self.builds()[0]["home"]), self.hosts / "h1")
        result, _ = self.bench("--build-host", "bench", ok_stop=False)
        self.assertIn("is the benchmark host", result.stderr)
        result, _ = self.bench("--build-host", "bench.example", ok_stop=False)
        self.assertIn("is the benchmark host", result.stderr)
        self.assertEqual(len(self.builds()), 1)

    def test_bad_build_host_values_are_refused_before_any_work(self):
        for value in ("-x", "a b", "h1;rm"):
            result, out = self.bench("--build-host", value, ok_stop=False)
            self.assertIn("--build-host is local, auto or a host name", result.stderr)
            self.assertFalse(out.exists())
        self.assertEqual(self.builds(), [])

    def test_every_host_busy_waits_for_the_first_ones_lock(self):
        # The lock outlasts the copies and the start, so the build finds it held.
        self.hold_lock("h1", seconds=6)
        self.hold_lock("h2")
        env = dict(self.env, SPARKLES_NSC_BUILD_HOSTS="h1 h2")
        started = time.monotonic()
        result, out = self.bench(env=env)
        self.assertGreater(time.monotonic() - started, 1.0)
        self.assertIn("every build host is busy", result.stdout)
        self.assertIn("--build-host local builds on this machine instead", result.stdout)
        self.assertEqual(Path(self.builds()[0]["home"]), self.hosts / "h1")
        self.assertIn("waiting for", (out / "logs" / "build-A.log").read_text())

    def test_no_reachable_host_stops_the_run(self):
        (self.hosts / "h1" / "unreachable").touch()
        env = dict(self.env, SPARKLES_NSC_BUILD_HOSTS="h1 nohost")
        result, _ = self.bench(env=env, ok_stop=False)
        self.assertIn("build host h1: unreachable", result.stdout)
        self.assertIn("pass --build-host local", result.stderr)
        result, _ = self.bench("--build-host", "nohost", ok_stop=False)
        self.assertIn("build host nohost is unreachable", result.stderr)
        self.assertEqual(self.builds(), [])
        # local overrides the configured hosts.
        self.bench("--build-host", "local", env=env)
        self.assertEqual(self.builds()[0]["home"], str(self.home))

    def test_the_config_file_supplies_the_hosts(self):
        config = self.root / "nsc-build.env"
        config.write_text("SPARKLES_NSC_BUILD_HOSTS=h2\nOTHER=ignored\n")
        self.bench(env=dict(self.env, SPARKLES_NSC_BUILD_CONFIG=str(config)))
        self.assertEqual(Path(self.builds()[0]["home"]), self.hosts / "h2")

    def test_a_failed_remote_build_leaves_no_scratch_and_no_entry(self):
        result, out = self.bench("--build-host", "h1", env=dict(self.env, FAKE_BUILD_FAIL="1"),
                                 ok_stop=False)
        self.assertIn("build of", result.stderr)
        self.assertIn("on h1 failed", result.stderr)
        self.assertEqual(self.scratch("h1"), [])
        self.assertFalse((self.entry() / "build.txt").exists())
        self.assertFalse((self.entry() / "sparkles").exists())

    def test_a_different_mbx_version_is_accepted_on_the_host(self):
        (self.repo / "mise.toml").write_text('[tools]\nmr-boxington = "1.0.0"\n')
        git = ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t",
               "-c", "commit.gpgsign=false"]
        subprocess.run([*git, "commit", "-qam", "pin"], check=True)
        self.sha = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True,
                                  text=True).stdout.strip()
        result, out = self.bench("--build-host", "h1")
        self.assertIn("mbx 9.9.9", (out / "logs" / "build-A.log").read_text())
        self.assertEqual(self.scratch("h1"), [])
        self.assertTrue(self.builds())

    def test_a_dropped_connection_neither_ends_nor_repeats_the_build(self):
        """The build runs detached, so lost replies of the start and of status polls are retried
        and the one build that started finishes and is fetched."""
        # The start command alone names the scratch directory right after the script. A lost
        # start reply is resolved by one status query, which finds the build started.
        for text, drops in (("nsc-build-remote.sh /", "1"), ("--status", "2")):
            with self.subTest(drop=text):
                (self.hosts / "drops").unlink(missing_ok=True)
                (self.entry() / "build.txt").unlink(missing_ok=True)
                env = dict(self.env, FAKE_SSH_DROP=text + ":2", FAKE_BUILD_DELAY="1")
                before = len(self.builds())
                result, out = self.bench("--build-host", "h1", env=env)
                self.assertEqual(len(self.builds()), before + 1, "exactly one build ran")
                self.assertEqual((self.hosts / "drops").read_text(), drops)
                log = (out / "logs" / "build-A.log").read_text()
                self.assertIn("closed by remote host", log)
                if drops == "2":
                    self.assertIn("polling the build on h1 failed with exit 255", log)
                self.assertIn("built on h1, sha256", log)
                self.assertEqual(self.scratch("h1"), [])

    def test_a_dropped_source_copy_is_retried_into_a_fresh_tree(self):
        env = dict(self.env, FAKE_SSH_DROP="tar -x -m:1")
        result, out = self.bench("--build-host", "h1", env=env)
        [build] = self.builds()
        self.assertTrue(build["cwd"].endswith("/src"), build["cwd"])
        log = (out / "logs" / "build-A.log").read_text()
        self.assertIn("copying the source of", log)
        self.assertIn("retrying", log)
        self.assertEqual(self.scratch("h1"), [])

    def test_lost_contact_fails_after_bounded_retries_and_stops_the_build(self):
        env = dict(self.env, FAKE_SSH_DROP="--status:1000", FAKE_BUILD_DELAY="30",
                   SPARKLES_NSC_BUILD_RETRIES="3")
        started = time.monotonic()
        result, out = self.bench("--build-host", "h1", env=env, ok_stop=False)
        self.assertLess(time.monotonic() - started, 20)
        self.assertIn("on h1 failed", result.stderr)
        log = (out / "logs" / "build-A.log").read_text()
        self.assertIn("polling the build on h1 failed 3 times", log)
        self.assertIn("lost contact with the build on h1", log)
        [unit] = list((self.hosts / "units").iterdir())
        self.assertIn(f"stop {unit.name}", (self.hosts / "systemctl.log").read_text())
        self.assertEqual(self.scratch("h1"), [])
        self.assertFalse((self.entry() / "build.txt").exists())

    def test_a_refused_start_fails_at_once_without_retries(self):
        env = dict(self.env, SPARKLES_NSC_BUILD_MEMORY_MAX="lots")
        result, out = self.bench("--build-host", "h1", env=env, ok_stop=False)
        log = (out / "logs" / "build-A.log").read_text()
        self.assertIn("not a whole number of G", log)
        self.assertIn("refused the build with exit 3", log)
        self.assertNotIn("retrying", log)
        self.assertEqual(self.builds(), [])
        self.assertEqual(self.scratch("h1"), [])

    def test_a_boat_screen_reuses_only_binaries_built_on_its_worker(self):
        """A Boat worker may have an older glibc than the host that built a cached binary."""
        record = self.root / "build.txt"
        program = '. "$1/scripts/nsc-build-host.sh"; BH_ARG=$2; bh_entry_host_ok "$3"'
        def ok(arg):
            return subprocess.run(["bash", "-c", program, "bash", str(self.repo), arg, str(record)],
                                  env=self.env, capture_output=True, text=True).returncode == 0
        for host, boat_ok in (("local (stanley)", False), ("h1", False), ("boat:bx_other", False),
                              ("boat:bx_worker", True)):
            record.write_text(f"built: now\ncargo args: \nbuild host: {host}\ncc: cc test\n")
            self.assertEqual(ok("boat:bx_worker"), boat_ok, host)
            self.assertTrue(ok(""), host)
            self.assertTrue(ok("h2"), host)
        record.unlink()
        self.assertFalse(ok("boat:bx_worker"))
        self.assertIn("bh_entry_host_ok", (self.repo / "scripts/nsc-bench.sh").read_text())

    def remote_dir(self):
        """A scratch directory on h1 with the scripts and a source tree, as bh_build leaves it."""
        d = self.hosts / "h1" / "lanes" / "nsc-build" / f"{self.sha}.direct"
        (d / "bin").mkdir(parents=True)
        (d / "src").mkdir()
        for name in ("nsc-build.sh", "nsc-build-remote.sh", "cargo-sparkles-task"):
            shutil.copy(self.repo / "scripts" / name, d / "bin" / name)
        return d

    def remote(self, *args, env=None):
        return subprocess.run(["bash", str(self.repo / "scripts/nsc-build-remote.sh"), *args],
                              env=dict(env or self.env, HOME=str(self.hosts / "h1")),
                              capture_output=True, text=True, timeout=60)

    def test_a_cancel_fences_a_stalled_start_and_a_later_one(self):
        """Cleanup that arrives while a start is still creating its scope waits for the start and
        then stops the scope, and a start after the cancellation starts nothing."""
        d = self.remote_dir()
        env = dict(self.env, FAKE_SCOPE_DELAY="1.5", FAKE_BUILD_DELAY="60")
        start = subprocess.Popen(["bash", str(self.repo / "scripts/nsc-build-remote.sh"), str(d), "u-fence", "600"],
                                 env=dict(env, HOME=str(self.hosts / "h1")), stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True, start_new_session=True)
        deadline = time.monotonic() + 10
        while not (d / "launched").exists():
            self.assertLess(time.monotonic(), deadline, "the start did not claim the build")
            time.sleep(0.02)
        cancel = self.remote("--cancel", str(d), "u-fence")
        out, err = start.communicate(timeout=30)
        self.assertEqual(cancel.returncode, 0, cancel.stderr)
        self.assertIn("started", out, err)
        # The stop came after the scope existed, so it ended the scope.
        calls = (self.hosts / "systemctl.log").read_text().splitlines()
        self.assertTrue(any(line.endswith("stop u-fence.scope") for line in calls), calls)
        self.assertTrue((self.hosts / "u-fence.scope.stopped").exists())
        scope = int((self.hosts / "units" / "u-fence.scope").read_text().split()[0])
        time.sleep(0.2)
        with self.assertRaises(ProcessLookupError):
            os.killpg(scope, 0)
        self.assertEqual(self.remote("--status", str(d), "u-fence", "0").stdout.splitlines()[0], "gone")
        # A start whose earlier attempt was lost is refused once the build is cancelled.
        (d / "launched").unlink()
        again = self.remote(str(d), "u-fence", "600", env=env)
        self.assertEqual(again.returncode, 3)
        self.assertIn("was cancelled", again.stderr)
        self.assertEqual(self.remote("--status", str(d), "u-fence", "0").stdout.splitlines()[0], "refused 3")

    def test_a_scope_that_systemd_refuses_is_a_refused_start(self):
        env = dict(self.env, FAKE_SCOPE_FAIL="1")
        d = self.remote_dir()
        result = self.remote(str(d), "u-fail", "600", env=env)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.remote("--status", str(d), "u-fail", "0").stdout.splitlines()[0], "refused 1")
        shutil.rmtree(d)
        result, out = self.bench("--build-host", "h1", env=env, ok_stop=False)
        log = (out / "logs" / "build-A.log").read_text()
        self.assertIn("refused the build with exit 1", log)
        self.assertNotIn("retrying", log)
        self.assertEqual(self.builds(), [])
        self.assertEqual(self.scratch("h1"), [])

    def test_a_build_without_runtime_left_is_not_started(self):
        env = dict(self.env, SPARKLES_NSC_BUILD_DEADLINE=str(int(time.time()) + 200))
        result, out = self.bench("--build-host", "h1", env=env, ok_stop=False)
        log = (out / "logs" / "build-A.log").read_text()
        self.assertIn("too little to start it", log)
        self.assertIn("refused the build with exit 75", log)
        self.assertFalse((self.hosts / "units").exists())
        self.assertEqual(self.builds(), [])
        self.assertEqual(self.scratch("h1"), [])
        # A deadline further away caps the scope's runtime, so that the scope ends, stop
        # included, by the deadline even when its start arrives and begins late.
        env = dict(self.env, SPARKLES_NSC_BUILD_DEADLINE=str(int(time.time()) + 1000))
        result, out = self.bench("--build-host", "h1", env=env)
        [unit] = list((self.hosts / "units").iterdir())
        runtime = int(unit.read_text().split("RuntimeMaxSec=")[1].split()[0])
        self.assertTrue(700 <= runtime <= 810, runtime)
        self.assertIn("TimeoutStopSec=30", unit.read_text())

    def test_the_runtime_counts_from_the_start_s_arrival(self):
        d = self.remote_dir()
        holder = subprocess.Popen(["flock", str(d / "launch.lock"), "sleep", "2"], start_new_session=True)
        self.holders.append(holder)
        while subprocess.run(["flock", "-n", str(d / "launch.lock"), "true"]).returncode == 0:
            time.sleep(0.02)
        result = self.remote(str(d), "u-late", "301")
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("too little to start the build", result.stderr)
        self.assertFalse((self.hosts / "units" / "u-late.scope").exists())
        self.assertEqual(self.remote("--status", str(d), "u-late", "0").stdout.splitlines()[0], "refused 3")

    def test_a_scope_that_begins_too_late_builds_nothing(self):
        """A start whose scope does not begin within the activation limit cancels the build, so
        the scope that the delayed launcher creates afterwards ends at once."""
        d = self.remote_dir()
        env = dict(self.env, SPARKLES_NSC_BUILD_ACTIVATION_SECONDS="1", FAKE_SCOPE_DELAY="2.5")
        result = self.remote(str(d), "u-slow", "600", env=env)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("did not start within 1 seconds", result.stderr)
        self.assertTrue((d / "cancelled").exists())
        deadline = time.monotonic() + 10
        while not (self.hosts / "units" / "u-slow.scope").exists():
            self.assertLess(time.monotonic(), deadline, "the delayed launcher did not create its scope")
            time.sleep(0.05)
        deadline = time.monotonic() + 10
        while self.remote("--status", str(d), "u-slow", "0").stdout.splitlines()[0] == "running":
            self.assertLess(time.monotonic(), deadline, "the late scope kept running")
            time.sleep(0.05)
        status = self.remote("--status", str(d), "u-slow", "0").stdout
        self.assertEqual(status.splitlines()[0], "refused 75")
        self.assertIn("was cancelled before its scope began", status)
        self.assertFalse((self.hosts / "build-started").exists())
        self.assertTrue((d / "src").is_dir())

    def test_a_publication_cut_short_or_in_progress_is_finished_once(self):
        """A retried copy waits for a publication in progress and accepts a tree that an attempt
        renamed into place before it could write its claim."""
        d = self.remote_dir()
        shutil.rmtree(d / "src")
        (d / "bin" / "ui-index.html").write_text("<!doctype html>")
        program = ('. "$1/scripts/nsc-build-host.sh"; BH_SEND_ATTEMPT=$4; bh_send_source h1 "$2" "$3"')
        def send(attempt):
            return subprocess.run(["bash", "-c", program, "bash", str(self.repo), str(d), self.sha, str(attempt)],
                                  env=self.env, capture_output=True, text=True, timeout=30)
        # An attempt that renamed its tree and ended before the claim.
        (d / "src").mkdir()
        (d / "src" / "first").write_text("first")
        self.assertEqual(send(1).returncode, 0)
        self.assertTrue((d / "src.claim").is_dir())
        self.assertEqual(sorted(p.name for p in (d / "src").iterdir()), ["first"])
        # A publication in progress holds the staging lock and finishes while the retry waits.
        shutil.rmtree(d / "src"); (d / "src.claim").rmdir()
        holder = subprocess.Popen(["flock", str(d / "stage.lock"), "sh", "-c",
                                   f"sleep 1; mkdir {d}/src && touch {d}/src/held && mkdir {d}/src.claim"],
                                  start_new_session=True)
        self.holders.append(holder)
        while subprocess.run(["flock", "-n", str(d / "stage.lock"), "true"]).returncode == 0:
            time.sleep(0.02)
        self.assertEqual(send(2).returncode, 0)
        self.assertEqual(sorted(p.name for p in (d / "src").iterdir()), ["held"])
        # Once the build has moved the tree away, the claim keeps a late attempt from publishing.
        shutil.rmtree(d / "src")
        self.assertEqual(send(3).returncode, 0)
        self.assertFalse((d / "src").exists())
        self.assertEqual(sorted(p.name for p in d.iterdir() if p.name.startswith("src")), ["src.claim"])

    def test_cleanup_of_an_unreachable_host_gives_up_within_its_budget(self):
        (self.hosts / "h1" / "unreachable").touch()
        program = ('. "$1/scripts/nsc-build-host.sh"; BH_RHOST=h1 BH_RDIR=/nowhere/lanes/nsc-build/x.y '
                   'BH_RUNIT=nsc-build-x-y; code=0; bh_cleanup || code=$?; echo "code $code"')
        result = subprocess.run(["bash", "-c", program, "bash", str(self.repo)],
                                env=dict(self.env, SPARKLES_NSC_BUILD_RETRIES="8"),
                                capture_output=True, text=True, timeout=60)
        self.assertIn("code 97", result.stdout, result.stderr)
        self.assertIn("stopping nsc-build-x-y.scope failed 4 times", result.stderr)
        self.assertIn("reconcile nsc-build-x-y.scope", result.stderr)
        self.assertEqual(self.ssh_log.read_text().count("--cancel"), 4)
        self.assertNotIn("rm -rf", self.ssh_log.read_text())

    def test_a_poll_dropped_after_part_of_its_reply_copies_the_log_once(self):
        env = dict(self.env, FAKE_SSH_DROP="--status:2", FAKE_SSH_DROP_BYTES="40", FAKE_BUILD_DELAY="1",
                   FAKE_BUILD_LOG_LINES="20")
        result, out = self.bench("--build-host", "h1", env=env)
        log = (out / "logs" / "build-A.log").read_text()
        self.assertEqual((self.hosts / "drops").read_text(), "2")
        self.assertEqual(log.count("building in /"), 1, log)
        self.assertEqual([line for line in log.splitlines() if line.startswith("log line")],
                         [f"log line {i:07d}" for i in range(20)])
        self.assertIn("built on h1, sha256", log)

    def test_a_log_larger_than_one_poll_is_copied_whole(self):
        lines = 200000
        env = dict(self.env, FAKE_BUILD_LOG_LINES=str(lines), SPARKLES_NSC_BUILD_RETRIES="2")
        result, out = self.bench("--build-host", "h1", env=env)
        log = (out / "logs" / "build-A.log").read_text()
        self.assertNotIn("polling the build on h1 failed", log)
        got = [line for line in log.splitlines() if line.startswith("log line")]
        self.assertEqual(len(got), lines)
        self.assertEqual(got[-1], f"log line {lines - 1:07d}")
        self.assertIn("built on h1, sha256", log)

    def test_a_boat_write_screen_keeps_entries_of_its_own_worker(self):
        program = '. "$1/scripts/nsc-build-host.sh"; BH_ARG=$2; echo "[$(bh_entry_key_suffix)]"'
        for arg, suffix in (("boat:bx_worker", "-bx_worker"), ("h1", ""), ("local", ""), ("", "")):
            result = subprocess.run(["bash", "-c", program, "bash", str(self.repo), arg], env=self.env,
                                    capture_output=True, text=True)
            self.assertEqual(result.stdout.strip(), f"[{suffix}]", arg)
        self.assertIn("key=$sha-$args_hash$(bh_entry_key_suffix)", (self.repo / "scripts/nsc-writes.sh").read_text())

    def test_an_interrupt_stops_the_scope_and_removes_the_scratch(self):
        out = self.root / "out-interrupted"
        process = subprocess.Popen(
            ["bash", str(self.repo / "scripts/nsc-bench.sh"), "--out", str(out), "--build-host", "h1",
             self.sha, self.sha], env=dict(self.env, FAKE_BUILD_DELAY="60"),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
        deadline = time.monotonic() + 20
        while not (self.hosts / "build-started").exists():
            self.assertLess(time.monotonic(), deadline, "the build did not start")
            time.sleep(0.05)
        [unit] = list((self.hosts / "units").iterdir())
        scope = int(unit.read_text().split()[0])
        os.killpg(process.pid, signal.SIGINT)
        process.communicate(timeout=20)
        self.assertEqual(process.returncode, 130)
        self.assertEqual(self.scratch("h1"), [])
        self.assertTrue(unit.name.endswith(".scope"))
        self.assertIn(f"stop {unit.name}", (self.hosts / "systemctl.log").read_text())
        time.sleep(0.2)
        with self.assertRaises(ProcessLookupError):
            os.killpg(scope, 0)
        self.assertFalse((self.entry() / "build.txt").exists())

    def test_writes_local_build_uses_managed_mbx_source_and_exact_feature_cache(self):
        (self.cache/'data').mkdir(parents=True)
        (self.cache/'data/people-100000.nt.zst').write_text('data')
        (self.repo/'crates/sparkles-core/tests/support').mkdir(parents=True)
        for index,flags in enumerate(('--features text --locked','--no-default-features')):
            out=self.root/f'local-writes-{index}'
            result=subprocess.run(['bash',str(self.repo/'scripts/nsc-writes.sh'),'--out',str(out),
                                   '--build-host','local','--cargo-args',flags,f'a={self.sha}',f'b={self.sha}'],
                                  env=self.env,capture_output=True,text=True,timeout=30)
            self.assertIn('executor acquire failed',result.stderr,result.stderr)
        builds=self.builds();self.assertEqual(len(builds),2)
        for build in builds:
            self.assertIsNone(build['target_override']);self.assertIsNone(build['cache_override'])
            self.assertNotIn('--target-dir',build['args'])
            self.assertIn('/writes-v2/src/',build['cwd'])
        self.assertEqual(builds[0]['args'][4:],['--features','text','--locked'])
        self.assertEqual(builds[1]['args'][4:],['--no-default-features'])
        entries=list((self.cache/'writes-v2/bin').iterdir());self.assertEqual(len(entries),2)
        for entry in entries:
            record=json.loads((entry/'build.json').read_text())
            self.assertEqual(record['build_policy'],'mbx-managed-v1')
            self.assertIn('managed target lease',record['command'])
        self.assertFalse((self.cache/'writes-v2/target').exists())
        self.assertEqual(list((self.cache/'writes-v2/src').iterdir()),[])

    def test_nsc_writes_builds_on_the_host_and_records_it(self):
        # nsc-writes stops when it tries to create the instance, after staging the binaries.
        (self.cache / "data").mkdir(parents=True)
        (self.cache / "data" / "people-100000.nt.zst").write_text("data")
        (self.repo / "crates/sparkles-core/tests/support").mkdir(parents=True)

        def writes(*args):
            self.runs += 1
            out = self.root / f"writes-{self.runs}"
            result = subprocess.run(["bash", str(self.repo / "scripts/nsc-writes.sh"), "--out", str(out),
                                     *args, f"a={self.sha}", f"b={self.sha}:--flag"], env=self.env,
                                    capture_output=True, text=True, timeout=60)
            self.assertIn("executor acquire failed", result.stderr, result.stderr)
            return result, out

        result, out = writes("--build-host", "h2", "--cargo-args", "--features text")
        [build] = self.builds()
        self.assertEqual(Path(build["home"]), self.hosts / "h2")
        self.assertEqual(build["args"][4:], ["--features", "text"])
        [entry] = (self.cache / "writes-v2" / "bin").iterdir()
        record = json.loads((entry / "build.json").read_text())
        self.assertEqual(record["build_host"], "h2")
        self.assertEqual(record["rustc"], "rustc test\n")
        self.assertIn("build host h2", record["command"])
        self.assertIn("built on h2", (out / "manifest.txt").read_text())
        self.assertEqual(self.scratch("h2"), [])
        self.assertEqual(list((self.cache / "writes-v2" / "src").iterdir()), [])
        # The verified entry is reused by a later run, which builds nothing.
        result, out = writes("--build-host", "h1", "--cargo-args", "--features text")
        self.assertEqual(len(self.builds()), 1)
        self.assertIn("built on h2", (out / "manifest.txt").read_text())


if __name__ == "__main__":
    unittest.main()
