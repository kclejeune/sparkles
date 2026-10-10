# Running benchmarks

This guide describes the benchmark harnesses, their modes, outputs and measurement
limits. [BENCHMARKS.md](BENCHMARKS.md) owns the published results, measurement context
and comparison methodology. [DEVELOPMENT.md](DEVELOPMENT.md) covers building and testing.

## Benchmark scripts

* `scripts/bench.sh` (`mise run bench [people] [workdir]`) runs the engine comparison.
  It compares Sparkles, Jena (Fuseki), QLever, Fluree and Oxigraph over HTTP. The mise
  task's `--mode` flag chooses what it measures. The script itself takes the size and
  the work directory as positional arguments and reads the mode from the `MODE`
  variable, as in `MODE=updates scripts/bench.sh 100000 target/bench`.

  ```sh
  mise run bench 100000 target/bench                   # load, queries and memory
  mise run bench 100000 target/bench --mode updates    # the queries after 5,000 commits
  mise run bench 100000 target/bench --mode mixed      # readers and a writer at once
  mise run bench 100000 target/bench --mode cold       # a restart with a cold page cache per query
  ```

  The default mode loads every engine, checks the answers and times each query, a
  single-triple update and the throughput of 16 concurrent clients. Each load runs under
  GNU `time`, which records its peak RSS, and the index size is the disk space the store
  takes. The harness reads each server's RSS 2 seconds after it starts, after every query
  has run once, and after the run. It also records the peak RSS during the throughput
  run. While it checks the answers, it resets each server's peak RSS before every query
  (through `/proc/<pid>/clear_refs`) and records how far the peak rose. For Sparkles it
  also reads the size of the decoded-block cache from `/$/stats`, so the summary can show
  the RSS without it. These readings go to `results/mem.json` and the memory tables of
  `results/summary.md`.

  The `updates` mode measures a store that has taken writes. It copies every engine's
  store into `scratch/` in the workdir and serves the copies. Then it sends each engine
  the same 5,000 commits (`--churn`), one request each. About 70% insert new
  `foaf:knows` edges, types and tags, and the rest delete existing `foaf:knows`,
  `foaf:name` and `rdf:type` triples. `scripts/bench-writes.py` generates these commits
  once per workdir from `data.nt` with a fixed seed, so every engine gets the same input
  and the answers stay comparable. The commit rate and latency go to `churn.json`. The
  queries, the answer check and the memory readings then run on the changed stores, and
  all results go to `results-updates/`. The harness never compacts the Sparkles copy,
  and 5,000 single-triple commits stay below the 10,000-quad minimum of automatic
  compaction, so its queries read the base index merged with the delta of the commits.

  The `mixed` mode runs each engine alone on a fresh copy of its store. For 30 seconds
  (`--mixed-seconds`), 16 clients run `star-join` through `oha` while one writer commits
  single-triple inserts as fast as the engine answers, or at `--write-rate` commits per
  second. It reports the reads and writes per second and their latency percentiles.
  Every request opens a new connection, as the curl requests of the other measurements
  do, because on a reused connection QLever's answers wait about 40 ms for a delayed TCP
  acknowledgement.

  The `cold` mode stops each engine before every query and evicts its files from the
  page cache. Then it starts the engine, records the time until it answers, and times one
  query. It repeats this 3 times (`--cold-runs`) and keeps the median.

  The copies of the `updates` and `mixed` modes are removed at the end of the run unless
  `KEEP_SCRATCH=1` is set. The servers listen on five ports from `--port-base` (default
  3931), so runs with different bases can share a machine. Results merge per engine, so
  `--engines qlever` measures QLever again and keeps the other engines' numbers.

  On a machine with less memory than the engines can use, set `SERVER_MEM_MAX` (for
  example `SERVER_MEM_MAX=12G`). Each server then runs in a systemd scope with that memory
  limit and no swap. An engine that exceeds it is killed on its own, and its remaining
  queries are reported as errors. Without the limit, an engine that grows past the
  machine's memory can make the whole machine stop responding.
* `scripts/bench-text.sh` (`mise run bench:text [people] [workdir]`) compares full-text
  search on the same generated data. Sparkles and Jena Fuseki both answer `text:query`.
  Fuseki serves a TDB2 store wrapped in a jena-text dataset with a Lucene index, and QLever
  answers `ql:contains-word` over a text index built from its literals. Fluree is left out
  because its documentation offers full-text scoring only in JSON-LD queries, and
  Oxigraph has no text search.

  The script loads each store without timing it, then times each text index build and
  records the index size on disk. Sparkles and Jena index `foaf:name`, `ex:title` and
  `rdfs:label`. QLever indexes every literal, since its text index cannot be limited to
  predicates, so its queries join the matching literal with the predicate. There are seven
  queries. Two take the top 10 by score, one for a rare word and one for a common word.
  The third counts all hits of the common word, the fourth joins them with a structural
  pattern, and the fifth asks for two words that must both occur. The sixth returns the
  hits of the common word with highlighted literals, which QLever cannot produce, so its
  form returns plain literals and only the counts are compared. The seventh is a stemmed
  search in English titles, which QLever runs for the indexed word, since it does not stem.
  Before timing, the script
  compares every engine's hit counts and hit sets with `scripts/bench-answers.py`. Scores and their order are never compared,
  because the engines rank differently. QLever builds its index with explicit scoring,
  because its BM25 and TF-IDF scoring fail on language-tagged literals in version 0.5.48.

  The query words are ones that the Lucene standard analyzer, the Tantivy tokenizer in
  Sparkles and QLever split and lowercase the same way. A prefix query is left out because
  QLever returns a row for each matching word, so a name with two matching words counts
  twice. Sparkles and Jena both take `al*`. The script writes
  `results/text-summary.md` in the work directory. `DATA` reuses a dataset that
  `scripts/bench.sh` generated. `PORT_BASE` moves the three servers to the ports after
  it, and its default is 3940.
* `scripts/bench-billion.sh` (`mise run bench:billion [scale]`) runs a comparison on real
  data: English DBpedia, release 2022.12.01 (every English file of its generic, mappings
  and text groups, and the DBpedia ontology), 1.24 billion triples in all. The files, their
  URLs and checksums are in `scripts/bench-billion/dbpedia-2022.12.tsv`. The scale is a
  parameter, so routine runs stay small and the full dataset is one command away:

  ```sh
  mise run bench:billion            # about 50M triples, Sparkles and QLever
  mise run bench:billion 10m --no-cold --runs 3
  mise run bench:billion 250m --engines "sparkles qlever oxigraph"
  mise run bench:billion full       # every file: 1.24B triples
  ```

  The first run downloads 12.7 GB (resumable, checksum-verified) into
  `target/bench-billion` (`--workdir`) and recompresses it as N-Triples with zstd (16 GB
  more, no uncompressed copies). The release's `.ttl` files are N-Triples except for
  96,021 lines of `images` with a `\n` escape in an IRI, which no N-Triples parser takes:
  they are dropped, so every engine loads the same triples. Some IRIs hold U+FFFD and are
  not valid RFC 3987 IRIs. QLever, Jena and lenient Oxigraph keep them, and Sparkles
  loads them with `sparkles load --lenient`.

  A scale below `full` keeps a fixed sample of the subjects (by a hash of the subject),
  with all their triples in every file, about that many lines. Files repeat some
  triples, so the engines hold fewer distinct ones (41.1M at `50m`). Every slice contains
  the smaller ones. Each scale has its own directory with its data, the engines' indexes
  and `results/summary.md`. Downloads, slices and indexes are reused by later runs;
  `--reload` rebuilds the indexes, and steps can run alone (`--steps load`,
  `--steps "queries report"`).

  The queries (`scripts/bench-billion/queries`) are DBpedia-style lookups instantiated
  with a fixed seed from the entities of the `10m` slice, in the manner of the DBpedia
  SPARQL Benchmark (whose templates are not copied: they were published without a license), plus
  analytic queries over the whole dataset: counts, group-bys, a property path, text and
  range filters. `scripts/bench-billion/instantiate.py` regenerates them. Every engine's
  answers are compared first (`scripts/bench-answers.py`), then each query is timed warm
  (hyperfine), cold (one run after a restart with the engine's files evicted from the
  page cache, which `--no-cold` skips), and two of them under 16 concurrent clients. Loads
  record their time, peak RSS (GNU `time`) and index size. `--engines` also takes `jena`
  (TDB2 `xloader` and Fuseki), `oxigraph` and `fluree`. Fluree is the release binary that
  `scripts/bench.sh` downloads. Its bulk import reads a directory of `.nt.zst` files, but
  version 4.2.2 fails on a file larger than its chunk size, so the load step first splits
  the files into pieces of whole lines of at most 512 MB uncompressed (`FLUREE_PIECE`),
  outside the timed load.
  `LOAD_TIMEOUT` (for example `4h`) stops a load that takes longer, and the load then
  fails. Jena and Oxigraph store `xsd:float`
  literals as values, so latitudes written with different precision that round to the
  same float are one triple there and two in Sparkles and QLever (3 triples at `50m`):
  `count-all` and `predicate-counts` then have no majority answer and are not ranked.
* `scripts/bench-watdiv.sh` (`mise run bench:watdiv [scale] [workdir]`) runs the five
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
* `scripts/nsc-bench.sh A B` (`mise run bench:nsc A B`) compares the query times of two
  Sparkles builds on an ephemeral [Namespace](https://namespace.so) instance, so a
  relative A/B comparison does not have to wait for a quiet local machine. It needs the
  `nsc` CLI, logged in to the workspace. A and B are git revisions or paths to `sparkles`
  binaries. The script builds each revision locally before it creates the instance,
  from a `git archive` of the revision, and caches the binary in `~/.cache/sparkles-nsc`
  (`SPARKLES_NSC_CACHE`). The dataset comes from `scripts/gen-data.py` and is cached
  there as zstd. The queries are those of `scripts/bench.sh` except `export-500k`.

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
  off). In a test with one binary, this halved the variation of the timings. In each
  server process every query gets 3 untimed and 20 timed requests (`--warmup`, `--reps`). A timed request records the server's
  execution time (`execMs` of `application/x-sparkles+json`) and curl's wall time. Before
  the first timed run, each binary's sorted TSV answer to every query is checksummed.

  The results go to `target/nsc-bench/<time>` (`--out`). `summary.md` has, for each
  query and for both measures, the medians of A and B, the ratio B/A, the ratio within
  each round and the spread of each binary between its server processes, followed by the
  geometric mean of the ratios. It also reports differing answers and both load times.
  `manifest.txt` records each binary's commit and SHA-256, the instance, the time of each
  phase and the billed vCPU-minutes. `results/samples.tsv` has every timed request.

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
  of a few percent.

  The instances also have a different microarchitecture from the Intel machines used
  for the published benchmarks. On Atlas, the merge zipper fix in `c2fe4b92` made
  `optional-count` 28.5% faster and `minus` 31.9% faster at 1.05M triples. On Namespace,
  the same comparison measured −1.0% and −1.8%. The regression it fixed, from
  `c4b49fab`, also measured +1.0% on Namespace, against +15% on Atlas. The other queries
  agreed with Atlas within the noise. A result that depends on how one CPU schedules a
  loop has to be measured on that CPU.
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
