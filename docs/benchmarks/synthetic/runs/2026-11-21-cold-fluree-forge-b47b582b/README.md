# Cold small scans against Fluree at 1.05M, forge, b47b582b

This is a manual record of the run that
[Cold against Fluree at 1.05M](../../RESULTS.md#cold-against-fluree-at-105m) reports. The
files are the raw outputs and are not edited.

## Host, builds and data

The run took place on forge, an Intel Core i5-13500 with 20 CPUs, 15 GiB of RAM and Linux
7.2.9, on 2026-11-21 between 11:10 and 11:50 UTC. Every step held `~/lanes/.bench.lock`
and `~/lanes/.heavy.lock` and ran as a systemd user unit capped at 12 GiB, with no other
heavy work on the host. The NVMe drive is an SK hynix HFS512GEJ9X102N with
`nvme_core.default_ps_max_latency_us=100000`.

The Sparkles binary is the release build of commit b47b582b, built on another host with
the configuration in `build-config.json`. `sparkles --version` prints no commit, so the
binary is identified by its SHA-256 in `binaries.sha256`. Fluree is release 4.2.2 from the
repository's pinned package, `/nix/store/jvhxwjlhzijkd1kvrh8l7wfkbclp8vnx-fluree-4.2.2`,
with its SHA-256 in the same file. The harness is `scripts/bench.sh` and
`scripts/bench-regressions.py` of b47b582b. The data is `scripts/gen-data.py 100000`,
1,052,807 triples, loaded fresh into both engines by the harness.

## Correctness

`answers/answers.json` holds both engines' answer fingerprints for the eight queries. The
value fingerprints agree on every query, and the exact fingerprints differ only on
`star-lookup`, whose rows come back in another order.

## Files

- `m1.sh.txt` loads both engines, checks the answers, runs the three cold rounds of
  `bench.sh` into `cold-1` to `cold-3`, and runs `bench-regressions.py --cold --io` into
  `io/io.json`.
- `m2.sh.txt` runs the three rounds again into `dcold-1` to `dcold-3` with a 1 s pause after
  the readiness check, which it adds to a copy of `bench-lib.sh` as the single line
  `sleep "${COLD_DELAY:-0}"`. It also records Sparkles' server timings for cold requests
  in `native/native.json`.
- `m3.sh.txt` adds the same pause to a copy of `bench-regressions.py` and records server
  timings at pauses of 0 and 1 s in `m3`. `m4.sh.txt` compares pauses of 0, 0.2, 0.5, 1 and
  3 s, and `serve --no-code-prefault` and `MIMALLOC_PURGE_DELAY=-1` at 1 s, in `m4`.
  `m5.sh.txt` records the `profile=io` timings of `count-all` at pauses of 0 and 1 s in `m5`.
- `fluree-october-store` holds the cold samples of Fluree on the store loaded for the
  earlier run, copied from `~/.cache/sparkles-bench/1m/fluree`, with `m6.log`.
- `device-read-probe.txt` holds single direct reads on the drive after idle gaps, and
  `idle-warm-server.txt` (from `idle.sh.txt`) holds warm requests to one server after idle
  gaps.
