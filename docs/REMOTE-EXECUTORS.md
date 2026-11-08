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

## Admission receipt

Set `SPARKLES_BOAT_ADMISSION` to the lifecycle owner's bounded JSON receipt, or to a
directory containing one `bx_ID.json` receipt for each explicitly selected worker. Schema 1
contains the following fields. Unknown schemas and insufficient remaining lifetimes are
refused before a job starts.

| Field | Meaning |
| --- | --- |
| `schema` | The integer 1. |
| `worker` | The exact `bx_ID` sandbox identity. |
| `environment`, `environment_version` | The qualified Sparkles development environment and its integer version. |
| `expires_at` | The earlier conservative worker and owner deadline as UTC epoch seconds. |
| `owner` | The identifier of the lifecycle owner. |
| `guard_unit`, `guard_boot_id`, `guard_invocation_id` | The fully suffixed local systemd service, local boot identity and exact invocation that owns stop reconciliation. A same-name replacement is refused. |
| `toolchain_manifest_path`, `toolchain_manifest_sha256` | The absolute worker path and observed SHA-256 of its qualified toolchain manifest. |
| `bootstrap_lock_path` | The absolute worker bootstrap lock path checked for quiescence. |
| `worker_env_path`, `worker_env_sha256` | The absolute worker path and SHA-256 of its nonsecret environment file. |

`python3 scripts/remote-executor.py admit boat:bx_ID --minimum 1800` checks the receipt,
the local guard invocation, selected current provider metadata and physical worker
tools. It prints selected nonsecret identities for a run record. The lifecycle owner
supplies the receipt after qualification. This command cannot turn an unqualified
worker into a qualified one.

## Warm worker sequence

The stable tracked lifecycle package remains an integration dependency under item 762.
The current lifecycle contract uses `run --release RELEASE`
to acquire and qualify one worker. Its handoff supplies the compact receipt and exact
sandbox ID. Keep the controller's local guard active throughout the following jobs.
Once the receipt is available, the ordinary commands need no remote shell preparation.

```sh
export SPARKLES_BOAT_ADMISSION=/absolute/path/to/admission.json
worker=boat:bx_ID
python3 scripts/remote-executor.py admit "$worker" --minimum 1800
scripts/build-remote.sh --host "$worker" --out .artifacts/builds/candidate HEAD
mise run bench:remote --executor "$worker" path/to/baseline path/to/candidate
mise run bench:remote-writes --executor "$worker" base=path/to/baseline head=path/to/candidate
mise run bench:jena-remote --executor "$worker" --no-build -- --arm sparkles=sparkles-mem --people 100 --small-people 10 --rounds 1 --seconds 1 --warmup-seconds 1 --threads 1 --jvm-heap 2g
mise run test:partitioned --hosts "$worker" --no-local
scripts/gate/setup-host.sh "$worker" wally
scripts/gate/gate.sh --host "$worker" --helper none HEAD
```

The Jena example uses prepared bindings and a small comparison workload. Gates require the
explicit setup command once per worker seed to copy public conformance suites and
prepare the pytest environment. The admitted tools stay pinned during Boat gate and
partition jobs. Jobs refuse inadequate lifetime instead of extending a sandbox or
selecting another provider. `SPARKLES_REMOTE_MIN_LIFETIME` sets the requested minimum
for screens and builds, which default to 1800 seconds, and gates, which default to
7200 seconds. Partition shares require `TEST_PARTITION_TIMEOUT` seconds and check again after the
archive transfer. Boat job caps follow the qualified receipt deadline. The legacy
`--duration` screen option controls Namespace instance lifetime.

After collecting results, establish closure of every owned job and Boxington lease.
Then use the lifecycle controller's `request-stop --operation OPERATION` entrypoint.
That submits a request. The lifecycle owner must observe its verified terminal closure
receipt and stopped worker state before reporting release complete. Reconcile an
unknown acquire, launch or stop by its existing operation and unit identities. Never
repeat it to obtain a clearer response.

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
