# Owned Boat workers

This package qualifies one explicitly owned warm Boat worker and keeps an independent
local deadline guard through normal stop. It starts no source job and never creates,
forks, extends, force-stops or silently replaces a worker. Build, screen, partition and
gate adapters keep their own source, resource, test and lease requirements.

The maintained defaults refuse. `profile.FALSE.json`, `release.FALSE.json` and
`job-closure.FALSE.json` contain no actual worker, account, environment or credential.
Private operator-reviewed copies provide those bindings. The release also requires
current local-first need, the shared start reservation and live account limits, exact
CLI/profile/source reviews, environment exclusion, snapshot continuity and previous
native/lease closure. An owned ledger alone does not establish account-wide usage.

## Qualification and source binding

The operator passes the existing owned worker, private profile and private release
explicitly. `config_schema.py` defines profile version 1 and refuses unknown fields.
The profile pins worker UID/home, the large 8-vCPU/16-GiB machine, environment/hook,
setup members, saved toolchain manifest, startup source boundaries, tool versions,
nonsecret read-only MBX endpoint/namespace and normal-stop semantics. It contains no
token content. Profile parameterization grants no authority over another worker.

The release's `controller_packet_manifest_sha256` binds the final
`runtime-sources.json`. That seal covers every maintained runtime, control, example and
schema member. Update it after source changes and obtain independent review of the new
seal before a real release. These commands only rebuild or check source metadata.

```sh
python3 -I -B scripts/boat/update_sources.py
python3 -I -B scripts/boat/update_sources.py --check
python3 -I -B scripts/boat/controls.py
```

The lifecycle requests one 10,800-second resume and arms a local guard at the earlier
10,500-second wall/BOOTTIME deadline. It checks the live guard's boot, invocation,
loaded command, PID, cgroup and limits. Null, malformed or lost acknowledgements retain
the start reservation and never authorize another resume. Provider readiness alone does
not qualify the worker. The pinned provisioning hook may run its reviewed normal setup,
including first bootstrap. Actual bootstrap-lock, descendant, tool, manifest, source,
capacity, token metadata and metadata-service closure checks must pass afterward. A
different manifest is refused rather than adopted automatically.

Development credentials remain on the development worker. The token is qualified with
lstat, and its content is never read by this package. The reviewed setup hook's own
metadata-open behavior must remain within the release's provisioning scope. GitHub
Actions retains separate OIDC authentication. This package exports no reusable snapshot
or credential-bearing template.

## One warm-worker sequence

The variables below name operator-owned private files and an unused controller unit.
Their reviewed values are required inputs. No maintained default selects a live worker.
The private release must opt into its independently reviewed job handoff. Without a job
handoff, successful qualification requests normal stop.

```sh
mise run boat:qualify --unit "$LOCAL_CONTROLLER_UNIT" --worker "$OWNED_WORKER" --profile "$PRIVATE_PROFILE" --release "$PRIVATE_RELEASE"
journalctl --user -u "$LOCAL_CONTROLLER_UNIT" --no-pager
python3 -I -B scripts/boat/qualified_worker_admission.py --owned-worker "$OWNED_WORKER" --profile "$PRIVATE_PROFILE" --operation "$OWNED_OPERATION"
mise run boat:job-env --owned-worker "$OWNED_WORKER" --profile "$PRIVATE_PROFILE" --operation "$OWNED_OPERATION" --out "$OWNED_OPERATION/job-admission.json"
export SPARKLES_BOAT_PROFILE="$PRIVATE_PROFILE"
export SPARKLES_BOAT_ADMISSION="$OWNED_OPERATION/job-admission.json"
worker="boat:$OWNED_WORKER"
python3 scripts/remote-executor.py admit "$worker" --minimum 1800
scripts/build-remote.sh --host "$worker" --out .artifacts/builds/candidate HEAD
mise run bench:remote --executor "$worker" path/to/baseline path/to/candidate
mise run bench:remote-writes --executor "$worker" base=path/to/baseline head=path/to/candidate
mise run test:partitioned --hosts "$worker" --no-local
scripts/gate/setup-host.sh "$worker" wally
scripts/gate/gate.sh --host "$worker" --helper none HEAD
mise run boat:finish --operation "$OWNED_OPERATION" --job-closure "$PRIVATE_JOB_CLOSURE"
```

Qualification runs in a named local 512-MiB, zero-swap controller unit with 2,700-second
runtime and 30-second stop limits. The controller returns after qualification/handoff;
its separate local guard remains active. Journal progress is observation rather than
qualification evidence. Admission export succeeds only when the exact operation has an
actual qualified handoff and matching local guard.

The original `qualified-worker-admission.json` remains unchanged and has
`runtime_env_file:null`. `job_environment.py` validates that receipt with the shared
`validate_admission` consumer and its exact private profile, operation, source seal,
job review and live guard. It creates one bounded mode-0600 nonsecret environment file
containing literal PATH and token FILE path only. Its separate job-admission record
references the original qualification bytes and actual environment-file proof. It
creates no extra authority or lifetime. Adapters pass the deterministic values
literally and never source arbitrary shell values. An uncertain preparation is
reconciled with the same command and `--check`, which creates no worker files.

Several jobs reuse the same worker without lifecycle starts. Each checks the original
deadline on both the wall clock and `CLOCK_BOOTTIME`, keeps the 30-second closure
reserve and refuses inadequate remaining time. The qualification receipt and local
guard stay bound to the same operation throughout. The transport's actual Boat quoting,
workload correctness, cache reuse and interruption behavior still require real
acceptance under roadmap item 762. Namespace remains the required screen fallback.

## Closure and normal stop

Before finish, the operator verifies all owned native processes, cgroups and Boxington
leases are closed and records that evidence in a fresh private job-closure receipt.
`job-closure.FALSE.json` documents the fields and cannot claim closure. The real receipt
must bind the worker, operation, identical local guard tuple, exact job-release hash and
actual closed state. `finish` validates and publishes that receipt without inventing
closure observations. The guard alone submits the one normal stop request.

`finish` waits up to 180 seconds for the same operation's verified completed/nonforced
stop, current stopped/snapshot proof and clean local guard exit. A pending, submitted,
unknown or unresolved stop fails and retains reconciliation ownership. It never forces,
resumes, retries stop or selects another worker. `request-stop --operation OPERATION`
remains available to submit the trigger without claiming completion. Recovery uses the
operation's protected original profile snapshot and source seal.
