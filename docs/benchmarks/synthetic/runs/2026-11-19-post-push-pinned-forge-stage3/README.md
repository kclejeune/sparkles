# Pinned checks and drift on forge at batch 37

This is a manual record of eleven interleaved A/B pairs that forge ran alone on 2026-11-19
under its benchmark and build locks. The
[synthetic results](../../RESULTS.md#pinned-checks-and-drift-at-batch-37) discuss them.

- The [validation record](validation.json) names the host, builds, data, settings, lock,
  verdict rule and answer check. The [binary hashes](binaries.sha256) and
  [compressed binary hashes](binaries-zst.sha256) identify both forms of the builds.
- [stage3.sh](stage3.sh) ran the pairs through [ab3.sh](ab3.sh), which invoked
  [nsc-bench-remote.sh](nsc-bench-remote.sh) with the pair's `run.env`.
- [analyze.py](analyze.py) wrote the [summary data](pinned-summary.json) and
  [summary table](pinned-summary.txt). [verdict.py](verdict.py) wrote the [verdicts](verdict.txt).
- The [attribution record](attribution/README.md) holds the laptop instruction counts and
  profiles used to locate changes.

Each pair directory holds its revisions, query files, samples, answer checksums, load
and progress logs, and uptime before and after.

- [big-b32-10m-12t-eager](big-b32-10m-12t-eager/)
- [big-b32-10m-12t-stream](big-b32-10m-12t-stream/)
- [big-b32-12t-eager](big-b32-12t-eager/)
- [big-b32-12t-stream](big-b32-12t-stream/)
- [big-b32-1t-stream](big-b32-1t-stream/)
- [drift-cc94-1t-stream](drift-cc94-1t-stream/)
- [sqa-b35-auto](sqa-b35-auto/)
- [sqa-b35-stream](sqa-b35-stream/)
- [tg-b33-auto](tg-b33-auto/)
- [tg-b33-eager](tg-b33-eager/)
- [tg-b33-stream](tg-b33-stream/)
