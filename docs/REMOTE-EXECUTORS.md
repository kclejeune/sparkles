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
current toolchain manifest must match the handoff. MBX 1.29.1 and the matching child
Cargo shim use the shared host cache and each source directory's managed target.

Development credentials stay on the owned development worker. They are excluded from
source packets, result manifests and reusable snapshot seeds. GitHub Actions retains
its separate OIDC authentication. A mode-0600 worker environment file contains only
literal PATH and `MBX_REMOTE_TOKEN_FILE` assignments. The latter names a file and never
contains a token. Admission binds the environment file by digest.

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
worker file containing literal PATH and token FILE path. Its small
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
source directory. A gate receives a standard bounded Git bundle of the exact
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
