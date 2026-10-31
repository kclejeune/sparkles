# Jena's syntaxes through the loader, local run

This run compares `sparkles load` of RDF Thrift, RDF Protobuf, TriX and RDF/JSON files
before and after the loader read them directly. The base build is commit `34b69139`, which
rewrote each file as N-Quads in a temporary file first. The new build is the branch tip
after the commit "Read Jena's syntaxes straight from their streams", with the batches of
one decoder passed to parallel sinks. Both are release builds of
`mbx build --release -p sparkles-server --bin sparkles`.

The host is a laptop with an Intel Core Ultra X7 358H (16 cores) and 30 GiB of RAM that
ran other builds and tests during the run, with a load average between 7 and 25. The runs
are interleaved, so both builds saw the same load. This is a local screen and not a forge
measurement.

The inputs are `scripts/gen-data.py 100000` (1,052,807 triples, 110 MB as N-Triples) and
`scripts/gen-data.py 500000` (5,263,584 triples, 559 MB). `sparkles convert` wrote the
four syntaxes from the N-Triples file with the base build.

`run.py` loads each file into a fresh database with `TMPDIR` set to an empty directory,
measures wall time of the whole `sparkles load` command, samples the size of that
directory every 20 ms for the peak temporary disk use, and reads the peak RSS of the
process from `wait4`.

```
python3 run.py 7 ./sparkles-base ./sparkles-new data.rt data.rpb data.trix data.rj
python3 run.py 5 ./sparkles-base ./sparkles-new big/data.rt big/data.rpb
```

`results-1.05m-single-decoder.txt` is a first build in which the decoder also encoded the
quads on its own thread. It measured level with the base build and led to the batches.
`results-5.26m-first-pass.txt` is a first pass of the larger run on a busier host.
