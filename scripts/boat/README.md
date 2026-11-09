# Owned Boat workers

This package qualifies one explicitly owned warm Boat worker and keeps an independent
local deadline guard through normal stop. It starts no source job and never creates,
forks, force-stops or silently replaces a worker. It extends a running worker only
through the [receipted extension](#lifetime-extension) below. Build, screen, partition
and gate adapters keep their own source, resource, test and lease requirements.

The maintained defaults refuse. `profile.FALSE.json`, `release.FALSE.json` and
`job-closure.FALSE.json` contain no actual worker, account, environment or credential.
Private operator-reviewed copies provide those bindings. The release also requires
current local-first need, the shared start reservation and live account limits, exact
CLI/profile/source reviews, environment exclusion, snapshot continuity and previous
native/lease closure. An owned ledger alone does not establish account-wide usage. A
qualification run may set `overflow_currently_needed` to false only with the
`qualification` purpose and the SHA-256 of the maintainer's recorded confirmation.

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
10,500-second wall/BOOTTIME deadline. The guard unit's own runtime limit is 43,500
seconds, which covers the 12-hour extension cap, the wait for a running extension act
and the stop margin. The provider TTL stays the worker's backstop. It checks the live guard's boot, invocation,
loaded command, PID, cgroup and limits. Null, malformed or lost acknowledgements retain
the start reservation and never authorize another resume. Provider readiness alone does
not qualify the worker. The pinned provisioning hook may run its reviewed normal setup,
including first bootstrap. Actual bootstrap-lock, descendant, tool, manifest, source,
capacity, token metadata and metadata-service closure checks must pass afterward. A
different manifest is refused rather than adopted automatically.

## Startup closure

The startup observer treats a process of the worker's uid as startup work, apart from
systemd, its PAM helper and dbus. It also treats any installer, shell or build tool as
startup work, whatever its uid, and tracks every descendant of startup work by PID and
start time. The provider's desktop session, which the first SSH connection starts, runs
more than 30 such processes for the worker's whole life, so that rule alone never closes.
`SESSION_CHAINS` in `startup-observer.py` allows those processes by their exact name and
the full chain of their ancestors up to pid 1, as observed on an admitted worker. Each
ancestor in a chain is pid 1 as root, the worker's user manager, or a name with the
worker's uid or root's uid, and every ancestor of the worker's uid must itself be an
allowed session process. The user manager is the main process that the system manager
reports for `user@UID.service`, matched by pid and start time, and a name never stands in
for pid 1 or the manager. The agent service's shell is allowed only while it has no
children, and its sudo only while its children are the agent's root process logger. A
process with any other chain, a name outside the table and any descendant of tracked
startup work still count as startup work. Adding an entry needs the observed process
tree and an independent review.

The setup hook can still be restoring the state directory when the observer starts, and
a file of a restored layer can get a new inode on its first open for writing. The hook
writes no completion marker. A bootstrap lock or manifest replaced during an attempt,
or a state directory whose entries change during it, therefore counts as startup still
running. The observer then waits until the directory's entries keep the same identity,
size and times for five seconds and observes again. It refuses only when its observation
deadline passes, with `startup_busy:bootstrap_lock_replaced` or
`startup_busy:state_restore_in_progress` when that was the last thing it saw.

Every refusal records a bounded reason code. A refusal of this package keeps its literal
reason, and any other exception records only its type and the function and line where it
was raised, never its text. The controller keeps a remote code only in one of these
forms, and a refusal code only when it names one of the packet's own refusal reasons.
Any other value is recorded as `malformed`. A busy startup reports up to 32 of the processes it counted,
each with its pid, name, uid and parent's name and uid, and never a command line or
environment. The controller keeps the first refused startup in `startup-refused.json` and
the code of a failed qualification in `qualification-failed.json`. An unresolved stop
records its code in `unresolved.json`.

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
mise run boat:closure-watch --unit "$LOCAL_WATCH_UNIT" --operation "$OWNED_OPERATION"
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
containing only literal PATH, pinned Rust toolchain and token FILE path values. Its separate job-admission record
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

## Lifetime extension

A worker can serve another lane without a stop and resume. The lifecycle owner records
a fresh environment fact, as for the post-resume check, and extends the same operation.

```sh
python3 -I -B scripts/boat/controller.py extend --operation "$OWNED_OPERATION" --seconds 7200 --environment-fact "$PRIVATE_ENV_FACT" --reason "next lane"
```

The act refuses a stopping or closed operation and a lifetime already inside the
30-second closure reserve. It adopts the worker only when the local boot and the live
guard's invocation and PID match the guard receipt, the worker's current boot id matches
the one the metadata probe recorded, the provider reports the qualified environment
version and the fact matches the environment, version and hook. The new end is the
act's own observation plus at most 10,500 seconds on the wall clock and
`CLOCK_BOOTTIME`, never shorter than the current end and never more than 12 hours after
the controller intent. The act sets the provider TTL 300 seconds past the new end with
`boat extend --ttl`, confirms `archiveAfter` through info and then publishes
`lifetime-extension-NNNN.json`. A failure leaves only an attempt record, which grants
nothing, and the provider lifetime may then be longer than the guard's deadline.

The act checks the previous end again after its last blocking call and publishes only
while the 30-second closure reserve remains. The receipt records that publication time
with the observation, provider confirmation and adoption facts. A receipt grants time
only after the running guard accepts it. The guard reads new receipts on every pass and
records `lifetime-extension-NNNN-accepted.json`, bound to the receipt's digest, only
while its own clocks leave the closure reserve before the previous end. If that write
finishes after the previous end, the guard records a veto, ignores the acceptance for
its life and stops at the earlier end. Each act must follow the previous acceptance. The command waits up to 15
seconds for that acceptance and exits nonzero without it. A later act refuses while an
earlier receipt is unaccepted. One act runs at a time
under the operation's extension lock. The guard takes the same lock before it stops,
whether at its deadline or on an explicit stop trigger. At the deadline it reads the
receipts again under the lock and, if no valid extension appeared, records the stop
trigger before it stops. Admission and job preparation refuse an operation with a stop
trigger, a stop request or a closure record, extended or not.

Admission, job preparation and the guard accept only consecutive version-1 receipts
whose every field matches the worker, operation, guard, worker boot and qualified
environment, each observed after the previous act was published and published before
the previous end. Without a valid chain the guard uses the conservative original end
that admission uses, and it keeps that end for its life, so missing or damaged receipts
can only end the lifetime sooner.
Receipts carry the same owner-only file trust as the other operation receipts. A job
admitted before an extension keeps the deadline it started with.

## Closure and normal stop

Before finish, the operator verifies all owned native processes, cgroups and Boxington
leases are closed and records that evidence in a fresh private job-closure receipt.
`job-closure.FALSE.json` documents the fields and cannot claim closure. The real receipt
must bind the worker, operation, identical local guard tuple, exact job-release hash and
actual closed state. `finish` validates and publishes that receipt without inventing
closure observations. The guard alone submits the one normal stop request.

## Scheduled closure

A worker kept warm past its jobs would reach the guard's deadline without a job-closure
receipt, and the guard would then leave it to the provider TTL. The closure watch runs
beside the guard under its own named local unit and closes the operation in time.

```sh
mise run boat:closure-watch --unit "$LOCAL_WATCH_UNIT" --operation "$OWNED_OPERATION" --lead 900 --local-unit "$LOCAL_JOB_UNIT"
```

The watch reads the guard's present end, with the extensions the guard accepted, and
waits until `--lead` seconds before it. It then checks over SSH that no
`sparkles-screen-*`, `nsc-build-*` or `gate-*` unit is active on the worker and that the
worker's boot is the qualified one. It also requires that no process remains that the
startup observer would count, which is any process of the worker's uid outside systemd,
dbus and the allowed provider session, any installer, shell or build tool and any
descendant of those. Detached work, such as a partition share under `setsid`, is caught
this way. An mbx lease lives only as long as its mbx process, so the process check
covers the Boxington leases. Each named local job unit must also be inactive, and at
most eight can be named, and a name without a unit suffix means a service. One bound
covers the SSH check and the single local `systemctl` query, including the teardown of
each call, and a check that ends after it counts as failed. Each check is kept as
`closure-watch-check-NNNN.json`. An open job is checked again every 30 seconds.

Every Boat command takes the operation's `jobs.lock` shared when its admission is
loaded and holds it until the command exits. Commands and the watch open that lock, and
the extension lock, only as a private regular file of the operation's owner with a
single link. Once a check finds every job closed, the
watch takes the extension lock and then `jobs.lock` exclusively. If a command holds the
job lock, the watch tries again at the next check. While the watch holds it, a new
command waits up to 120 seconds for it and then sees the stop trigger if the watch
closed the operation. The watch finishes its decision within 90 seconds, inside the
guard's 100-second wait for the extension lock. It waits up to 15 seconds for the guard
to accept an extension published before it, checks the jobs once more and publishes
`root-job-closed.json` from that check with the stop trigger. It publishes only if the
end, the accepted extensions and the terminal receipts are unchanged since it took the
locks and the decision bound has not passed. It then observes the guard's verified stop
as `finish` does. If no check finds the jobs closed by 60 seconds before the end, the
watch records `closure-watch-unresolved.json` with the last reason code and leaves the
guard's own deadline stop in place. One watch runs per operation.

`finish` waits up to 180 seconds for the same operation's verified completed/nonforced
stop, current stopped/snapshot proof and clean local guard exit. A pending, submitted,
unknown or unresolved stop fails and retains reconciliation ownership. It never forces,
resumes, retries stop or selects another worker. `request-stop --operation OPERATION`
remains available to submit the trigger without claiming completion. Recovery uses the
operation's protected original profile snapshot and source seal.
