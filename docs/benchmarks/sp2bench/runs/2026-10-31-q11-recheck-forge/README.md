# SP²Bench `q11` at 10M on forge

This run pairs Sparkles `d65eaf40` with QLever 0.5.48 on SP²Bench `q11` over the 10M
corpus. It repeats the comparison that the earlier table took from Sparkles `6699f27d`,
which decoded and ranked every `rdfs:seeAlso` value before it could return ten rows.

## Host and build

The host is `forge`, an Intel Core i5-13500 with 20 threads and 16 GiB of RAM, on kernel
7.2.9 with the `powersave` governor. The run held `~/lanes/.heavy.lock`, and no other
build or benchmark ran during a load or a timed request. `preflight.txt` records the load
and memory before the run, and `trim.txt` the `fstrim` that preceded it.

The Sparkles binary was built on forge from `d65eaf40` in `nix develop` with
`mbx build --release --locked -p sparkles-server --bin sparkles` and a placeholder
`ui/build/index.html`. Its SHA-256 is in `inputs.sha256` and in each record's manifest,
together with the SHA-256 of the 10,000,091-triple `data.nt`. QLever is the pinned Nix
store path in the manifests.

## Method

`scripts/bench-sp2b.sh 10m` ran twice with `QUERIES=q11`, `WARMUP=10`, `RUNS=30`,
`CLIENT_CPU=11` and `SERVER_MEM_MAX=12G`. The first run loaded both stores and timed
Sparkles first. The second run reused the stores with `SKIP_LOAD=1` and timed QLever
first. Each engine served alone during its answer check and its timings.

Both engines returned the same ten rows in both runs, with the value and order
fingerprint `fba9c0ebb1734be4/f0c0253753657af0` of the retained Jena, Oxigraph and Sparkles
answers in `../../../2026-10-23-sp2b-10m-forge.json`.

## Result

Over the 60 pooled requests, Sparkles takes a mean of 3.46 ms with a sample standard
deviation of 0.15 ms, and QLever takes 43.56 ± 4.60 ms. The medians are 3.43 and
41.54 ms. Each run's `summary.md` gives the per-run means.

## Files

`run1/` and `run2/` hold each run's `record.json`, `manifest.json`, hyperfine output
`q11.json`, answer fingerprints `answers.json`, load and memory summaries and
`summary.md`. The records are registered in `../../RUNS.json`.
