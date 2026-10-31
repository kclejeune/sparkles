# Python file objects through the loader, atlas run

This run compares `Dataset.load` from a Python file object before and after the loader
read file objects as a stream. The base build is commit `99fb6e95`, which inserted the
quads of a file object in automatic mode one at a time into a transaction, passed Jena's
syntaxes through an N-Quads pipe first, and copied the file object to a temporary file in
buffered mode. The new build is commit `5607bf86`, which hands the file object to the
bulk loader as a stream that it reads once. Both are release wheels of
`mise run py:build`, built from a source archive of the commit and installed into a
virtual environment of their own.

The host is atlas, an Intel Core i5-13500 with 20 logical CPUs and 15 GiB of RAM, with
CPython 3.14.7. Its load average was below 1 when the run began, and no build or other
job ran during it. Each load runs in a new process into a new database directory, and the
rounds interleave the two builds.

The input is `gen.py 5000000` (5,000,000 triples, 402 MB as N-Triples). The new build
wrote the Turtle file (242 MB) and the RDF Thrift file (451 MB) from it. `one.py` opens
the file in binary mode and passes the file object to `load`, or passes the path for the
`path` rows. `ab.sh` runs three rounds of every case.

```
ROUNDS=3 ./ab.sh
```

`results.jsonl` holds one line for each load, with its wall time in seconds and the peak
RSS of the process in bytes. Every load of both builds left 5,000,000 quads in the
dataset.

## Rerun after the review fixes

The review fixes bound the block reader to two growing buffers, load a stream that ends
within 8 MiB into an empty dataset by a transaction, and run the request on a thread of
its own with signal checkpoints. The rerun compares commit `99fb6e95` with commit
`bcef8114`, both release wheels built by `maturin build --release` through
`mbx sparkles-task` from a source archive, as `heavy-review-fixes.sh` does. It ran
under `flock ~/lanes/.heavy.lock`, so no other heavy job ran on atlas, and the load
average was below 9 at its start, from its own builds, and below 4 at its end.

The inputs are `gen.py 5000000` as before, its first 1, 20,000, 80,000 and 120,000 lines,
a Turtle file written from it by the new build, and a gzip copy at level 1.
`ab-review-fixes.sh` runs five interleaved rounds of every case, and
`results-review-fixes.jsonl` holds one line for each load. Every load left the expected
number of quads.

```
ROUNDS=5 ./ab-review-fixes.sh
```
