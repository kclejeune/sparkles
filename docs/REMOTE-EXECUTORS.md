# Remote build and validation executors

Available local hosts take implementation, builds and gates first. Boat provides explicit
overflow capacity on an isolated 8-vCPU, 16-GiB `large` worker running one heavy job at a
time. The configured benchmark host runs benchmarks only, Atlas remains available for
benchmarks (or as an explicitly permitted quiet migration fallback), and Wally is the
default gate host.

| Rule | Policy |
|---|---|
| Default placement | Local build and gate hosts first; Boat (`boat:bx_ID`) for explicit overflow or isolation. |
| Benchmark host | Runs benchmarks only, never builds, CI or gates. |
| Worker concurrency | One heavy job per 8-vCPU, 16-GiB Boat worker under a 12-GiB `MemoryMax` (no swap). |
| Sandbox lifecycle | Remote commands never create, resume, extend, snapshot or stop a worker; the lifecycle owner qualifies it once and holds the deadline and stop guard. |
| Quota & provider boundary | Each create, resume, fork and CI start counts against the shared 200 starts/day allowance. Failed Boat operations never silently retry on another provider or host. |

* [Qualification boundary](#qualification-boundary)
* [Qualification and job admission](#qualification-and-job-admission)
* [Warm worker commands](#warm-worker-commands)
  * [Remote CI](#remote-ci)
* [Source, results and cleanup](#source-results-and-cleanup)

## Qualification boundary

The Boat transport and workload adapters are separate from provider qualification. The
batch query screen runs on Boat and is the only screen of record; Namespace is retired and
not used. Local contract tests check quoting, exact source identities, result bounds and
cleanup, without establishing provider behavior or performance equivalence. Remaining
command qualification and a write screen are tracked in [ROADMAP.md](ROADMAP.md) (items
762 and 1134).

A lifecycle owner must establish startup quiescence, actual tool and source identities,
cache authentication, available memory and disk, worker exclusivity and a finite lifetime
before handing over a worker. A ready provider state or `setupStatus: null` does not prove
tool readiness. The worker's bootstrap lock must be idle and its toolchain manifest must
match the handoff. Only the provider's own desktop-session processes are permitted by exact
name and parent; any other user process keeps startup open. Every refusal records a bounded
reason code ([startup closure](../scripts/boat/README.md#startup-closure)). MBX 1.29.1 and
its matching child Cargo shim use the shared host cache and each source directory's managed
target.

### Memory and headroom checks

The memory check counts only unreclaimable memory: anonymous, shared, unevictable and
unreclaimable kernel memory on the host, and the same fields from `memory.stat` for every
ancestor cgroup with a limit. Each limit must leave room for a 12-GiB job, the 512-MiB
metadata service and a 1-GiB host reserve. Page cache is excluded, so a slice holding
several gigabytes of cache still qualifies; a slice without `memory.stat` is refused.

On a 16-GiB worker this leaves 2.1 GiB for unreclaimable host memory. A worker restored
from a snapshot starts with 1.4 to 2.2 GiB of unreclaimable memory (about 0.9 GiB in the
provider's restore filesystem, part of it transient during restore). If admission fails
only on memory headroom, the probe re-reads every 2 seconds for up to 60 seconds before
inspecting any source, bounded by the 120-second qualification phase and the last 40
seconds of the metadata unit. Admission requires two consecutive passing readings so a
single dip never admits; a missing or malformed `memory.stat` fails immediately. All
readings are recorded with the qualification or refusal.

Every job unit on a Boat worker carries a 12-GiB `MemoryMax` — covering lane CI
(`scripts/remote-ci.sh`), gates (`scripts/gate/gate.sh`), screen workloads
(`scripts/remote-screen.sh`) and screen builds (`scripts/nsc-build-host.sh`).
`BOAT_JOB_MEMORY_GIB` in `scripts/boat/metadata-probe.py` defines that cap, and a test
enforces consistency across scripts. The worker's user slice allows no swap regardless of
`MemorySwapMax`. The largest single process is `rustc` building the server's unit-test
binary, which peaks near 11.4 GiB.

### Toolchains, local-only MBX cache and hydration

`admit` resolves `mbx` and `rustc` in the same environment jobs receive: the pinned mise
MBX install on `PATH`, `RUSTUP_TOOLCHAIN` fixed to the pin and automatic toolchain
installation disabled. Any missing or mismatched `mbx`, `rustc` or `rustdoc` refuses
admission by name instead of falling back to the rustup default. Because the worker image
places rustup's `rustdoc` proxy only in `/usr/local/cargo/bin`, the job `PATH` appends that
directory after `/usr/local/bin` for doctests.

Development credentials remain on the owned worker and never enter source packets, result
manifests or reusable snapshot seeds. GitHub Actions keeps its separate OIDC setup. A
mode-0600 worker environment file contains only literal `PATH`, `RUSTUP_TOOLCHAIN`,
`RUSTUP_AUTO_INSTALL`, `MBX_REMOTE_URL`, `MBX_CACHE_DIR`, `MBX_GC_MAX_TOTAL_SIZE` and
`MBX_GC_MIN_FREE_SIZE` assignments, bound by digest at admission.

Boat jobs run MBX in local-only mode: an empty `MBX_REMOTE_URL` overrides any worker remote
cache, and the job shell clears inherited tokens. Because provider snapshots preserve the
home directory but not `~/.cache`, jobs point `MBX_CACHE_DIR` at
`~/.local/share/mbx-cache` so normal stops retain the shared store, managed targets and
incremental state. Its 40-GiB budget fits a cold workspace build on the 146-GiB disk, and
its 45-GiB free-space floor preserves qualification headroom; active builds are never
evicted mid-run. `admit` verifies these effective MBX settings and refuses any mismatch.
Laptops, gate hosts and other SSH hosts keep their own MBX settings.

A cold server build on an 8-vCPU worker takes about 29 minutes, leaving roughly 5 GiB of
store objects and a 5-GiB managed target. After a normal stop and resume onto a new
machine, rebuilding the same commit at its stable path takes about 4 minutes with zero
misses and produces the same binary. Because a full 40-GiB store consumes over a third of
the 111 GB of own files a large worker restores, keeping saved files under that cap is
tracked in [ROADMAP.md](ROADMAP.md) (item 858).

Resuming onto a new machine downloads snapshot files in the background. If a file arrives
while a compiler reads it, MBX discards the action and fails the build. `admit` therefore
refuses any worker until the provider reports it `hydrated` (about 90 seconds for a 45-GB
snapshot). Callers re-admit once hydration completes.

## Qualification and job admission

The tracked [owned-worker package](../scripts/boat/README.md) supplies the stable
qualification, environment preparation and verified-stop sequence. Its FALSE examples
cannot authorize a provider operation. Private independently reviewed profile and
release files bind the actual worker, environment, source seal, account limits,
nonsecret cache endpoint/namespace, startup scope and local guard. No live identity is
selected by maintained defaults.

Qualification publishes an immutable `sparkles.boat.qualified-worker-admission`
version-1 receipt only after an actual explicit job handoff. The shared
`validate_admission` consumer binds the exact profile, worker/environment, effective UTC
deadline, local guard boot/invocation, actual manifest/bootstrap lock and source/job
review hashes. The receipt's `runtime_env_file` remains null.

`boat:job-env` validates that original receipt and prepares a deterministic mode-0600
worker file containing literal PATH, Rust and local-only MBX values. Its small
`sparkles.boat.job-admission` version-1 record references the unchanged qualification
file and its SHA-256, plus actual environment path, SHA-256, byte count, mode and UID.
It adds no authority or lifetime. Adapters validate the original qualification with the
shared consumer and check the environment proof before dispatch. They pass known
values literally and never source arbitrary shell values.

Set `SPARKLES_BOAT_PROFILE` to the private reviewed profile and
`SPARKLES_BOAT_ADMISSION` to the prepared job-admission record. Either setting can also
name a directory of exact-worker `bx_ID.json` files. Unknown schemas, changed original
qualification bytes, profile/source drift, a replaced local guard and insufficient
remaining lifetime are refused.

Remaining lifetime comes from the original operation receipts on every call. The
controller intent, guard observation and handoff give an original wall end and the
same instant on `CLOCK_BOOTTIME` for the guard's boot and invocation. Each check
takes the earlier of the two remaining times and subtracts the 30-second closure
reserve, so a wall clock moved backward cannot lengthen the lifetime and a suspend
counts even when the wall clock misses it. A different boot refuses the admission.
A warm worker reused across many jobs and lanes keeps that one original end, since
every job-admission record references the same qualification and receipts. No call,
job environment, lane or wall-clock change renews it.

The lifecycle owner can extend the same running worker's lifetime through an explicit,
receipted act. Adoption requires matching local boot, live guard invocation, qualification
worker boot, provider environment version and fresh environment/hook facts. Each extension
sets a new end at most 10,500 seconds ahead on both clocks, capped at 12 hours from the
original controller intent. The provider lifetime is updated and confirmed before writing a
numbered `sparkles.boat.lifetime-extension` version-1 receipt to the operation directory.

Admission and the guard apply only a consecutive chain of valid receipts accepted by the
running guard before the previous end. Any malformed or unknown receipt refuses admission
and keeps the conservative original deadline. When the guard decides to stop, it holds the
extension lock and records the stop trigger, so admission refuses a stopping operation.
`admit --minimum`, environment preparation and active transfers all check both clocks so
host suspends are counted. `admit` outputs a version-2 `sparkles.boat.adapter-observation`
carrying both original ends and an `expires_at` never later than the original; scripts
derive job runtimes with `remote-executor.py runtime FILE --reserve SECONDS`, refusing
other versions, other boots or exhausted lifetimes.

## Warm worker commands

The [complete sequence](../scripts/boat/README.md#one-warm-worker-sequence) runs
`boat:qualify` with explicit unit/worker/profile/release, observes qualification, prepares
`boat:job-env`, runs several ordinary jobs and finishes with actual closure evidence.
After setting the two validated receipt paths, ordinary jobs need no remote shell
preparation.

```sh
worker="boat:$OWNED_WORKER"
python3 scripts/remote-executor.py admit "$worker" --minimum 1800
scripts/build-remote.sh --host "$worker" --out .artifacts/builds/candidate HEAD
mise run bench:remote --executor "$worker" path/to/baseline path/to/candidate
mise run bench:remote-writes --executor "$worker" base=path/to/baseline head=path/to/candidate
mise run bench:jena-remote --executor "$worker" --no-build -- --arm sparkles=sparkles-mem --people 100 --small-people 10 --rounds 1 --seconds 1 --warmup-seconds 1 --threads 1 --jvm-heap 2g
mise run test:partitioned --hosts "$worker" --no-local
scripts/gate/setup-host.sh "$worker" wally
scripts/gate/gate.sh --host "$worker" --helper none HEAD
python3 scripts/remote-executor.py exec "$worker" 'python3 -I ~/gate/bin/disk_clean.py --force --log ~/gate/disk-clean.jsonl'
mise run boat:finish --operation "$OWNED_OPERATION" --job-closure "$PRIVATE_JOB_CLOSURE"
```

The Jena example requires prepared bindings. Gates require the explicit setup command
once per worker seed to copy public suites and prepare pytest. Boat jobs retain pinned
tools without implicit reinstall. Screens/builds request at least 1800 remaining seconds
and gates at least 7200 through `SPARKLES_REMOTE_MIN_LIFETIME`. A gate unit's runtime is
computed to end `GATE_BOAT_RESERVE` seconds before the admitted end, 1200 by default, which
leaves the closure watch its 900-second lead. The gate's start runs on the worker in the
transient scope `gate-prep-NAME`, which systemd ends with all its processes
`GATE_BOAT_PREP_SECONDS` after it begins, 900 by default, and the runtime adds that bound,
so a late activation still ends in time. `gate.sh` computes the runtime before it sends the
start, so a slow dispatch can still carry a gate past the lead ([ROADMAP.md](ROADMAP.md),
item 803). Partitions request
`TEST_PARTITION_TIMEOUT` and check again after archive transfer. Boat caps follow the
original qualification deadline. The `--duration` screen option applies only to the
retired Namespace executor and has no effect on a Boat worker.

A worker kept warm past its jobs runs `boat:closure-watch`, which checks the jobs'
closure shortly before the guard's present end and finishes the operation, so the
guard's stop does not depend on the provider TTL. The
[scheduled closure](../scripts/boat/README.md#scheduled-closure) section describes it.

A worker frees finished work when its root disk reaches 90% use, the level at which the
provider warns the maintainer, and before the job-closure receipt of every stop, which the
closure watch runs itself and the owner runs before `finish`. The cleanup removes finished
gate worktrees and runs `mbx gc`, and keeps the store's live entries and every active
job's files. The
[disk cleanup](../scripts/boat/README.md#disk-cleanup) section describes where it runs.

A Boat screen builds both revisions on the worker with mbx, because binaries built on a
host with a newer glibc do not run there. The executor does not choose the build host,
so the screen names the worker as its build host and passes two revisions rather than
binary paths. When the build host is also the executor, each binary stays on the worker in
its build directory, since a release binary is about 1 GB and the operator's link to a Boat
worker can carry only a few hundred KB/s. Only the build records come back. The worker reads
the binary's SHA-256, its compilers and whether it is dynamically linked, so it needs
`readelf`, `ldd` and `sha256sum`. The local cache entry, under a key that names the worker,
records the binary's path and SHA-256 there. A later screen on the same worker reuses the
entry only after the worker confirms that the file still has that SHA-256, and builds again
when it is gone or changed. The screen links each binary into its own directory on the
worker, and the remote run records the SHA-256 of what it runs, which must match the
manifest. A write screen, or a screen whose build host is another worker, still copies each
binary back and keeps it in the local cache with a record of the worker that built it.

```sh
mise run bench:remote --executor "$worker" --build-host "$worker" origin/main HEAD
```

The worker builds each revision in a swapless scope with 6 compile jobs. The default
12-GiB cap admitted by qualification is tight because the server's final `rustc` peaks
above 12 GiB at any job count, which can OOM a cold release build. A 13-GiB build requires
a dedicated worker running nothing else, a job release verifying 14.5 GiB of headroom for
the build, metadata service and host reserve, and `SPARKLES_BOAT_BUILD_MEMORY_MAX=13G` in
the job environment. The build scope keeps `OOMPolicy=continue` so exceeding 13 GiB kills
only a build process (if the host runs short first, the kernel chooses). Because resumed
unreclaimable memory varies, the host formula is not part of that check
([ROADMAP.md](ROADMAP.md), item 1092). `SPARKLES_BOAT_BUILD_MEMORY_MAX` accepts `8G` to
`13G` and `SPARKLES_BOAT_BUILD_JOBS` accepts `2` to `8`; out-of-range values fail before
admission. Because a cold build of both revisions takes most of an hour, the worker's
admitted lifetime must cover both builds, the screen and its reruns.

Binaries built on a Boat worker differ from binaries built elsewhere, so timings from
different providers or hosts are never compared. Boat and Namespace screens of the same
two batches differed by roughly 10% in geometric mean ([ROADMAP.md](ROADMAP.md), item
1133 settles the difference on an Intel host).

The operator establishes actual native/cgroup/lease closure before supplying the fresh
job-closure receipt. `boat:finish` validates that evidence and observes the guard's
verified normal stop and clean exit. A submitted or unresolved request is a failure to
establish release. Reconcile its existing worker/operation/unit identities before any
new action. A new receipt never silently renews an in-flight transfer deadline. The
raw Boat command stream shares that original deadline with a 30-second reserve. It
bounds stdin at 64 GiB, stdout at 4 GiB and stderr at 64 MiB. Stalled producer and
consumer pipes fail within that bound, and every terminal outcome retires the exact
owned local CLI process group. Remote native jobs remain owned by their named units
and require their separate verified closure.

From the moment the local CLI process starts until its teardown ends, Ctrl-C is
recorded rather than raised at an arbitrary point. A transfer acts on it at its next
loop check, within about 0.2 seconds, and then tears down. Teardown has a fixed
five-second window that an interrupt never resets. A Ctrl-C that arrives during
teardown is counted while TERM, KILL, reaping and the empty-group check finish, and
the pipes close even when closure stays unconfirmed. A transfer error stays the
reported failure and carries the interrupt count. A command that would otherwise
have returned is cancelled with `KeyboardInterrupt` once teardown ends. An
unconfirmed closure after a zero exit is refused rather than reported as success.

### Remote CI

`scripts/remote-ci.sh` runs a lane's `scripts/ci.sh --fast --changed <base>` or a batch's
`scripts/premerge.sh` on a gate host or an admitted Boat worker.

```sh
scripts/remote-ci.sh --host "$worker" --base origin/main HEAD
scripts/remote-ci.sh --host "$worker" --premerge HEAD
scripts/remote-ci.sh --host "$worker" --status ci-1234abcd-5678ef90
scripts/remote-ci.sh --host "$worker" --fetch ci-1234abcd-5678ef90
```

The command bundles the commit and, for `--changed`, its merge base with the target base,
verifies both on the host and launches `scripts/gate/remote/ci-job.sh` as user unit
`gate-NAME`. Inside that unit the job cleans disk at 90% usage, acquires `~/gate/.lock`,
creates a worktree, runs the check and removes the worktree. Starts on one host are
serialized, and a job with an existing name starts only after the earlier unit stops.

* **Runtime bounds.** On a Boat worker the job ends `REMOTE_CI_RESERVE` seconds (default
  1200) before the admitted end, refusing to start with fewer than `REMOTE_CI_MIN_SECONDS`
  (default 600) remaining. The job also stops its own unit at that deadline (with a
  60-second shutdown bound) so the closure watch finds it closed. SSH hosts allow up to 6
  hours; the benchmark host is always refused.
* **Disk floors and monitoring.** Under the heavy lock every job requires
  `REMOTE_CI_MIN_FREE_GIB` free (default 85), exiting 4 if still short after Boat disk
  cleanup or SSH `mbx gc`. On SSH hosts, the job also checks free space every
  `REMOTE_CI_WATCH_SECONDS` (default 30) and again after any failure, stopping with exit 5
  if free space drops below `REMOTE_CI_RUN_FLOOR_GIB` (default 4; server tests require
  1 GiB). Either disk refusal writes `disk-below-floor` to `outcome`, and the job reserves
  1 MiB up front so it can still record outcome files on a full disk.
* **Process cleanup and temporary space.** SSH units run delegated with the check in a
  child cgroup so every descendant is killed before copying results and deleting the
  worktree; if processes cannot be confirmed gone, the job exits 6 and keeps the worktree
  for cleanup. SSH checks use a sibling `~/gate/tmp-NAME.XXXXXX` as `TMPDIR` instead of
  `/tmp`. Boat workers keep their own temporary directory and their 95%-disk watchdog
  timer (a 1-minute recovery check, not a hard guarantee).
* **Boat cache sizing.** A cold workspace test build adds roughly 67.8 GiB to a large
  worker's disk and stores about 48 GiB of outputs — exceeding the 40-GiB MBX store budget
  — so repeating the same commit hit only 3 of 343 lookups and grew the disk by 65.7 GiB.
* **Results and invocation.** Each start records a unique ID so waiting callers only fetch
  their own run into `.artifacts/remote-ci/NAME.ID` (with `NAME` linked to the newest).
  `SPARKLES_REMOTE_CI_HOST` sets the default host. `--cmd` runs one command under the
  commit's pinned compiler instead of `ci.sh`, and `--worktree` checks uncommitted changes
  (combined by `mise run check:remote`).

## Source, results and cleanup

Query and write commands keep the current worker scripts, A/B order, compiler checks,
answer checks, default query selection, rerun settings and regression thresholds. A Boat
screen holds the host's heavy lock across both arms, result collection and flagged reruns,
running each workload in an exact named memory-capped unit. Cleanup stops those units
before releasing the lock; uncertain cleanup retains the lock and artifacts for the
lifecycle owner to reconcile.

Each build receives an exact commit archive in a fresh source directory so deleted files
never leak across warm runs. While holding the heavy lock, the build moves the directory to
a stable path keyed by commit and Cargo flag hash, builds there and moves it back. Because
MBX indexes predicted compiler actions by checkout path, rebuilding an unchanged commit at
its stable path reuses cached crates and produces an identical binary, whereas alternating
two revisions at fresh paths causes path-embedded metadata misses and distinct binaries.

### Detached builds and gate bundles

Remote builds run detached from SSH in named `nsc-build-*` scopes, polled over fresh
connections for logs and exit status. Every remote step is idempotent with bounded retries
on dropped connections; lost start replies resolve from recorded state, refused starts fail
immediately, and cancellation takes the start lock to fence late scopes. A start whose
scope does not begin within 30 seconds cancels itself.

Both remote builds and Boat screen lock/workload units carry an absolute expiry (the
admitted lifetime minus the closure watch's lead and one minute), checked against the
host's clock. Each scope or unit runs an internal watchdog that stops it 30 seconds before
expiry regardless of activation delay; the lock unit starts its watchdog before signaling
its readiness FIFO (waiting at most 15 seconds for a reader). If contact is lost past the
retry bound, cleanup stops the scope or names it for reconciliation, and the closure watch
covers detached scopes.

Gates transfer a bounded Git bundle of the commit and comparison base using temporary refs
outside the caller's repository. The first seed preserves complete history and merge-base
selection; later jobs reuse an exact verified commit or send an incremental bundle only
after proving complete coverage of its prerequisite (otherwise falling back to a bounded
full seed or refusing). Each gate source receipt records the mode, bundle digest and
prerequisite digests. Source packets exclude untracked credentials, compiler targets, agent
authentication and private evidence history.

### Artifact transport and retired Namespace tasks

Boat artifacts travel over explicit SSH transport with byte-preserving stdin/stdout,
per-file SHA-256 verification and a shared deadline capped by the original admission end
(stalled transfers never extend it). Archive creation validates byte, member and expanded
limits during traversal: screen and gate transfers cap at 4 GiB and 100,000 members
(refusing escaping paths, links and device entries), while nextest archives use a 32-GiB
expanded cap (`TEST_PARTITION_ARCHIVE_MAX_BYTES` up to 64 GiB) with SHA-256 verification.

A Boat fetch with `--delete` preserves the previous tree beside the destination and
validates a staged tree before swapping, so failed fetches never leave a stale `gate.exit`.
Failed or interrupted runs retain their worker directories and local receipts. Cleanup
never stops another job or the warm sandbox; the lifecycle owner manages retention and
verified stop after evidence and leases close.

The `bench:nsc`, `bench:nsc-writes` and `bench:jena-nsc` tasks, the `nsc` executor and the
`nsc` and `nsc:MACHINE` test shares are retired and unsupported ([ROADMAP.md](ROADMAP.md),
item 1135). The underlying `nsc-*` helper scripts used by `bench:remote`,
`bench:remote-writes` and SSH build hosts keep their names and stay in use. Failed Boat
operations never fall back silently to another provider or local host, and historical
benchmark records retain their original provider and host attribution.
