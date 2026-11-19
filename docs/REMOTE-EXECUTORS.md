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
The batch query screen runs on Boat and is the only screen of record. Namespace is not
used. Local contract tests exercise quoting, exact source identities, result bounds and
cleanup. They do not establish provider behavior or performance equivalence. Roadmap
item 762 tracks the remaining qualification of the other commands, and item 1134 tracks
a write screen.

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
exposes no `memory.stat` is refused. On a 16-GiB worker that leaves 2.1 GiB for memory
the kernel cannot reclaim. A worker restored from its snapshot holds 1.4 to 2.2 GiB of
it at qualification, about 0.9 GiB of which is the provider's restore filesystem, and
part of that is transient while files are restored. A reading that is refused only
because the headroom is short is therefore read again every 2 seconds for up to 60
seconds, before the probe observes any source, and never past the qualification phase's
120-second bound or the last 40 seconds of the metadata unit's runtime. A later
reading admits only when the reading before it admitted too, so one low reading never
admits, and the first reading that admits ends the wait at once. A missing or malformed
`memory.stat` is not read again. The readings are recorded with the qualification, or
with the refusal when the headroom never settles.

Every job unit on a Boat worker has a 12-GiB `MemoryMax`, which covers lane CI from
`scripts/remote-ci.sh`, a gate from `scripts/gate/gate.sh`, a screen's workload units
from `scripts/remote-screen.sh` and a screen build from `scripts/nsc-build-host.sh`.
`BOAT_JOB_MEMORY_GIB` in `scripts/boat/metadata-probe.py` holds the figure, and a test
checks that each of these caps matches it. The worker's user slice allows no swap, so a
job has none whatever its `MemorySwapMax` says. The largest single user is the rustc
that builds the server's unit-test binary, which reaches about 11.4 GiB.

`admit` resolves `mbx` and `rustc` through the same environment that jobs receive. That
environment puts the pinned mise MBX install on PATH, selects the pinned Rust toolchain
with `RUSTUP_TOOLCHAIN` and turns off automatic toolchain installation. A missing `mbx`,
an `mbx` found elsewhere or at another version, a `rustc` at a version other than the pin,
and a missing `rustdoc` or one at another version each refuse admission with the reason
named, rather than running with the rustup default. The worker image keeps rustup's
`rustdoc` proxy only in `/usr/local/cargo/bin`, so the job PATH includes it after
`/usr/local/bin` for the doctests.

Development credentials stay on the owned development worker. They are excluded from
source packets, result manifests and reusable snapshot seeds. GitHub Actions retains
its separate OIDC authentication. A mode-0600 worker environment file contains only
literal PATH, `RUSTUP_TOOLCHAIN`, `RUSTUP_AUTO_INSTALL`, `MBX_REMOTE_URL`,
`MBX_CACHE_DIR`, `MBX_GC_MAX_TOTAL_SIZE` and `MBX_GC_MIN_FREE_SIZE` assignments.
Admission binds the environment file by digest.

Boat jobs build with MBX in local-only mode. The empty `MBX_REMOTE_URL` overrides the
worker's configured remote cache, and the job shell drops any inherited token setting.
The provider's snapshot saves the worker home but not `~/.cache`, so MBX's default cache
directory would be empty after every resume. Every job therefore sets `MBX_CACHE_DIR` to
`~/.local/share/mbx-cache`, which holds the worker's single shared store with its managed
targets and incremental state, and the normal stop's snapshot keeps it. Its 40 GiB budget
leaves a cold workspace test build room on the large worker's 146 GiB disk, and its 45 GiB
free-space floor keeps the disk headroom that qualification requires. A build directory
that a running build uses stays whatever its size, so the budget does not bound a build
in progress. `admit` asks the pinned `mbx` for
these effective settings and refuses a remote URL, a token setting, another cache
directory or another budget. Laptops, gate hosts and other ssh hosts keep their own MBX
configuration.

A cold server build on the 8-vCPU worker takes about 29 minutes, and the store then holds
about 5 GiB of objects and a 5 GiB managed target. After a normal stop and a
resume onto a new machine, the store comes back unchanged, and a rebuild of the same
commit at its stable path takes about 4 minutes with no misses and gives the same binary.
The snapshot holds the store's files, so a full 40 GiB store takes more than a third of the
111 GB of own files that the provider restores on a large worker, and roadmap item 858
covers keeping the saved files under that limit.

A resume runs on a new machine and downloads the snapshot's files in the background. A
file whose content arrives while a build reads it changes under the compiler, and mbx
then discards the result and fails the build. `admit` therefore refuses a worker until
the provider reports it `hydrated`, which took about 90 seconds for a 45 GB snapshot. A
job started sooner is refused, and its caller admits it again once the worker is
hydrated.

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
start, so a slow dispatch can still carry a gate past the lead, and roadmap item 803 keeps
the absolute expiry open. Partitions request
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

The worker builds each revision in a scope without swap, with 6 compile jobs. The cap is
12 GiB by default, the job that qualification admits, but the server's final rustc peaks just
above 12 GiB at any job count, so that cap runs a cold release build out of memory. A 13 GiB
build needs a dedicated worker that runs nothing else, whose own job release checks that its
user slice leaves 14.5 GiB for the build, the metadata service and the host reserve, and whose
job environment sets `SPARKLES_BOAT_BUILD_MEMORY_MAX=13G`. The build scope keeps its own
13 GiB cap with `OOMPolicy=continue`, so reaching that cap kills a process of the build and
nothing outside it, but a host that runs short first leaves the choice to the kernel. The host
formula is not part of that check while the memory a resumed worker cannot reclaim varies
(item 1092). The setting takes a cap
from 8G to 13G and `SPARKLES_BOAT_BUILD_JOBS` a count from 2 to 8, and a value outside either
range is refused before the worker is admitted. A cold build of both revisions takes most of an hour, so the worker's
admitted lifetime must cover both builds, the screen and its reruns.

Binaries that a Boat screen builds therefore differ from those built for another host,
and timings from different providers or hosts are never compared. Item 1133 tracks a
bias of a few percent between the Boat and Namespace screens of the same batches.

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

The command bundles the commit and, for `--changed`, its merge base with the base, checks
both on the host and starts the job as the user unit `gate-NAME`. The job script
`scripts/gate/remote/ci-job.sh` runs the disk cleanup when the disk is 90% used, takes the
heavy lock `~/gate/.lock`, adds a worktree, runs the check and removes the worktree, all
inside that unit. On a Boat worker the job's end is fixed before the start at
`REMOTE_CI_RESERVE` seconds before the admitted end, 1200 by default. The worker computes
the unit's runtime from that end immediately before the start and refuses with under
`REMOTE_CI_MIN_SECONDS` left, 600 by default. Because the runtime counts from the unit's
activation, the job also stops its own unit at that end and takes every wait from it, and the
unit's shutdown is bounded at 60 seconds, so the closure watch finds the job closed. Starts on
one host are serialized, and a job of the same name starts only after the earlier unit has
fully stopped. Under the heavy lock a job on a Boat worker needs `REMOTE_CI_MIN_FREE_GIB`
free after the disk cleanup, 85 by default, and ends with exit 4 rather than start with
less. A cold workspace test build of the lane CI added 67.8 GiB to the large worker's disk,
and the default adds 25% to that. The build cache does not help a second run on the large
worker, because the test build stores about 48 GiB, more than the store's 40 GiB budget, so
a rerun of the same commit measured 3 hits in 343 lookups and grew the disk by 65.7 GiB. The worker's disk timer stops any running job at 95% use, which is a recovery rather
than a guarantee, since it checks once a minute and a build can fill the disk sooner. A job on an ssh host may run 6
hours. The benchmark host is refused. `SPARKLES_REMOTE_CI_HOST` sets the default host. Results
land in `.artifacts/remote-ci/NAME`, with the exit status and the logs.

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
the admitted lifetime. The worker refuses a unit that begins too close to it, and a
watchdog in each unit stops the unit 30 seconds before it. The lock starts its watchdog
before it answers through its readiness FIFO, and its answer waits at most 15 seconds for
a reader. When contact cannot be restored
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

The `bench:nsc`, `bench:nsc-writes` and `bench:jena-nsc` tasks, the `nsc` executor and the
`nsc` and `nsc:MACHINE` test shares are retired and unsupported, and item 1135 removes
them. The `nsc-*` scripts that `bench:remote`, `bench:remote-writes` and the build hosts
run keep their names and stay in use, and SSH build hosts remain a local build path. A
failed Boat operation never silently retries on another provider or a local host.
Historical benchmark records retain their actual provider and host attribution.
