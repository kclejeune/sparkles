# DBpedia 50M `births-by-decade` on forge

This run pairs Sparkles `1acdc38a` with QLever 0.5.48 on the DBpedia 50M subject slice
for the one query `births-by-decade`. It follows the decimal arithmetic and inline date
changes. It measures warm and cold requests under two timers and checks that both
engines return the same 80 groups.

## Host and build

The host is `forge`, an Intel Core i5-13500 with 20 threads and 16 GiB of RAM, on kernel
7.2.9 with the `powersave` governor and the `balance_performance` energy preference. Each
driver held both `~/lanes/.bench.lock` and `~/lanes/.heavy.lock` while it ran, and no other build or
benchmark ran during a load or a timed request. The disk was 96% full and the last
trim was 3.5 days old, which the preflight files in `logs/` record. Cold requests may
therefore include write stalls of the drive, and both engines see the same state.

The Sparkles binary is `bin/1acdc38a`, with SHA-256
`512c5e61fe8eee94fb17dc4f03f2236f619bcb58c0bea9a300cd51135b76d10c`. It was built on forge
from a `git archive` of `1acdc38a` with `mbx build --release --locked -p sparkles-server`
and a placeholder `ui/build/index.html`. The rustc is 1.99.1, the C compiler is clang 21.1.8
with GCC 16.2.0 and the linker is LLD 23.1.1. QLever is the pinned Nix store path in
`record.json`. The harness scripts are the ones in that archive, so `scripts_commit` in
the manifest names the archive and not a checkout.

## Method

`drive432.sh` loaded both stores from the 38 files of the 50M slice with
`scripts/bench-billion.sh load`. Sparkles took 27.5 s with a peak of 3,181,212 KiB and QLever
50.5 s with a peak of 7,273,776 KiB. `drive432b.sh` then ran `scripts/bench-billion.sh queries`
for `births-by-decade` five times, each with two warm-ups, ten timed requests and one cold
request after a restart with the engine's files evicted from the page cache. The timer is
hyperfine 2.0.0 with curl, the client is pinned to CPU 11, and each server runs in a
systemd scope capped at 12 GiB with no swap. The answer check of `scripts/bench-answers.py`
ran with both engines up. The two engines agree on the value fingerprint
`ca64446e6253f769` and on 80 rows.

`births_http.py` and `drive432c.sh` ran the complete-HTTP JSON timer of the cursor
assessment. The directory `python-timer/20261031T121953Z` holds the 20-round run. The
directory `python-timer/20261031T121829Z` holds a first four-round run, which uses the same
script and agrees with it, and is kept as evidence. Ports 41910 and 41911 were used.

## Files and their handling

`record.json` is the common record of the hyperfine passes, built by `make-record.py` from
`passes/`. The hyperfine of forge writes per-run `measurements`, where
`scripts/bench-record.py` reads `times`, so `make-record.py` converts them and joins the five
passes into one result per engine. Every pass file also holds the results of earlier
passes, because the harness merges by command and each command names a random port.

The first query pass ran right after the loads, while the disk was still writing back
the QLever index, with a load average of 9. Its results are the first two entries of each
pass file and are not in `record.json`. Its driver stopped at the report step, whose
summary script cannot read the newer hyperfine output, and `discarded-first-attempt/`
keeps that log. Two earlier attempts failed before timing anything, the first because
`find` did not follow a symbolic link to the slice and the second because QLever's indexer
ignored symbolic links. `drive432.sh` has the corrected hard-link copy. A first Python timer
run waited on `/$/ping`, which answers before a dataset is open under bind-before-open
startup, and failed with a 503. Its log is also in `discarded-first-attempt/`, and
`births_http.py` waits on `/$/ready` instead.

All raw files stay as the run produced them. `record.json` has the SHA-256
`90af9861b347fb271ae1f3ed76cf13da8e3cd2c768db8f0b391be483d765f5ef`, which is its run
identifier in [RUNS.json](../../RUNS.json).
