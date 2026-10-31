# Jena's syntaxes through the loader, atlas run

This run repeats the larger comparison of the [local run](../2026-10-30-jena-syntaxes-local/README.md)
on a quiet host, with the final loader. The base build is commit `34b69139`, which
rewrote each Jena file as N-Quads in a temporary file first. The new build is commit
`23a95ac7`, which reads the file straight from its stream, passes the batches of one
decoder to parallel sinks, bounds those batches by bytes, and holds the decoded quads to
the decompressed-size cap with `nquads_len`. Both are release builds of
`mbx build --release -p sparkles-server --bin sparkles` from a source archive of the
commit.

The host is atlas, an Intel Core i5-13500 with 20 logical CPUs and 15 GiB of RAM. Its
load average was below 3 when the run began, and no build or other job ran during it. The
rounds are interleaved, as before.

The input is `scripts/gen-data.py 500000` (5,263,584 triples, 559 MB as N-Triples).
`sparkles convert` of the base build wrote the RDF Thrift file (610 MB) and the RDF
Protobuf file (611 MB) from the N-Triples file. The script is the one of the local run.

```
python3 run.py 5 ./sparkles-base ./sparkles-new data/data.rt data/data.rpb
```

`results-5.26m.txt` is the output of that command. Both builds loaded 5,263,576 quads in
every round. The new build wrote no temporary file in any round.
