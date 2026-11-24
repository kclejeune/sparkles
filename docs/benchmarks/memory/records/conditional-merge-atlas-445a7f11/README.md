# Pair conditions in the streamed merge, atlas

These are the manual records behind the conditional merge comparison in the
[memory results](../../RESULTS.md#automatic-exists) and the
[X05 outcome](../../../../specs/X05-streaming-query-execution.md#pair-conditions-in-the-streamed-merge-2026-11-23).
They are manual records, not bench-record files, so the [run catalog](../../RUNS.json)
does not list them. Every copied TSV, query, script, log and checksum file keeps the exact
bytes written on `atlas`.

Both binaries are release builds made on `atlas` with `rustc 1.99.1 (58cf8f9c3 2026-10-13)`,
`mbx 1.30.0`, `mbx build --release -p sparkles-server` and `strip --strip-debug`. The
preceding base is commit `fc7b0bb9015d96f19e4c5cc3f1d9bfe1f9a2896b`, and the candidate is
the clean merge of `445a7f11b8b60f779db41d9d0b161761fe2ab438` into that base, producing
tree `40c5aaafced4f80105a318a93849f1f785d7a297`. `binary-and-query.sha256` records the
SHA-256 digests of the two stripped binaries, the switch-off wrapper, the five queries in
`qp2/` and the scripts `ab4.sh`, `run-confirm.sh` and `verify_quiet.py`. The driver
`driver.sh`, the harness scripts `run-confirm.sh` and `ab4.sh` and the verifier
`verify_quiet.py` are archived verbatim in `scripts.tar.zst` to preserve their exact
recorded bytes:

* `bin/base`: `9a1313d670262538825303648360079fcb5b997f5583db1b0eb1ee93bbe94dac`
* `bin/cand`: `90a1557e0908b252a6d1a75535ffbd715448b185ef23bed0d151a2df41d88ec7`
* `bin/cand-off` (wrapper setting `SPARKLES_DISABLE_OPTIMIZATIONS=merge_pair_conditions`): `bc9b218ad8e704eef20adb5bedb1b11c8533e3e4cc85673c49837d436e3d4f5a`

The dataset is the 10.5M-quad synthetic store at
`/home/kclejeune/lanes/fanout-570-look/store-base` (`device=65026`, `inode=5243723`,
`mtime=2026-11-17 20:18:24 -0500`), recorded in `host.txt`. Every server ran on `atlas`
(`Linux atlas 7.3.3 #1-NixOS SMP PREEMPT_DYNAMIC x86_64`) with one Rayon thread,
`--result-cache-mb 0`, forced streaming (`MODES=streaming`), the server pinned to CPU 2
and the client pinned to CPU 4. `driver.sh` built both binaries and ran `run-confirm.sh`
under the host's heavy and benchmark locks, waiting for a one-minute load average of at
most `1.0` before each arm as logged in `admission.log`. Each arm ran six A, B, B, A
rounds with two warm-up requests and twelve timed requests per query per process, giving
144 timed samples per binary, query and arm (4,320 timed samples and 360 answer checks in
total).

| Directory | Arm A | Arm B | Purpose |
|---|---|---|---|
| `base-cand` | `bin/base` (`fc7b0bb9`) | `bin/cand` (`fc7b0bb9` + `445a7f11`, switch on) | Full candidate effect against the preceding base |
| `off-cand` | `bin/cand-off` (switch off) | `bin/cand` (switch on) | Isolation of `merge_pair_conditions` within one binary |
| `base-base` | `bin/base` | `bin/base` | Same-binary validity check |

`ab4.sh` recorded server execution times in each arm's `samples.tsv`, sorted answer
digests in `answers.tsv` and per-query medians and process spreads in `summary.tsv`.
`verify_quiet.py` checked completeness and answer agreement across all three arms and
computed 95% confidence intervals with 10,000 percentile bootstrap draws (seed `20261124`)
both at the sample level (144 against 144 requests) and paired by round across the six
round median ratios. All five queries agreed across every binary and arm; `checked-answers.tsv`
lists the digests, with four distinct values because `class-exists` and `class-fail`
return the same answer (`d03230387b6069c9`).
