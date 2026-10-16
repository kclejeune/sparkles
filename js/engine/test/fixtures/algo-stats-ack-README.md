# Algorithm statistics ACK regression

These fixtures exercise private acknowledgments in a failpoints Node build. They
add no JavaScript API or production option. The ordinary addon does not contain
this probe. On non-Unix platforms an armed failpoints probe returns Unsupported
before installing an observer or acquiring an algorithm slot.

The publisher serializes a complete record, exclusively creates an ACK-specific
file in the fresh child directory, writes all bytes, checks its consuming close,
and renames it to entered.json or finished.json. A stale final name or duplicate
publisher is an error. A failed write, close or rename does not publish a final
record and removes only the temporary file created by that invocation. This is
atomic visibility for ephemeral test records. It makes no fsync or crash durability
claim. Each child directory has one writer for each ACK and must never be reused.

Build and test in an exclusive target directory with two build jobs. Copy the
corrected failpoints library to a file named sparkles.node before rebuilding the
ordinary library. Build the JavaScript packages with `pnpm --dir js build`.

```sh
cargo test --manifest-path crates/sparkles-node/Cargo.toml --features failpoints algo_stats -- --test-threads=1 --nocapture
cargo build --manifest-path crates/sparkles-node/Cargo.toml --features failpoints
python3 -I -S -B js/engine/test/fixtures/algo-stats-ack-regression.py --addon /absolute/corrected/sparkles.node --output /absolute/new/output
```

The Python driver requires Linux, a C compiler and Node. It copies and hashes the
addon, actual JavaScript child, package outputs and C interposer independently.
It compiles only the small C helper and applies LD_PRELOAD only to each isolated
Node child. Every child has a finite bound and is killed and reaped on timeout.
Raw stdout, stderr, return code, signal, elapsed duration and reap status are kept.
Use a new output directory for every invocation. Elapsed durations diagnose
bounded fixtures and are not benchmark results.

The real child runs compute and slot-wait cancellation and timeout controls
without delay and with delayed or partial writes to each ACK's temporary file.
The interposer also matches the old final names so the preserved faulty addon
fails the desired success assertion with an actual JSON.parse error. Each repaired
injected case must prove that the temporary write was intercepted, preventing a
vacuous pass after the final-name publication change. The child still checks exact
deadline forwarding, native Cancelled or Timeout, no early holder release, slot
reuse and the public abort or QueryTimeoutError. Public timeouts can race native
cancellation with native deadline expiry.

Additional expected failures exercise write, close and rename faults for both
ACKs, missing final records and malformed final JSON. A malformed published record
must fail parsing. The JavaScript reader retries only ENOENT within its original
bound. The driver's unpublished finished payload records that cleanup already ran
without treating a failed publication as successful entry or cancellation.

The Rust tests pause the actual publisher after temporary-file creation and after
a prefix write for each ACK. A concurrent reader must observe the final name as
absent until explicitly releasing and joining the writer, then read exactly the
complete JSON. Other controls cover stale final files, dangling final symlinks,
duplicate publication, failed temporary creation, original write errors and owned
temporary cleanup. Separate bounded native children force entered publication to
fail during computation and slot admission, catch the original observer panic,
and prove holder cleanup with actual slot reuse and another successful native
query. The source-bound counterfactual used during repair disables the publisher
and must restore the delayed entered JSON failure. Keep that artifact distinct
from the final corrected addon.
