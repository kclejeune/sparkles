# Ordered windows on wally

This record holds the measurement of the ordered windows of
[F13 §4.4](../../../../specs/F13-incremental-views-and-provenance.md#ordered-windows). It is
a manual record. The files are the raw output of the run and are not edited.

`window-against-recompute.log` is the whole output of one job of `scripts/remote-ci.sh
--cmd` on wally, a 12th Gen Intel Core i7-12800H with 20 CPUs as `lscpu` reports, which ran under the host's heavy
lock with no other build or test beside it. The job checked out commit `9934f16b`, whose
tree is that of `1396fa6c`, and ran
`mbx test --release -p sparkles-core --test incr_bench -- --ignored --nocapture --test-threads 1 window_against_recompute`
with Rust 1.99.1 and mbx 1.29.1, which the log's first lines give. The benchmark used its
default of 200,000 people and 400 commits. The test passed, which includes its check that
every view's maintained rows equal its query evaluated again at the end. `SHA256SUMS`
identifies the log.

This is a development-host measurement for the design, not a published result.
