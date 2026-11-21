# Three builds of the 10.5M, 12-thread matrix cell on forge

This is a manual record of the run that
[Matrix flags at batches 49 and 50](../../RESULTS.md#matrix-flags-at-batches-49-and-50)
reports. The files are the raw outputs and are not edited, apart from `paired.py`, which
reads them from this directory.

## Host, builds and data

The run took place on forge, an Intel Core i5-13500 with 20 CPUs, 15 GiB of RAM and Linux
7.2.9, on 2026-11-21 between 15:00 and 18:33 UTC. It held `~/lanes/.bench.lock` and
`~/lanes/.heavy.lock` and ran as a systemd user unit, with no other heavy work on the host.
The root filesystem was 96% to 97% full throughout, as `run.log` shows.

The three binaries are the matrix release builds of 6dbf2a08, b47b582b and 0779d666, the
same files that the forge matrices of those commits ran, identified by their SHA-256 in
`binaries.sha256`. All three were built with the configuration in `build-config.json`.
The harness is the unmodified `scripts/bench-executors.sh` of 8baa5686, which the
matrices use, and the data is the matrix's own `people-1000000`, 10.5M triples.

## Method

`run.sh` runs the matrix harness for the 10.5M cell with 12 threads and one round per
invocation, six times in the order 6dbf2a08, b47b582b, 0779d666, 0779d666, b47b582b,
6dbf2a08. Each invocation loads one shared store and starts four servers in the order
eager, streaming, streaming, eager, with the matrix's settings in `run.env`, so each build
has four eager and four streaming server epochs. Each epoch times 31 queries with 3 warm-up
and 10 timed requests. `samples/` holds each invocation's samples.

`summary.txt` gives each build's epoch medians and the ratio of the medians of the eight
epochs. `paired.py` pairs each invocation with its neighbour in the palindrome, invocation
2 with 1, 5 with 6, 3 with 1 and 4 with 6, and prints the ratio of each paired epoch in
`paired.txt`, with `ALL>1` where all four ratios of a mode are above 1 and `ALL<1` where
all four are below 1. In `paired.txt`, `A` is eager and `B` is streaming.

`run.log` holds a snapshot of the host before the run and after each invocation, with the
disk use, memory, pressure stall figures, CPU frequency settings, throttle counts and NVMe
counters. `host-samples.tsv` samples the host every 15 s. The throttle counters did not
change during the run.

`binaries.sha256`, `answers.sha256` and `paired.txt` were derived from the raw outputs
after the run.

## Correctness

`answers.sha256` holds the checksums of each invocation's answer and warm-answer digests.
All six invocations have the same checksums.
