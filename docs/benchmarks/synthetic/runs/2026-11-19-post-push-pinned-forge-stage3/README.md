# Pinned checks and drift on forge at batch 37

This is a manual record of eleven interleaved A/B pairs that forge ran alone on 2026-11-19
under its benchmark and build locks. The
[synthetic results](../../RESULTS.md#pinned-checks-and-drift-at-batch-37) discuss them.

- `validation.json` names the host, builds, data, settings, lock, verdict rule and answer
  check, and `binaries.sha256` and `binaries-zst.sha256` hold the binary hashes.
- `stage3.sh` ran the pairs through `ab3.sh`, which ran `nsc-bench-remote.sh` in each pair
  directory with that directory's `run.env`.
- Each pair directory holds `revs.txt`, the query files, the samples, the answer checksums,
  the load and progress logs and the uptime before and after.
- `analyze.py` wrote `pinned-summary.json` and `pinned-summary.txt`, and `verdict.py` wrote
  the combined verdicts in `verdict.txt`.
- `attribution/` holds instruction counts and profiles from the laptop, which locate
  changes and do not size them.
