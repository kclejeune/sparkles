# Running benchmarks

This guide describes how to measure Sparkles: which host to use, how the comparison
engines are pinned, how release builds are configured, and what each benchmark harness
does and writes. [BENCHMARKS.md](BENCHMARKS.md) holds the published results and their
methodology, and [DEVELOPMENT.md](DEVELOPMENT.md) covers building and testing.

* [Choosing a host](#choosing-a-host)
* [Pinned comparison engines](#pinned-comparison-engines)
* [Release build settings](#release-build-settings)
* [Benchmark scripts](#benchmark-scripts)

## Choosing a host

Each kind of claim needs a host that can resolve it.

| Host | Use | Why |
|---|---|---|
| Namespace instance, `mise run bench:nsc A B` | Quick A/B screens of changes of about 10% or more, and checks that a change leaves other queries alone | The instances are noisy shared AMD EPYC virtual machines, so they cannot confirm a change of a few percent. |
| `atlas` or `forge`, Intel Core i5-13500 | Effects under about 5%, queries under 0.3 ms and changes sensitive to code generation | The published numbers come from this microarchitecture, and a result that depends on how one CPU schedules a loop has to be measured on that CPU. |
| `forge` | Published numbers | Every comparison in [BENCHMARKS.md](BENCHMARKS.md) runs there, so new figures stay comparable with the old ones. |

The microarchitecture matters. A fix to the merge join's zipper made `optional-count`
28.5% faster and `minus` 31.9% faster at 1.05M triples on atlas, and the same comparison
measured −1.0% and −1.8% on Namespace. The regression it fixed measured +15% on atlas and
+1.0% on Namespace.

A benchmark on `forge` or `atlas` runs alone. Move builds and other benchmarks off the
host first, run `fstrim` before a batch that loads or writes, and stop each server by its
PID or port before starting the next, because the engines are timed one request at a time
and an untrimmed SSD stalls writes and slows cold reads.

Build both sides of an A/B with the same compilers. The C compiler builds mimalloc, so two
binaries built with different C compilers differ on every request. A binary built outside
the Nix development shell with GCC 16.2 and Clang 21 ran 11 to 33 thousand fewer
instructions per request than the same source built in the shell, enough to make small
queries look slower on the other side. Build both revisions in `nix develop`.
`scripts/nsc-bench.sh` reads the compilers from each binary's `.comment` section and
refuses a pair whose compilers differ, and `readelf -p .comment` shows them for any other
comparison.

Server ports belong below the kernel's range of ephemeral client ports, which is 32768 to
60999 by default (`/proc/sys/net/ipv4/ip_local_port_range`). A client connection that
happened to use a server's port as its own local port stays in TIME-WAIT for 60 s after it
closes, and until then the server cannot bind that port, even with `SO_REUSEADDR`. The
scripts warn about a port base inside the range.

## Pinned comparison engines

The flake exposes `apache-jena`, `apache-jena-fuseki`, `qlever`, `oxigraph` and
`fluree`, plus `benchmark-engines` with all their executables. Build them before a
benchmark so a cache miss or a compiler failure cannot interrupt a timed run.

```sh
nix build .#benchmark-engines
nix build .#qlever .#fluree
```

The Nix workflow builds these packages, the WatDiv and SP2Bench generators
(`watdiv-gen` and `sp2b-gen`) and the nixpkgs tools that the runners use, which
`nix/benchmark-tools.nix` lists, and pushes them to the binary cache at
`https://cache.kclj.io`. A host that trusts the cache downloads them instead of building
them. The flake's `nixConfig` names the cache, which Nix uses after asking or with
`accept-flake-config = true`. Hosts that run benchmarks unattended should list it in their
own `nix.conf` as an extra substituter with the key
`cache.kclj.io-1:StGAmbogIZLS5IAQD2IQCbbmIjv3Sq8rl/AVEw4Sy7s=`. The cache holds
Fluree's release binaries unmodified as well. A test in `scripts/bench-tests` fails when a
runner resolves a package that is not in the cached set.

Jena, Fuseki, QLever and Oxigraph reuse the definitions from the nixpkgs revision in
`flake.lock`. The current versions are Jena 6.2.0, Fuseki 5.1.0, QLever 0.5.48 and
Oxigraph 0.5.11. Fluree 4.2.2 uses the published release archives with SHA-256 hashes
recorded in `nix/fluree.nix`. Its Linux executable is patched to use the locked runtime
libraries. No Fluree source is read or built. The flake permits its Business Source
License package explicitly. Fluree is available on Linux x86-64, Linux ARM64 and macOS
ARM64. That release has no macOS x86-64 archive. The combined package includes Fluree only
on its three supported systems.

The HTTP comparison scripts resolve engines through these flake outputs, even when
another version is on `PATH`, and never through the host's nixpkgs registry, whose
compilers may not build the pinned QLever. Each package gets a GC root in the run's
`gcroots/` directory. `TDBLOADER`, `FUSEKI`, `QINDEX`, `QSERVER`, `OXIGRAPH` and `FLUREE`
can explicitly select other executables. The billion-triple loader also accepts `XLOADER`.
`FLUREE_VERSION`, when supplied without `FLUREE`, must match the packaged version.
Jena loaders and servers clear `JAVA_HOME` so their wrappers use the packaged JDK.
The RDFS oracle also defaults to the locked JDK, with `JAVA` as an explicit override.
Other auxiliary tools prefer `PATH` and otherwise use the repository's locked nixpkgs.

The locked nixpkgs builds QLever with GCC 15.3. GCC 16 rejects range-v3's late
`enable_borrowed_range` specialization for `std::string_view`.
[Upstream nixpkgs](https://github.com/NixOS/nixpkgs/blob/nixos-unstable/pkgs/by-name/ql/qlever/package.nix)
packages QLever 0.6.0 with a
[range-v3 patch](https://github.com/NixOS/nixpkgs/blob/nixos-unstable/pkgs/by-name/ql/qlever/fix-range-v3-borrowed-range-instantiation.patch)
that moves the specialization before its first possible instantiation, and disables
`-Wmaybe-uninitialized` to avoid a separate GCC 16 internal compiler error. A lock update
should keep those fixes and rebuild QLever before results are refreshed.

The binding comparisons keep their language-specific versions. Python's pyoxigraph
and rdflib dependencies are pinned with hashes in
`scripts/bench-bindings/python/requirements.txt`. Oxigraph JS, N3.js and Comunica use
`scripts/bench-bindings/node/package-lock.json` with integrity hashes and `npm ci`.
The JVM comparisons use the Jena version in `jvm/gradle/libs.versions.toml` and Gradle's
dependency locks and verification metadata. These are separate from the HTTP engine
packages because they measure libraries through each language's API.

The JVM driver's `sparql-initial-binding` case requires Jena 5. It calls the legacy
builder method through cached reflection so the driver also compiles with Jena 6,
which removed that API. Timings include the reflective invocation. Asking for this
case with Jena 6 fails before loading the dataset.

## Release build settings

Release builds compile `sparkles-core` as one codegen unit and every other crate with
four. With four units for the engine crate, the split of its functions across units moved
with unrelated changes elsewhere in the crate, and so did the inlining of hot query loops.
A change that touched only SERVICE code once added 4.5% to the instructions per request of
`expr-order-key`. One unit gives `expr-order-key` 6.4% fewer instructions per request on
atlas, and in 24 interleaved rounds on forge it ran 5.3% faster, 12.72 ms against
13.43 ms, with no overlap between the ranges of round medians. `exists-join`, `star-join`
and `employee-docs` moved by 2% or less.

`order-by-full` is the one query that one unit makes slower. It runs the same number of
instructions under both settings, 26.67 billion per request at 10.5M, so it does no extra
work and no call in its sort lost its inlining. In 12 interleaved rounds on forge its
server time was 0.8% longer with one unit, 679.6 ms against 674.1 ms, slower in every
round, and its client time was the same, 1250.4 ms against 1250.5 ms. The difference
comes from where the sort's code lands, and it is small next to the gain on
`expr-order-key`, so the setting stays.

A Namespace screen could not decide this setting. Its geometric mean was 0.997 for server
time, with queries under 2 ms moving 9% to 32% in both directions, so the pinned check on
forge decided it. A release rebuild after a change to `sparkles-core` takes about 35
seconds longer, 2m37s instead of 2m02s on atlas with six jobs.

## Benchmark scripts

### Engine comparison

`scripts/bench.sh` (`mise run bench [people] [workdir]`) compares Sparkles, Jena
(Fuseki), QLever, Fluree and Oxigraph over HTTP. The mise task's `--mode` flag chooses
what it measures. The script itself takes the size and the work directory as positional
arguments and reads the mode from the `MODE` variable, as in
`MODE=updates scripts/bench.sh 100000 target/bench`.

```sh
mise run bench 100000 target/bench                   # load, queries and memory
mise run bench 100000 target/bench --mode updates    # the queries after 5,000 commits
mise run bench 100000 target/bench --mode mixed      # readers and a writer at once
mise run bench 100000 target/bench --mode cold       # a restart with a cold page cache per query
```

The default mode loads every engine, checks the answers and times each query, a
single-triple update and the throughput of 16 concurrent clients. After the loads it
starts one engine's server at a time, with no other server running, checks that
engine's answers, times it and stops it. The fingerprints of all engines go to one
`answers.json`, which the summary and the record compare. Each load runs under GNU
`time`, which records its peak RSS, and the index size is the disk space the store takes.
The harness reads each server's RSS 2 seconds after it starts, after every query has run
once, and after the run. It also records the peak RSS during the throughput run. While it
checks the answers, it resets each server's peak RSS before every query (through
`/proc/<pid>/clear_refs`) and records how far the peak rose. For Sparkles it also reads
the size of the decoded-block cache from `/$/stats`, so the summary can show the RSS
without it. These readings go to `results/mem.json` and the memory tables of
`results/summary.md`.

The `updates` mode measures a store that has taken writes. It copies every engine's
store into `scratch/` in the workdir and serves the copies. Then it sends each engine
the same 5,000 commits (`--churn`), one request each. About 70% insert new
`foaf:knows` edges, types and tags, and the rest delete existing `foaf:knows`,
`foaf:name` and `rdf:type` triples. `scripts/bench-writes.py` generates these commits
once per workdir from `data.nt` with a fixed seed, so every engine gets the same input
and the answers stay comparable. The commit rate and latency go to `churn.json`. The
queries, the answer check and the memory readings then run on the changed stores, and
all results go to `results-updates/`. The churn and the suite run in one server process
per engine, because QLever keeps updates in memory and would lose them in a restart. The
harness never compacts the Sparkles copy, and 5,000 single-triple commits stay below the
10,000-quad minimum of automatic compaction, so its queries read the base index merged
with the delta of the commits.

The `mixed` mode runs each engine alone on a fresh copy of its store. For 30 seconds
(`--mixed-seconds`), 16 clients run `star-join` through `oha` while one writer commits
single-triple inserts as fast as the engine answers, or at `--write-rate` commits per
second. It reports the reads and writes per second and their latency percentiles.
Every request opens a new connection, as the curl requests of the other measurements
do, because on a reused connection QLever's answers wait about 40 ms for a delayed TCP
acknowledgement.

The `cold` mode stops each engine before every query and evicts its files from the
page cache. Then it starts the engine, records the time until it answers, and times one
query. It repeats this 3 times (`--cold-runs`) and keeps the median. The eviction asks
the kernel to drop every file of the store from the page cache (`posix_fadvise` with
`POSIX_FADV_DONTNEED`) from one process, which takes about a tenth of a second even for a
Fluree store of ten thousand files. It does not drop pages that are not yet written back.

The copies of the `updates` and `mixed` modes are removed at the end of the run unless
`KEEP_SCRATCH=1` is set. The servers listen on five ports from `--port-base` (default
3931), so runs with different bases can share a machine. Before a server starts, the
harness waits up to 75 s (`PORT_WAIT`) until its port can be bound, and refuses a port
that another process listens on. Results merge per engine, so `--engines qlever` measures
QLever again and keeps the other engines' numbers.

On a machine with less memory than the engines can use, set `SERVER_MEM_MAX` (for
example `SERVER_MEM_MAX=12G`). Each server then runs in a systemd scope with that memory
limit and no swap. An engine that exceeds it is killed on its own, and its remaining
queries are reported as errors. Without the limit, an engine that grows past the
machine's memory can make the whole machine stop responding.

### Full-text search

`scripts/bench-text.sh` (`mise run bench:text [people] [workdir]`) compares full-text
search on the same generated data. Sparkles and Jena Fuseki both answer `text:query`.
Fuseki serves a TDB2 store wrapped in a jena-text dataset with a Lucene index, and QLever
answers `ql:contains-word` over a text index built from its literals. Fluree is left out
because its documentation offers full-text scoring only in JSON-LD queries, and
Oxigraph has no text search.

The script loads each store without timing it, then times each text index build and
records the index size on disk. Sparkles and Jena index `foaf:name`, `ex:title` and
`rdfs:label`. QLever indexes every literal, since its text index cannot be limited to
predicates, so its queries join the matching literal with the predicate. There are seven
queries:

1. the top 10 by score for a rare word;
2. the top 10 by score for a common word;
3. a count of all hits of the common word;
4. those hits joined with a structural pattern;
5. two words that must both occur;
6. the hits of the common word with highlighted literals, which QLever cannot produce, so
   its form returns plain literals and only the counts are compared;
7. a stemmed search in English titles, which QLever runs for the indexed word, since it
   does not stem.

Before timing, the script compares every engine's hit counts and hit sets with
`scripts/bench-answers.py`. Scores and their order are never compared, because the
engines rank differently. QLever builds its index with explicit scoring, because its BM25
and TF-IDF scoring fail on language-tagged literals in version 0.5.48.

The query words are ones that the Lucene standard analyzer, the Tantivy tokenizer in
Sparkles and QLever split and lowercase the same way. A prefix query is left out because
QLever returns a row for each matching word, so a name with two matching words counts
twice. Sparkles and Jena both take `al*`. The script writes `results/text-summary.md` in
the work directory. `DATA` reuses a dataset that `scripts/bench.sh` generated. `PORT_BASE`
moves the three servers to the ports after it, and its default is 3940.

### DBpedia

`scripts/bench-billion.sh` (`mise run bench:billion [scale]`) runs a comparison on real
data, English DBpedia release 2022.12.01. It takes every English file of the generic,
mappings and text groups and the DBpedia ontology, 1.24 billion triples in all. The files,
their URLs and checksums are in `scripts/bench-billion/dbpedia-2022.12.tsv`. The scale is a
parameter, so routine runs stay small and the full dataset is one command away.

```sh
mise run bench:billion            # about 50M triples, Sparkles and QLever
mise run bench:billion 10m --no-cold --runs 3
mise run bench:billion 250m --engines "sparkles qlever oxigraph"
mise run bench:billion full       # every file: 1.24B triples
```

The first run downloads 12.7 GB, resumable and checksum-verified, into
`target/bench-billion` (`--workdir`) and recompresses it as N-Triples with zstd, which
takes 16 GB more and keeps no uncompressed copies. The release's `.ttl` files are
N-Triples except for 96,021 lines of `images` with a `\n` escape in an IRI, which no
N-Triples parser takes. They are dropped, so every engine loads the same triples. Some
IRIs hold U+FFFD and are not valid RFC 3987 IRIs. QLever, Jena and lenient Oxigraph keep
them, and Sparkles loads them with `sparkles load --lenient`.

A scale below `full` keeps a fixed sample of the subjects, chosen by a hash of the
subject, with all their triples in every file, about that many lines. Files repeat some
triples, so the engines hold fewer distinct ones, 41.1M at `50m`. Every slice contains
the smaller ones. Each scale has its own directory with its data, the engines' indexes
and `results/summary.md`. Downloads, slices and indexes are reused by later runs.
`--reload` rebuilds the indexes, and steps can run alone (`--steps load`,
`--steps "queries report"`).

The queries in `scripts/bench-billion/queries` are DBpedia-style lookups instantiated with
a fixed seed from the entities of the `10m` slice, in the manner of the DBpedia SPARQL
Benchmark, plus analytic queries over the whole dataset such as counts, group-bys, a
property path, and text and range filters. The benchmark's own templates are not copied,
because they were published without a license. `scripts/bench-billion/instantiate.py`
regenerates the queries.

The query phase checks every engine's answers with all selected engines up
(`scripts/bench-answers.py`), stops them, then runs each engine alone through a full
untimed warm pass, the warm timings (hyperfine), the cold timings and two queries under 16
concurrent clients, with RSS sampling. A cold timing is one run after a restart with the
engine's files evicted from the page cache, and `--no-cold` skips it. `SERVER_MEM_MAX`
applies the same per-server memory limit with swap disabled, and `CLIENT_CPU` pins timed
clients when set. Unfinished hyperfine files are kept as diagnostic artifacts and excluded
from generated records. Loads record their time, peak RSS (GNU `time`) and index size.
`LOAD_TIMEOUT`, for example `4h`, stops a load that takes longer, and the load then fails.

`--engines` also takes `jena` (TDB2 `xloader` and Fuseki), `oxigraph` and `fluree`. Fluree
is the release binary that the flake packages. Its bulk import reads a directory of
`.nt.zst` files, but version 4.2.2 fails on a file larger than its chunk size, so the load
step first splits the files into pieces of whole lines of at most 512 MB uncompressed
(`FLUREE_PIECE`), outside the timed load. Jena and Oxigraph store `xsd:float` literals as
values, so latitudes written with different precision that round to the same float are
one triple there and two in Sparkles and QLever, 3 triples at `50m`. `count-all` and
`predicate-counts` then have no majority answer and are not ranked.

### WatDiv

`scripts/bench-watdiv.sh` (`mise run bench:watdiv [scale] [workdir]`) runs the five
engines of `scripts/bench.sh` on WatDiv, the Waterloo SPARQL Diversity Test Suite. Its
20 basic query templates cover linear, star, snowflake and complex query shapes. Scale
factor 1 generates about 112,000 triples. The default of 100 generates 11.0 million,
of which 10.9 million are distinct. The workdir defaults to `target/bench-watdiv`.

WatDiv's generator is a C++ program, and its source is not part of this repository.
The script builds it with Nix from the WatDiv v0.6 release, which
`scripts/bench-watdiv/watdiv.nix` pins by checksum, against the nixpkgs revision in
`flake.lock`. The release seeds its random generators from the clock and reads the
system word list, so the build patches it to use a fixed seed and a pinned word list.
The same scale and seed always produce the same data and the same queries. The seed is
`WATDIV_SEED` and defaults to 1. The generator, the data and the queries are cached in
the workdir.

The generator often draws the same query instance more than once. The script keeps the
first five distinct instances of each template, and `--instances` changes the count.
The three complex templates have no parameters, so each of them has a single instance.
The engines load and serve the data exactly as in `scripts/bench.sh`, and every
engine's answer to every instance is compared before anything is timed.

`results/summary.md` gives each template's geometric mean over its instances. It then
gives the geometric mean of each category and of all templates. These rows only use
the templates that every engine answered correctly, so all engines are measured on the
same queries. `results/instances.md` lists every instance with its row count and
times. `ENGINES=""` only generates the data and the queries.

WatDiv may be used freely on the condition that publications cite G. Aluç, O. Hartig,
M. T. Özsu and K. Daudjee, "Diversified Stress Testing of RDF Data Management
Systems", ISWC 2014.

### Standard suites

`scripts/bench-sp2b.sh`, `scripts/bench-lubm.sh`, `scripts/bench-owl2bench.sh` and
`scripts/bench-bsbm.sh` (`mise run bench:sp2b`, `bench:lubm`, `bench:owl2bench` and
`bench:bsbm`) run SP²Bench, LUBM, OWL2Bench and the Berlin SPARQL Benchmark on the five
engines of `scripts/bench.sh`. They share `scripts/bench-suite-lib.sh`. Each one prepares
pinned data and a fixed query set. The shared code then loads the data into every engine,
starts the servers, checks every answer, times each query with hyperfine, and writes the
run manifest, the result record and `results/summary.md`. The answer check runs with every
server up. After it, the script stops them all and then starts, times and stops one engine
at a time, so each engine's queries and query mixes run with no other server running.

```sh
# small runs that check the setup
mise run bench:sp2b 50k
mise run bench:lubm 1
mise run bench:owl2bench 1
SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 2000

# the runs of the published comparisons, on forge
mise run bench:sp2b 10m --cold-runs 3
mise run bench:sp2b 100m --cold-runs 3
mise run bench:lubm 100 --cold-runs 3 --jena-reasoners "rdfs owl-mini"
mise run bench:owl2bench 10 --jena-reasoners "rdfs owl-micro owl-mini"
# the rdfs materialization of LUBM(1000) alone, for F16's acceptance run
mise run bench:lubm 1000 target/lubm-1000 --engines "" --infer-profiles rdfs
SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m
SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m --usecase bi
SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 100m
```

A published run needs a quiet host with nothing else running. The 10M and 100M runs need
`SERVER_MEM_MAX` set to the same limit for every engine and enough free disk for five
indexes of the data. At 100M triples SP²Bench needs a 14 GB N-Triples file, and its slower
queries can run into the 600 second timeout. A timeout is a result and is printed as one.

The options are the ones of `scripts/bench.sh`, which are `ENGINES`, `QUERIES`, `RUNS`,
`WARMUP`, `SKIP_LOAD`, `ANSWERS_ONLY`, `PORT_BASE`, `SERVER_MEM_MAX` and `CLIENT_CPU`.
`COLD_RUNS=3` adds the cold measurement, a restart with an evicted page cache before each
sample. The suites default to the ports from 28300, and the long write harness to the
ports from 28400, below the ephemeral range.

#### Timeouts

`MAX_TIME` is the longest a request may take, and each suite sets its own
default. The clients give up after `MAX_TIME`, and every server's query timeout is
`MAX_TIME` plus a margin of 15 seconds (`TIMEOUT_MARGIN`), so a server stops a query that
the client has abandoned. A run that fails after taking as long as `MAX_TIME` is recorded
as a `timeout`, whichever side stopped it.

#### Probes and restarts

After the start of each engine's server, every selected query runs
once untimed, and then hyperfine's warm-ups and timed runs follow. Every timing loop, in
these suites and in `scripts/bench.sh`, `scripts/bench-watdiv.sh` and
`scripts/bench-billion.sh`, also sends each query once more untimed just before hyperfine
runs it (`scripts/bench-probe.sh`). An engine whose request fails or reaches `MAX_TIME`
there is recorded with that one sample, as an `error` or a `timeout`, and hyperfine leaves
it out, so a query that cannot finish costs `MAX_TIME` once rather than 12 times. At the
large scales a suite with several slow queries still takes longer by up to `MAX_TIME` for
each of them and each engine. After a failure and after each query's timed runs, every
engine must answer a one-row query within 30 seconds. An engine that does not is
restarted, and `results/restarts.json` names it and the query after which it stopped
answering. This check exists because Fuseki 5.1 can answer its ping while every query
waits forever, after a `StackOverflowError` in `knows-reach` leaves TDB2's node table
cache deadlocked. WatDiv runs its engines together and cannot restart one alone, so there
such an engine is left out of the remaining queries and recorded as an error for them. A
server that is not running when a query is about to be timed, for example because an
earlier query ran it out of memory, is restarted first. A server that does not start, or
that stops answering and does not start again, is left out of the rest of the run, which
goes on with the other engines, writes the record and exits with status 1.

#### SP²Bench

The scale is the number of triples. The generator is SP²Bench v1.01, which is under the
BSD license. `scripts/bench-sp2b/sp2b.nix` builds it from the release, which it pins by
checksum, against the nixpkgs revision in `flake.lock`. The generator seeds its random
numbers with a constant, so a size always gives the same document. The 17 queries come
from the same release. The script converts the generated Turtle to N-Triples once with
`sparkles convert`, so all engines load the same file. Requests time out after 600
seconds.

#### LUBM

The scale is the number of universities, where 100 gives about 13.4M triples. The
generator, UBA 1.7, and the univ-bench ontology carry no license text. The script
downloads them at run time, checks them against `scripts/bench-lubm/pins.tsv` and never
commits them. The 14 queries are in `scripts/bench-lubm/queries.txt`, written out from
the descriptions in the LUBM paper.

Several queries only return their complete answers with inference. The script therefore
materializes the data with `sparkles infer`, once with the `rdfs` profile and once with
the `owl-rl` profile, each on its own copy of the loaded data. `--infer-profiles`
(`INFER_PROFILES`) runs only some of them, which saves the disk of a second dump at 1,000
universities. For each materialization `results/inference.json` and the record get the
wall time, the peak RSS, the number of derived triples, the reasoner's own estimate of
its peak heap and its thread count from `sparkles infer --status`, and the SHA-256 of the
sorted derived triples. The digest lets two builds' closures be compared, and
`DERIVED_DIGEST=0` skips the sort it needs. The derived graph streams from
`sparkles dump` through `sort` into one pass that counts and hashes it, and the run stops
when the count differs from the derived triples that `infer` reported or the dump fails,
with the dump's errors in `logs/`. No copy of the derived graph is written, but `sort`
spills temporary files into the workdir that take about the size of the derived graph in
N-Triples. `INFER_THREADS` sets `--threads`. A Sparkles build that does not report the
peak heap gets `null` there, and its peak RSS still compares. Then the script dumps the
data with the inferred triples of `--profile` (default `owl-rl`), and every engine loads
that one file. The materializations are also printed as a table in `results/summary.md`,
or in `results/inference.md` when no engine is selected.

`--jena-reasoners` (`JENA_REASONERS`) also times Jena's rule reasoners on the same data,
choosing among `rdfs`, `owl-micro`, `owl-mini` and `owl`, the reasoners that Jena's
assembler names. Jena has no command that writes a closure, so `sparql` counts every
triple of an inference model over an in-memory copy of `base.nt`, which makes the forward
and backward rules derive everything. The same count without a reasoner measures the start
of the JVM and the parse of the file, which is recorded apart. Each run is limited to
`JENA_REASON_TIMEOUT` seconds (default 3600) and a heap of `JENA_HEAP` (default 8G), and
a run that hits a limit is recorded as `timeout`, `oom` or `error`. Jena's closure
counts cannot be compared with Sparkles' derived triples, because its rule sets derive
other triples, such as `rdfs:Resource` types and `owl:sameAs` of each individual with
itself. Fluree's documentation describes query-time reasoning with RDFS, OWL 2 QL, OWL 2
RL and Datalog profiles, but the script does not turn it on, so Fluree also answers over
the materialized file. QLever and Oxigraph have no reasoner.

#### OWL2Bench

The scale is the number of universities. The paper's datasets have 1, 2, 5, 10, 20, 50,
100 and 200 universities, and 1 university gives 54,815 triples of TBox and ABox. The
generator and the TBoxes are Apache-2.0, and the 22 queries are a Zenodo record under
CC-BY-4.0. The script downloads both at run time, checks them against
`scripts/bench-owl2bench/pins.tsv` and never commits them. The pinned generator is the
jar of 2020 that made the paper's datasets, run with seed 1 on Java 8 from nixpkgs. The
jar of July 2026 writes its ABox in another namespace than its TBoxes, so no TBox axiom
applies to its data, and the script refuses data generated that way. Two generations of
the same scale give the same file. The script uses the OWL 2 RL TBox and runs the 18
queries whose profile list in the query record contains RL. `queries/profiles.tsv` in the
workdir lists all 22 with their profiles. Materialization, the Jena comparison and the
query comparison work as for LUBM, with `owl-rl` as the only default profile.

The OWL 2 RL closure grows faster than the data. At one university, 1.31 million of the
1.40 million derived triples are `hasSameHomeTownWith`, a symmetric and transitive
property whose closure is a clique per home town, and query q20 returns all of them. The
larger scales are therefore run one after another until a run fails on its limits, and
that failure is a result.

#### BSBM

The scale is a product count or a size such as `10m`. The sizes `1m`, `10m` and `100m`
use the product counts of the datasets that BSBM publishes, which are 2,785, 28,480 and
284,826 products, from the V2.0 specification for 1m and 100m and the 2013 V3.1 results
for 10m. Other sizes get 2,848 products per million. The generator makes about 360 triples
per product, so 2000 products give about 725,000 triples, and the manifest records the
triple count of the data that was generated. The BSBM tools are GPL-3.0, so the script
runs them only when `SPARKLES_ALLOW_GPL_BENCHMARK=1` is set. It downloads them into the
workdir, checks them against `scripts/bench-bsbm/pins.tsv` and runs the generator and the
test driver as separate processes. The generator uses forward chaining and its default
seed. The test driver draws its query parameters from the generator's output with a fixed
seed.

The script runs the driver once against a stub endpoint, on a free port from the kernel,
to record the queries of its own query mixes, and it keeps three distinct instances of
each template. The answers to these instances are compared across engines before timing,
except for the DESCRIBE query, whose result the SPARQL specification leaves to the engine.
When a query with an ORDER BY on plain variables also has a LIMIT or an OFFSET, the check
sends a second, untimed request one row wider on each side of the cut. The rows that tie
with the row past the cut may legitimately differ between engines, so when that second
request shows a tie across the cut, the comparison leaves those rows out and compares only
their key values. Each instance is then timed, and the record pools the instances of a
template into one result.

After that, the driver itself runs the query mix against each engine with 1, 4 and 16
clients (`--clients`) after 50 warm-up mixes and 500 measured ones, and the record gets
its query mixes per hour. The driver does not look at what a query returns, so a failing
or wrongly answering engine would look fast. A run in which the driver exits with an error
is recorded as `error`. Each engine, mix and client count has a wall budget of
`BSBM_MAX_TIME` seconds, defaulting to 1800, separate from the request timeout, because
the driver can continue after a request timeout. At the wall deadline the harness
terminates the driver's process group, allows up to `BSBM_KILL_AFTER` seconds to exit,
defaulting to 5, and kills surviving processes. The interrupted run records `timeout` and
its reason without a throughput value, then the harness checks the server and continues
with the next client count. The result record keeps both limits, the mix counts and the
client counts in its settings.

Afterwards the average result count of each query is compared across the runs in which the
driver succeeded without a timeout. Each engine counts once, and runs with different mix
counts are not compared with each other. Averages that differ by at most 0.05 or 1% count
as equal, because the driver's averages of the same queries differ in the last digit
between client counts. Equality within that margin is not transitive, so when two values
have the most support but different supporters, the query is not judged. A run that
differs from the majority is recorded as `wrong-answer` with the numbers of the queries
that differ, and a failed run keeps its `error` or `timeout`. For DESCRIBE and CONSTRUCT
queries the driver counts the bytes of the RDF/XML it receives, which differ between
engines that return the same triples, so those queries are left out of this comparison. A
server that stops answering during a driver run is restarted before the next one.

The driver runs with `-Dhttp.keepAlive=false`, so every request opens a new connection, as
the curl and `oha` requests of the other measurements do. With Java's default keep-alive,
QLever's answers on a reused connection wait for a delayed TCP acknowledgement, which in a
smoke run with 500 products cut its query mixes per hour from 2,702 to 1,666.

The driver asks for DESCRIBE and CONSTRUCT results as RDF/XML only
(`Accept: application/rdf+xml`) and stops at the first request that fails. QLever 0.5.48
writes no RDF/XML. It answers such a request with 406 Not Acceptable and lists the
formats it supports, which for these queries are Turtle, N-Triples, CSV, TSV and its own
formats. The Explore mix has a DESCRIBE (query 9) and a CONSTRUCT (query 12), so QLever
cannot run it. Before the standard mix, the script sends each engine the first DESCRIBE
and CONSTRUCT instance as the driver would. An engine that refuses one is recorded as
`error` for the standard mix, with the status code in `status_reasons`, and the mix is
not run on it. A second mix, `explore-mix-select` in `results/qmph-select.json`, leaves
those queries out through the driver's own `ignoreQueries.txt` and runs on every engine.
The driver still draws the parameters of the skipped queries, so the other queries are
the same instances in both mixes. This mix compares all five engines, and the standard
mix compares the engines that answer in RDF/XML. Neither the queries nor the driver are
changed, and no engine's answers pass through a proxy.

`--usecase bi` runs the Business Intelligence use case instead of Explore. It has no
DESCRIBE or CONSTRUCT query and runs only its standard mix. The Explore and Update mix is
not run. BI's q4 and q5 average `xsd:float` prices, and engines that add them in different
orders return different floats. `scripts/bench-bsbm/bi-float-oracle.py DATASET.nt QUERIES RAW`
checks each engine's raw q4 and q5 answers against exact means computed from the data,
within the error bound of float summation in any order. The raw answers come from a run
with `ANSWERS_RAW_DIR` set, which makes `scripts/bench-answers.py` keep each engine's
response as `<query>.<engine>.json`.

### Long write runs

`scripts/bench-longwrite.sh` (`mise run bench:longwrite [data] [workdir]`) runs an hour
or more of concurrent reads and writes against each engine of `scripts/bench.sh` in turn,
with compaction under load. The data is a person count for `scripts/gen-data.py` (default
1,000,000, which is 10.5M triples), an N-Triples file, or a directory of `.nt.zst` files
such as a slice of `scripts/bench-billion.sh`, which is decompressed once. Every engine
loads the same file. A workdir keeps the data it was made with and refuses other data.
Every engine then answers the readers' queries on its loaded store. An engine whose
answers differ from the majority is recorded as `wrong-answer` and not run, and one that
fails a query is recorded as `error` and not run, unless `IGNORE_ANSWERS=1` is set. A
majority needs three engines, so when two engines disagree both run and the script says
so. Then, for one engine at a time, the script copies its store, serves the copy and
runs `scripts/bench-longwrite/longrun.py` against it for `--seconds` (default one hour).
The intended runs are one hour per engine on the 10.5M store and one four-hour run of
Sparkles on the same store.

```sh
mise run bench:longwrite 20000 target/lw-smoke --engines sparkles --seconds 120   # smoke
mise run bench:longwrite                                    # 10.5M, five engines, 1 h each
mise run bench:longwrite 1000000 target/lw-4h --engines sparkles --seconds 14400   # 4 h
mise run bench:longwrite ~/.cache/sparkles-bench/billion/50m/data target/lw-dbpedia \
    --engines "sparkles qlever oxigraph"                    # the DBpedia 50M slice
```

The load generator starts 16 reader processes (`--readers`) and one writer
(`--writers`). The readers run a fixed set of short queries in rotation, from
`scripts/bench-longwrite/synthetic.txt` for the synthetic data and from
`scripts/bench-billion/queries` for DBpedia. Each writer inserts and deletes triples of
its own, `<http://example.org/lw/w/n> ex:lwValue "n"`, one at a time or 100 at a time,
with a fixed seed, and deletes only while it holds more than 200,000 of them, so the data
stays near its size while every commit leaves work for compaction. `--write-rate` paces
the writers. Without it each writer sends its next request when the last one is
answered. Every request opens a new connection, as the rest of the harness does. After
an error a reader or writer waits 10 ms, doubling up to one second, before it tries
again. A writer keeps at most 100,000 triples whose writes failed as uncertain and counts
the rest as overflow. On a host with 4 or more CPUs the load generator runs pinned to the
last quarter of them (`CLIENT_CPUS`), and on a smaller host it is not pinned. The
generator runs in its own session under a time limit of the run plus 15 minutes, its
workers exit when it dies, and the script kills the session when it exits.

A monitor samples the server every 5 seconds. It reads the RSS, the disk space allocated to
the store, the write requests completed by the block device that holds the store, the
flushes completed by the whole disks under that device, and for Sparkles the `compaction`
object of `/$/stats/bench`, whose `last` entry gives each compaction's start, kind,
duration and writer lock time. `results/longwrite.json` gets one entry per minute with the
committed writes and triples, the write and read p50 and p99, the errors and timeouts, the
RSS, the disk use, the device counters and whether a compaction ran. The disk space is read
by a separate thread every 60 seconds (`--disk-every`), because `du` over a large store
can take longer than a sample. The summary gives the median of the per-minute values from
the second minute on, since the first minute starts on a cold read set.

The device counters separate the durability classes of X06 §5.1. The device and its disks
carry every process's I/O, so before the run the monitor counts them for 60 seconds with
the server idle (`BASELINE`), and the summary reports the run's counts net of that rate,
per committed write request. With one writer, an engine that synchronizes each commit
makes at least one device write per commit, made durable either by a flush or by a write
with forced unit access, which the kernel does not count as a flush. Sparkles writes its
log with `O_DIRECT` and `O_DSYNC`, so in a one-minute smoke run it made 1.6 device writes
but only 0.03 disk flushes per commit. With several writers, group commit lets commits
share one write, so the ratio is read from a single-writer run. The summary shows both
ratios, and the flushes alone do not decide the class. The kernel counts flushes only on
whole disks, not on partitions or device-mapper nodes, so the monitor follows the store's
device down to its disks through `/sys/block`. On an LVM volume, 500 `fdatasync` calls gave
500 flushes on the NVMe disk and none on the volume.

After the run the script compares the engine's `lwValue` triples with the state the
writers were told about, reading SPARQL JSON results. A write that failed leaves its
triples uncertain, and the check accepts them either way. A value that is not the plain
or `xsd:string` literal the writer sent counts as extra. Then the script kills the server
with SIGKILL, starts it again, which replays the engine's log, and checks again. Last, it
stops the server normally, starts it and checks a third time. Both restarts record the
time from the start to the first answered query, so the summary gives the recovery time
after the SIGKILL beside the time of a normal restart. A server that does not start is
recorded as a failed check and the run goes on to the next engine. Sparkles runs with
automatic compaction at its defaults. TDB2 is not compacted, because its compaction needs
the server stopped or an administrative request, so its disk growth is reported instead.
QLever keeps updates in memory, and its checks after the restarts fail by design. Its RSS
can grow past the host's memory, 27 GiB after about 2,300 write requests in a one-minute
smoke run on 210,000 triples, so a long run sets `SERVER_MEM_MAX` to keep an engine that
outgrows its limit from taking the host down, and such an engine gets the status `error`.
The servers' logs are copied to `logs/` before each store copy is removed.

Each engine gets a status. It is `error` when the state check failed or could not decide,
when no write succeeded, when more than 1% of the reads or writes failed, when the server
went away during the run, or when a `synced` or `written` engine lost acknowledged writes
after the SIGKILL or the restart. It is `wrong-answer` when the engine was not run because
of its answers, and `ok` otherwise. `results/summary.md` has one row per engine with the
status and the checks, and `results/record.json` one `longrun` result for the writes and
one for the reads.

### Result records and regressions

`scripts/bench-record.py` writes and checks the common result record that the standard
suites produce, and `scripts/bench-regressions.py compare` compares records.
`results/record.json` of a suite run holds one entry per engine, query, mode and client
count with the samples in milliseconds, the median, the p95, the answer fingerprint and
whether it agrees with the majority, a status (`ok`, `timeout`, `error`, `oom` or
`wrong-answer`), and for write results the durability class. A long write run has the
mode `longrun`, which needs record version 2. Its samples are the per-minute medians
from the second minute on, and its write result carries the per-minute series, the
compactions, the state checks and the reasons for its status. `compare` refuses a record
whose version it does not know.

`results/manifest.json` next to the record holds the commit of the checkout that holds the
scripts, the Sparkles binary's checksum and, when the caller sets `SPARKLES_COMMIT`, the
commit it was built from. It also holds each engine's version, binary checksum and
license, with the store path for an engine from nixpkgs, the host's CPU, memory, disk
model, kernel, file system and mount options, the memory limit, the client CPU, the data
checksums and the start and end times.

`bench-record.py build` also reads the results directory of `scripts/bench.sh`, and
`--pool-instances` merges instance names such as WatDiv's `L1-3` into one result per
template. `bench-record.py validate FILE` checks a record against the schema in its
header. `build --reuse ENGINE=MANIFEST` carries an engine's results over from an earlier
run whose manifest is MANIFEST, after it checks that the host and the engine's version
and checksum are unchanged, and names the engine under `manifest.reused`.
`manifest --data-path PATH|LICENSE|PARAMS` names an input by its base name, for inputs
whose names contain `=`.

```sh
scripts/bench-record.py build --suite synthetic --scale 10.5m --results target/bench/results \
    --manifest target/bench/results/manifest.json --out docs/benchmarks/2026-11-01-synthetic-forge.json
scripts/bench-regressions.py compare docs/benchmarks/2026-11-01-bsbm-explore-forge.json \
    docs/benchmarks/2026-11-15-bsbm-explore-forge.json
```

`compare` takes two or more records, oldest first, and pairs results by suite, scale,
engine, query, mode and clients. For every pair it prints the ratio of the medians and
whether the sample ranges overlap. A Sparkles result is a regression when its median is
more than 10% slower and the ranges do not overlap, or when it is more than 5% slower in
two comparisons in a row. Throughput results (`qmph` and `qps`) have no samples, so they
are compared by ratio with the same two thresholds, and a drop is the bad direction. A
changed answer fingerprint and a result that stopped being `ok` are always reported. The
exit status is 1 when anything is reported, so a refresh script can stop on it.
`mise run test:bench-scripts` runs the tests of both tools.

### Namespace query screens

`scripts/nsc-bench.sh A B` (`mise run bench:nsc A B`) compares the query times of two
Sparkles builds on an ephemeral [Namespace](https://namespace.so) instance, so a
relative A/B comparison does not have to wait for a quiet local machine. It needs the
`nsc` CLI, logged in to the workspace. A and B are git revisions or paths to `sparkles`
binaries. The script builds each revision locally before it creates the instance,
from a `git archive` of the revision, and caches the binary in `~/.cache/sparkles-nsc`
(`SPARKLES_NSC_CACHE`). The cache keys a binary by its commit alone and builds with
whatever compilers the shell provides, so run it inside `nix develop`. The dataset comes
from `scripts/gen-data.py` and is cached there as zstd. The queries are those of
`scripts/bench.sh` except `export-500k`.

```sh
mise run bench:nsc main my-branch                                   # 1.05M triples, every query
mise run bench:nsc ba547b89 c2fe4b92 --queries "optional-count minus regex-iri"
scripts/nsc-bench.sh --people 1000000 --rounds 4 old/sparkles new/sparkles   # 10.5M triples
```

The script creates one `linux/amd64:8x16` instance with a lifetime cap of two hours
(`--duration`) and destroys it when it exits, also after an error or Ctrl-C. It uploads
the compressed binaries, the data, the queries and `scripts/nsc-bench-remote.sh`, which
runs detached on the instance and needs only bash, busybox, curl and zstd. The binaries
are dynamically linked against the build host's glibc and run under the instance's
glibc loader. Each binary loads its own store from the same data. The remote script
then runs four rounds of A, B, B, A (`--rounds`). Each of these runs a fresh server
process pinned to CPU 2 with `RAYON_NUM_THREADS=1`, and the client and curl are pinned
to CPU 4, a different physical core. A busy loop at the lowest scheduling priority runs
on both CPUs, so that their vCPUs do not halt between requests (`--no-spin` turns it
off), which halves the variation of the timings. In each server process every query gets
3 untimed and 20 timed requests (`--warmup`, `--reps`). A timed request records the
server's execution time (`execMs` of `application/x-sparkles+json`) and curl's wall time.
Before the first timed run, each binary's sorted TSV answer to every query is checksummed.

The results go to `target/nsc-bench/<time>` (`--out`). `summary.md` has, for each
query and for both measures, the medians of A and B, the ratio B/A, the ratio within
each round and the spread of each binary between its server processes, followed by the
geometric mean of the ratios. It also reports differing answers and both load times.
`manifest.txt` records each binary's commit and SHA-256, the instance, the time of each
phase and the billed vCPU-minutes. `results/samples.tsv` has every timed request.

The C compiler builds mimalloc, so two binaries built with different C compilers differ
on every request, by 11 to 33 thousand instructions per request between GCC 16.2 with
Clang 21 and the development shell's GCC 15.3 with Clang 22. The script therefore reads
the compilers of each binary from its `.comment` section, which names rustc, the linker
and the C and C++ compilers of its native dependencies, and writes them to
`manifest.txt`. It refuses to compare two binaries whose compilers differ or cannot be
read, unless `--allow-mixed-compilers` is given. A revision it builds also gets a
`build.txt` beside the cached binary with the shell's `cc`, `c++` and `rustc` versions
and the cargo flags. A cache entry that a build outside the shell left behind has to be
removed before the revision is built again in the shell.

A run of all 27 queries at 1.05M triples takes about five minutes from creation to
destruction, of which the instance spends about 10 seconds receiving 224 MB of
binaries and data. Namespace bills it as 40 to 48 vCPU-minutes, about 7 cents at the
overage rate. Building the two revisions locally usually takes longer than the run.

The instances are AMD EPYC (Zen 4) virtual machines with 4 cores and 8 threads, and
they are noisier than a dedicated host. Within one server process, the interquartile
range of a query's execution time is about 10 to 15% of its median. Medians of the
same binary in different server processes differ by 10 to 30%. In a control run that
compared one binary with itself, each ratio for a query of 0.3 ms or more was within
6% of 1, and the geometric mean was 0.986. Queries under 0.1 ms vary by 20% or more
and say nothing. The script is therefore suited to changes of about 10% or more and
to checking that a change leaves the other queries alone. It cannot confirm a change
of a few percent, and [Choosing a host](#choosing-a-host) says where to measure those.

### Namespace write screens

`scripts/nsc-writes.sh` stages write workloads before creating a capped Namespace
instance. Its revision builds use `SPARKLES_NSC_CACHE/writes-v1`, separately from the
query harness. Each source commit and exact `--cargo-args` string has its own build key,
target directory and cross-process lock. The lock covers publication, verification and
compressed staging. Each staged revision binary has a `bin-<id>.build.json` recording its
source, flags, original compiler information and binary hash. A cache hit reuses that
original provenance. Missing or inconsistent receipts refuse reuse without deleting the
entry.

This cache identifies source and build arguments. It does not make compiler, environment
or external configuration changes hermetic, so use a new cache root for a rebuild under a
changed environment. File arguments are recorded as externally built snapshots with their
supplied path and staged hash. The launcher gives each default run a unique results
directory. `--out` requires a new destination and refuses any existing file, directory or
symlink before opening evidence files. `--index text`, `geo` or `embed` builds a
full-text, spatial or embedding index on each store after loading it, and with `embed` the
servers run with `--no-embedding`, so nothing leaves the instance. Each run's
`metrics-<run>.txt` also records the server's peak resident memory.

The instances synchronize in about 0.2 ms, which hides the cost of a slow device.
`delay-us=4000` adds 4 ms to every synchronization and write-through write, which
brings the ordinary writer down to the 230 transactions/s it reached on forge. Reader tails do not settle on
these instances, so measure them on forge.

`scripts/write-bench.rs` is the client of `scripts/nsc-writes.sh`. It sends each update
as one request on a kept-alive connection and requires distinct, contiguous receipts.
It sends a request again on a new connection only when writing it failed or the server
had closed the idle connection before it was sent. A request whose answer cannot be read
is an error that fails the run, because the server may have committed it. With
`--write-rate N`, the writers of the `writers` and `mixed` scenarios send N requests per
second in total, each client on its own schedule, so two variants can be compared at the
same commit rate. `nsc-writes.sh --write-rate N` passes the rate to those scenarios, and
variant names must be shell identifiers.

### Other scripts

* `scripts/gen-data.py N` generates a synthetic dataset for benchmarking.
* `scripts/gen-geo.py N` generates a GeoSPARQL dataset and its queries. The data has
  points around cities, lines, polygons and an administrative hierarchy.
* `scripts/bench-geo.sh N` times the GeoSPARQL queries with and without the spatial
  index, and the nearest-neighbour query with the spatial rewrites off. It first checks
  that every run gives the same answers.
* `scripts/geosparql-benchmark.sh` runs the GeoSPARQL Compliance Benchmark. The
  benchmark is GPL-2.0, so it is never added to the repository. The script fetches it
  into `target/` at a pinned commit, and only when `SPARKLES_ALLOW_GPL_BENCHMARK=1` is
  set.
* `scripts/backup-bench.sh DB` (`mise run bench:backup DB`) backs up an existing
  database to an `fs` repository. It times a full backup, an incremental backup after
  small commits, a restore and a data verification.
