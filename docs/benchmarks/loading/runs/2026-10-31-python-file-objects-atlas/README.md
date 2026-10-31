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
