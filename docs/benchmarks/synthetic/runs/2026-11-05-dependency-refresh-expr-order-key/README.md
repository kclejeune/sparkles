# Expression sort-key control after the dependency refresh

This manual record compares source `6d55f1e6caadea5309558f2e27261e057d50d702`
with `6724abe989b232e1b534d8b634b06ae3819d19b3` on forge. It is a bounded
follow-up of `expr-order-key`, rather than a peer comparison or a complete
performance suite. The later source contains the dependency refresh and numeric
correctness repairs, rendering preferences and documentation changes. This
comparison does not isolate the dependency refresh as the cause of the cost.

Both release executables were built on atlas in the same Nix environment with
empty extra Cargo arguments, learned reuse disabled and the same release settings.
The retained configurations and ELF comments agree, including GCC 15.3.0, Clang
23.1.0git, rustc 1.99.1 and LLD 23.1.1. Variant A is the earlier source, with
binary SHA256 `891fc6f6317372add5a237aa730fe01c1e2a59ea608ca703c7dca3d99f966016`.
Variant B is the later source, with binary SHA256
`a15cc9b3d2de293f3ceefec79ce975531fb34d2437d8201e5ced56f70dc9bc61`.
Neither executable was built on forge.

Forge has an Intel Core i5-13500, 20 logical CPUs and 15,620 MiB of memory. The
recorded kernel is 7.2.9. The stock system-loader probe reports glibc 2.44,
while both server ELF files embed the same Nix glibc 2.42-84 interpreter. The
[ELF linkage receipt](linkage.json) records the actual binary hashes and embedded
interpreter paths separately from that system probe. One named job held both
heavy-work and benchmark locks after a quiet-host preflight, with a 12 GiB memory
cap and a 2 GiB swap cap. The stock remote runner is retained byte for byte with
SHA256 `48a6e858f040a2ac60fb0561843e16b36f34ac795d1b5822ff888f0a6ffe781e`.
No execution-mode or calibration override, allocator preload or result cache was
used. The server ran one query thread on CPU 2 and the client on CPU 4, with idle
spinners. Each variant loaded its own store before timing.

The input is the cached output of `scripts/gen-data.py 100000`, containing
1,052,807 input statements and 1,052,801 distinct indexed triples after loading.
Its uncompressed SHA256 is
`c2d387f07a0717fdcd1d001081507b5ac09608961f562f0102d971c36e36edd8`.
The retained query has SHA256
`e4331de164d97e2f26250c4b85ebf20f5a17c747b25c199a294e56732417270c`.
Eight A,B,B,A rounds ran fresh servers, each with five warm-ups and 40 timed
requests. All 1,280 execution and wall samples are finite and complete.

The standard TSV references have identical ten-row answer and order fingerprints
across the two variants. All 1,280 native answer checks also agree. The native and
TSV paths use different fingerprint algorithms, so this record does not claim a
cross-format equality proof or an independent data oracle. The query, stores'
metadata, reference fingerprints, native checks and raw samples are immutable in
[first-batch-intel-evidence.tar.gz](first-batch-intel-evidence.tar.gz).
[control-receipt.json](control-receipt.json) binds the source, build, input and
runner hashes. [run.env](run.env) records the controls.

| Metric | Earlier source | Later source | Later / earlier | Paired-round 95% ratio interval |
|---|---:|---:|---:|---:|
| Native execution | 1.104797 ms | 1.120524 ms | 1.014235 | 1.011584 to 1.016480 |
| Complete HTTP wall | 2.804 ms | 2.803 ms | 0.999643 | 0.999287 to 1.000267 |

Each estimate is the median of 16 fresh-process medians per variant. The interval
resamples complete paired A,B,B,A rounds 10,000 times with seed 534498, retaining
both processes of each variant, and takes the nearest-rank 2.5 and 97.5 percentiles.
The execution process-median ranges do not overlap. The retained
[summary.json](summary.json) includes every process median.

The execution cost is a consistent 1.4235%, about 15.73 microseconds, and is
accepted while retaining the numeric and parser correctness repairs. Wall time is
unchanged within this interval. The cost is not classified as noise. Roadmap item
748 tracks attribution and recovery on the combined source. This bounded control
does not establish the effect of later query optimizations.

To validate the raw inventory and reproduce the manual summary, extract the archive
and run the standard-library-only analyzer from this directory.

```sh
mkdir evidence
tar -xzf first-batch-intel-evidence.tar.gz -C evidence
python3 analyze.py
```

The analyzer refuses missing, duplicate, non-finite or unexpected samples and
incomplete or inconsistent answer checks before writing `summary.json`. It does
not rerun the benchmark. The successful remote job exited zero and released both
locks. Earlier preflight attempts stopped before any server started because zstd
refused symbolic-link inputs. Resolving the owned input paths fixed preflight.
No failed timing run was discarded or relabeled.
