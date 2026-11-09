# Remote build and validation executors

Available local hosts take implementation, builds and gates first. Boat provides explicit
overflow capacity on an isolated 8-vCPU, 16-GiB `large` worker. One worker runs one heavy
job at a time. The configured benchmark host runs benchmarks only. Atlas remains
available for benchmarks and can be named explicitly as a quiet migration fallback.
The default gate host list contains Wally alone.

The remote commands accept `boat:bx_ID` for a running warm worker. They do not create,
resume, extend, snapshot or stop a sandbox. The lifecycle owner qualifies and admits a
worker once, supplies the admission receipt, and retains the deadline and stop guard
while several jobs reuse it. Each create, resume, fork and CI start counts against the
shared allowance of 200 starts per day. An owned-operation ledger does not establish the
account-wide total.

## Qualification boundary

The Boat transport and workload adapters are separate from provider qualification.
Namespace remains the mandatory query-screen fallback until Boat has real lifecycle,
build, transport, correctness and full-screen acceptance evidence. Local contract tests
exercise quoting, exact source identities, result bounds and cleanup. They do not
establish provider behavior or performance equivalence. Roadmap item 762 tracks the
remaining qualification.

A lifecycle owner must establish startup quiescence, actual tool and source identities,
cache authentication, available memory and disk, worker exclusivity and a finite
lifetime before handing over a worker. A ready provider state or `setupStatus: null`
does not establish tool readiness. The worker's bootstrap lock must be idle and its
current toolchain manifest must match the handoff. The provider's own desktop-session
processes are allowed by exact name and parent, and any other process of the worker's
user still holds startup open. Every refusal records a bounded reason code, as the
[startup closure](../scripts/boat/README.md#startup-closure) section describes. MBX 1.29.1 and the matching child
Cargo shim use the shared host cache and each source directory's managed target.

The memory check counts only memory that the kernel cannot reclaim. On the host this is
anonymous, shared, unevictable and unreclaimable kernel memory. For every ancestor
cgroup with a limit it is the cgroup's anonymous, shared, unevictable and unreclaimable
kernel memory from `memory.stat`. Each limit must leave room for a 12-GiB job, the
512-MiB metadata service and a 1-GiB host reserve. Page cache does not count, so a
worker whose slice holds several gigabytes of cache is admitted, and a slice that
exposes no `memory.stat` is refused.

`admit` resolves `mbx` and `rustc` through the same environment that jobs receive. That
environment puts the pinned mise MBX install on PATH, selects the pinned Rust toolchain
with `RUSTUP_TOOLCHAIN` and turns off automatic toolchain installation. A missing `mbx`,
an `mbx` found elsewhere or at another version, and a `rustc` at a version other than
the pin each refuse admission with the reason named, rather than running with the
rustup default.

Development credentials stay on the owned development worker. They are excluded from
source packets, result manifests and reusable snapshot seeds. GitHub Actions retains
its separate OIDC authentication. A mode-0600 worker environment file contains only
literal PATH, `RUSTUP_TOOLCHAIN`, `RUSTUP_AUTO_INSTALL`, `MBX_REMOTE_URL`,
`MBX_GC_MAX_TOTAL_SIZE` and `MBX_GC_MIN_FREE_SIZE` assignments. Admission binds the
environment file by digest.

Boat jobs build with MBX in local-only mode. The empty `MBX_REMOTE_URL` overrides the
worker's configured remote cache, and the job shell drops any inherited token setting.
The local store stays in MBX's default cache directory in the worker home, which the
normal stop's snapshot keeps for the next resume. Its 64 GiB budget holds two release
builds and the workspace test builds, and its 45 GiB free-space floor keeps the disk
headroom that qualification requires. `admit` asks the pinned `mbx` for these effective
settings and refuses a remote URL, a token setting or another budget. Laptops, gate
hosts and other ssh hosts keep their own MBX configuration.

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

The lifecycle owner can lengthen the lifetime of the same running worker with an
explicit extension, which is its own receipted act. The act adopts the worker only when
the local boot, the live guard invocation, the worker's boot recorded at qualification,
the provider environment version and a fresh environment and hook fact all match. It
sets a new end of at most 10,500 seconds after its own observation on both clocks, and
no end may fall more than 12 hours after the original controller intent. The provider
lifetime is set past the new end and confirmed before the act publishes a numbered
`sparkles.boat.lifetime-extension` version-1 receipt in the operation directory.
Admission and the guard apply only a consecutive chain of valid receipts that the
running guard accepted, by its own clocks, before the previous end. A malformed or unknown receipt refuses admission and
leaves the guard on the conservative original deadline. The guard holds the extension
lock when it decides to stop and records the stop trigger, and admission refuses a
stopping operation.
`admit --minimum` uses this measure. Environment preparation and each operation
also check both ends while they run, so a suspend during a transfer is counted.
`admit` prints a version-2 `sparkles.boat.adapter-observation` that records the
original wall and BOOTTIME ends. Its `expires_at` is the effective wall end at that
observation and is never later than the original. Scripts take a job runtime from a
saved observation with `remote-executor.py runtime FILE --reserve SECONDS`, which
measures both ends again and refuses another version, another boot or an exhausted
lifetime.

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
mise run boat:finish --operation "$OWNED_OPERATION" --job-closure "$PRIVATE_JOB_CLOSURE"
```

The Jena example requires prepared bindings. Gates require the explicit setup command
once per worker seed to copy public suites and prepare pytest. Boat jobs retain pinned
tools without implicit reinstall. Screens/builds request at least 1800 remaining seconds
and gates at least 7200 through `SPARKLES_REMOTE_MIN_LIFETIME`. Partitions request
`TEST_PARTITION_TIMEOUT` and check again after archive transfer. Boat caps follow the
original qualification deadline. The legacy `--duration` screen option controls
Namespace instance lifetime.

A worker kept warm past its jobs runs `boat:closure-watch`, which checks the jobs'
closure shortly before the guard's present end and finishes the operation, so the
guard's stop does not depend on the provider TTL. The
[scheduled closure](../scripts/boat/README.md#scheduled-closure) section describes it.

A Boat screen builds both revisions on the worker with mbx, because binaries built on a
host with a newer glibc do not run there. The executor does not choose the build host,
so the screen names the worker as its build host and passes two revisions rather than
binary paths. The screen keeps each binary in its local cache with a record of the host
that built it. A later screen with the same worker as its build host reuses a cached
binary only when that worker built it, so a repeated screen of the same revisions skips
both builds. A write screen with a Boat build host keeps its cache entries under a key
that names the worker, with the same effect.

```sh
mise run bench:remote --executor "$worker" --build-host "$worker" origin/main HEAD
```

A Boat screen and a Namespace screen of the same revisions therefore run different
binaries, and the Boat screen is judged by its ratios and noise against Namespace rather
than by identical binaries.

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

## Source, results and cleanup

Query and write commands keep the current worker scripts, A/B order, compiler checks,
answer checks, default query selection, rerun settings and regression thresholds. A
Boat screen holds the host's heavy lock across both arms, result collection and flagged
reruns. Each workload runs in an exact named memory-capped unit. Cleanup stops those
units before releasing its exact lock holder. Uncertain cleanup retains the lock and
source/results for the lifecycle owner to reconcile.

Each build receives an exact commit archive in a new source directory. Warm reuse
shares immutable compiler cache entries and cannot retain deleted files in a reused
source directory. While it holds the host's heavy lock, the build moves that directory
to a stable path named by the commit and a hash of its cargo flags, builds there and
moves it back. mbx records the predicted compiler actions of each checkout path, and a
rebuild of an unchanged commit at the same path finds its own predictions, so its crates
come from the store when the store still holds them. A rebuild at a new path takes the
latest predictions of the same command, which name the other revision's crates when a
screen alternates two revisions. The crates it then compiles embed the new path in their
metadata, so the crates that depend on them miss as well, and the rebuilt binary differs
from the first.

A remote build runs detached from the ssh connection in its named `nsc-build-*` scope.
The caller polls the scope over a fresh connection for its log and recorded exit
status. Each remote step is written so that repeating it does not change what an earlier
attempt did, and a step whose connection drops is retried a bounded number of times. A
start whose reply was lost is resolved from the build's recorded state, and a start the
host refuses fails at once. A cancellation takes the same lock as a start and fences any
later one, and a scope that begins after a cancellation builds nothing. A start whose scope
does not begin within 30 seconds cancels its own build. The caller gives the start an
absolute expiry, at which the remaining admitted lifetime falls to the closure watch's lead
and a minute, and the host enforces it on its own clock, which must agree with the
caller's. The scope's runtime limit is computed from the expiry at the launch, and a
watchdog in the scope stops it at the expiry less its 30-second stop time however late it
began. A Boat screen's lock and workload units get an absolute expiry at the same point of
the admitted lifetime. The worker refuses a unit that begins too close to it, the lock's
sleep ends before it, and a watchdog in each workload unit stops the unit 30 seconds
before it. When contact cannot be restored
within the retry bound, the build fails. Its cleanup then stops the scope, or names the
scope for reconciliation when it cannot reach the host within its own shorter bound. The closure watch's unit check covers the detached scope. A gate receives a standard bounded Git bundle of the exact
commit and comparison base. Temporary bundle refs live outside the caller's repository.
The first bounded seed preserves a complete valid history and the original merge-base
selection. Later jobs reuse an exact commit whose reachable object coverage matches
the local source. A changed commit uses an incremental standard bundle only after the
worker proves complete coverage of the exact prerequisite. Unknown or incomplete
coverage requires a bounded full seed or a clear refusal. Each gate source receipt
records the mode, bundle digest, prerequisite IDs and prerequisite coverage digest. Source packets exclude untracked credentials,
compiler targets, agent authentication and private evidence history.

Boat artifacts travel over its explicit SSH transport with byte-preserving stdin and
stdout. Individual files are checked by SHA-256. Sender and receiver share a finite operation
deadline capped by the original admission end. No blocked chunk renews that deadline.
Archive creation checks byte, member and expanded bounds during traversal and writing.
Screen and gate archive transfers are bounded to
4 GiB and 100,000 members, including expanded size, and refuse escaping paths, links and
device entries before extraction. Nextest archives use a 32-GiB expanded bound, set explicitly with
`TEST_PARTITION_ARCHIVE_MAX_BYTES` up to 64 GiB, and retain their SHA-256. Larger
workloads need an explicit artifact-transport extension. A Boat fetch with `--delete` first retains the previous tree beside its destination,
then validates a fresh staged tree before publishing it. Failed fetches leave no old
`gate.exit` at the current result path. Failed and interrupted runs retain their owned
worker directories and local receipt paths. Cleanup never stops another job or the
warm sandbox. The lifecycle owner decides retention and verified stop after evidence
and leases are closed.

Existing `bench:nsc`, `bench:nsc-writes`, `bench:jena-nsc`, `nsc:MACHINE` test shares and
SSH build hosts remain explicit compatibility paths. A failed Boat operation never
silently retries on Namespace or a local host. Historical benchmark records retain
their actual provider and host attribution.
