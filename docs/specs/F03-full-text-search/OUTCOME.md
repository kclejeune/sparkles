# F03: Full-text search (Tantivy, `text:query`) — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented in part (Phases 1 and 2, most of Phase 3)
>
> **Phases:** Phase 1 shipped. It covers the `text` cargo feature, `text:query`, per-quad
> documents kept current in the commit path, catch-up or rebuild at open,
> `sparkles text-index` and `/$/text/{ds}`. Two Phase 2 items shipped with it:
> `PUT`/`DELETE /$/text/{ds}` and the UI (an index admin panel and ranked search in
> Explore). Later, Lucene's query syntax replaced the planned Sparkles grammar, and
> highlighting with snippets and stemming per language shipped. Of Phase 3, the fast
> fields shipped with index format 2, and hybrid retrieval with vector search shipped as
> `spk:hybridSearch` ([F04](../F04-vector-search.md#outcome)). On 2026-10-03 `text` on
> dataset creation, online rebuilds with a journal, variable query strings with
> bound-subject pushdown, incremental bulk maintenance and rebuilds over the literal
> range shipped, and so did a `porter` analyzer for English. Missing/damaged indexes
> now recover in the background at persistent open. Facets, search at historical
> snapshots and an opt-in stale mode are not built.
>
> **User docs:** [API: Full-text search](../../API.md#full-text-search) · [Features](../../search/README.md#search) · [Benchmarks: Full-text index and observability](../../BENCHMARKS.md#full-text-index-and-observability-105m-triples)
>
> This is the design as written before implementation. The [Outcome](SPEC.md#outcome) section at
> the end records how it landed.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 (`1120061`), right after durable commit
identity, and mostly as specified. Each quad is one Tantivy 0.26.2 document, keyed by a hash
of its terms' vocabulary keys. A planner leaf applies the graph, predicate, language and
subject filters inside the search. The rest is as designed: `text.json` and `text/`,
`sparkles text-index`, `serve --text`, `GET /$/text/{ds}` and `POST /$/text/{ds}/rebuild`.
Two Phase 2 items came with it or soon after: `PUT`/`DELETE /$/text/{ds}`, and the UI with
an admin panel and ranked search in Explore. Clones keep text search enabled (`89b455c`).

**Background startup recovery.** Persistent open classifies the derived index before
allocating its writer or cleaning staging. Healthy verification and covered WAL catch-up
remain synchronous. Missing, damaged, ahead, mismatched and uncovered indexes instead
publish configured unavailable views and recover from RDF on a small process-wide worker
pool. Linked inherited WAL changes are loaded only for a reusable index needing catch-up.
A new linked branch copies its upstream's committed checkpoint into its own directory
when the upstream store is open with a ready index and that checkpoint is at or before
the fork point. The copy is then caught up from the inherited WAL before the branch
opens, so the branch serves text searches at once. A checkpoint past the fork point may
hold upstream commits the branch lacks, so it is never copied. When no checkpoint can be
copied, the branch index rebuilds in the background in its own directory, and the
upstream is never checkpointed by the branch. A clone builds its full-text index from
its own data before `Store::clone_to` returns, and opening the clone then reuses that
index. The source's checkpoint is not copied, because it names a commit of the source and
the clone starts again at commit 0. The build honours the clone's cancel flag and
deadline, and a clone stopped during it leaves no directory behind. In-memory clones
enable text search synchronously. Restored datasets whose backups omit `text/` use the
same configured recovery path.

RDF reads and writes remain available. Text searches return 503 while rebuilding or after
failure; disabled text search still returns 400. Retained unavailable snapshots stay
unavailable, and ready cache entries cannot bypass this state. Concurrent effective quad
changes feed a bounded journal, with final presence checked against the caught-up snapshot.
Generation changes, journal overflow and catch-up that cannot close on the head all
restart the build within a fixed budget. The pool has one worker per four CPUs, with at
least one and at most two. Its queue holds weak entries without a bound, so a dataset is
never failed because many datasets need recovery at once, and the entry of a closed store
is dropped. A job is queued once even when requested again. Only an active job pins a
snapshot. Each active build uses at most two indexing threads with a shared 64 MiB writer
arena budget, or one thread on a single-CPU host. Its journal holds at most 131,072
changed quads. A run allows three build attempts, eight catch-up rounds per attempt and a
final writer-held tail of at most 1,024 records. When a run uses up its attempts, the job
is queued again after 5 seconds, then 30 seconds, then 120 seconds, and the status message
names the retry. After the third retry fails the text status is `failed`, and an explicit
rebuild retries it. An explicit rebuild during a retry delay starts the retry at once.
Other failures, such as a quota or disk reserve refusal, are not retried automatically.
A failure is published in the dataset's snapshot when it is recorded, so text searches
report `failed` without waiting for another commit. Native logs retain failure details.

Ready publication verifies the current owner and generation under writer admission, then
completes the directory rename/fsync fence. Disable, reconfigure and Store close cancel
and join recovery before releasing the root or staging owner. A queued manual join releases
the lifecycle lock, so disable/reconfigure can cancel it without waiting for another
dataset's build. Retry metadata updates preserve concurrent RDF publication and keep group
admission excluded continuously. A ready view's owner also distinguishes same-commit
configuration replacements in the result cache. Same-family callbacks are refused before
lifecycle locks or joins and keep their query/commit failure sticky.

Explicit enable/reconfigure remain synchronous. Explicit rebuild is a synchronous online
build or a join/retry of startup recovery. HTTP task cancellation retains the existing
synchronous native rebuild behavior; this change introduces no per-waiter Ctl API.
An admitted directory publication fence completes before automatic cancellation returns.
Cancellation is checked between batches of documents and before each blocking step of a
build. Tantivy segment merges and the publication fence cannot be interrupted, so a close
or disable that arrives during them waits for them to finish.
Disk reserve is checked at recovery checkpoints, and an authoritative dataset quota check
refuses growing ready publication. A preliminary check walks the dataset directory before
final writer admission. After applying the final tail, recovery checks again under the
writer lock so concurrent writes and quota changes cannot invalidate the result. Writers
wait for this final walk. Derived staging can consume transient disk space before that
final check; refusal removes staging and preserves RDF. This does not add a hard
build-space reservation.

Native controls cover missing/damaged indexes, concurrent writes, generation and journal
restart, catch-up round restart, delayed automatic retries, more queued jobs than one
worker can take, branch checkpoint seeding, cancellation and close, owner/cache isolation, failed installation,
metadata/WAL classification, lazy linked catch-up, callback refusal, quota/reserve refusal
and explicit retry. Child-process interruption at build and both directory-renaming
boundaries survives reopen with the same RDF identity/head. These are process-crash
checks, not simulated power-loss measurements.

**Deviations, and why.**
- *Commit path.* A Tantivy commit with an fsync on every write cost too much (§5.3, open
  question 11). The first change (`8f3eb13`) stopped fsyncing index commits. The WAL is the
  durable record, and a checkpoint about once a second bounds the unsynced window. At open,
  a dirty index is verified by checksum and caught up from the WAL instead of rebuilt.
  A segment file torn shorter than Tantivy's 8-byte footer trailer counts as damaged,
  because Tantivy's own check panics on it. The
  second change (`9bc3db2`, `2cc3361`) made the commit lazy. A write only stages its
  documents. They are committed by the next text query that needs them, by a tick about
  once a second, or once about 16,000 changes are staged. Each snapshot still searches
  exactly its own documents, so read-your-writes holds.
  [COMPARISON.md](../../COMPARISON.md#divergences-from-jena--qlever-decisions) records this as
  a design decision.
- *States.* Ready indexes report `ready` or `stale`; automatic startup recovery reports
  `rebuilding` or `failed`. Explicit HTTP rebuilds still return `202` with a
  `text-rebuild` task.
- *Limits.* `maxHits` is reported through the shared budget error (`507`, row budget) that
  came with per-request budgets ([C01](../C01-observability-and-budgets.md)).
- *Historical reads.* A `text:query` against an `?at=` snapshot
  ([F06](../F06-snapshots-and-point-in-time.md)) answers `501` (`history-unsupported`), not
  `503`.
- *Doc store.* The compression work ([X01](../X01-compression-codecs.md)) made zstd the default
  for the doc store (`docstoreCompression`). That brings in tantivy's `zstd-compression`
  when the `zstd` feature is on.

**Tests at landing.** Integration tests in `crates/sparkles-core/tests/text.rs` cover the
acceptance examples, with a test hook that fails an index commit. Server tests cover the
routes, and the Playwright smoke tests include text search.

**Performance** ([benchmarks](../../BENCHMARKS.md#full-text-index-and-observability-105m-triples),
10.5M triples). Building the index over every string literal takes 8.0 s and 102 MB on
disk. With the lazy commit, a 1,000-triple insert takes 28.4 ms with text search on, against
24–26 ms with it off and 46.2 ms with a commit per write. A top-10 `text:query` takes about
15 ms.

**Query syntax, limits and search cost (2026-10-02).** The full-text benchmark
(`scripts/bench-text.sh`) compared Sparkles with Fuseki and jena-text and found three
problems, which this change fixed.

- *Query syntax.* Tantivy's query parser found nothing for a single word with a trailing
  `*`, so `al*` and `+ada +lov*` returned no rows and no error. Instead of the small grammar
  that Phase 2 planned, Sparkles now parses Lucene's classic query syntax itself
  (`text/lucene.rs`) and applies Lucene's rules for combining clauses. Words, phrases,
  slops, prefixes, wildcards, fuzzy words, regular expressions, ranges, boosts and the
  boolean operators all work. On the benchmark data, 67 query strings that cover these
  forms matched the same literals as in Jena, and both engines refused 5 malformed ones.
  A sloppy phrase uses Lucene's semantics,
  which accept the words in either order, so it has its own Tantivy query. A fuzzy word
  expands to at most 50 terms, as Lucene's `FuzzyQuery` does. Forms that would find
  nothing or that Sparkles cannot reproduce give `400`. These are field names, a query
  with only excluded words or with no word left after analysis, and a few Lucene regular
  expression operators. The phrase prefix `"quick bro"*` stays a Sparkles extension, which
  Jena reads as the phrase or any document.
- *Limits above `maxHits`.* A search with an explicit limit above `maxHits` fetched
  `maxHits + 1` hits, treated them as all hits and returned them without an error. Such a
  limit now counts as no limit, so more than `maxHits` hits give `507`.
- *Cost per hit.* Resolving each hit loaded its stored document, which cost a doc store
  block decompression per hit. Index format 2 keeps `s`, `p`, `o` and `g` in columns
  (fast fields) and stores nothing, so the Phase 3 item that planned fast fields for `s`
  and `g` is done. A search reads only the columns its outputs need, decodes each distinct
  term once in sorted order, and caches the term ids per segment for the store generation.
  The planner drops a score or literal output that the query uses nowhere else, so a
  `COUNT` or a subject join reads no literal. A search without a limit collects its hits
  without a top-k heap. On 1.05M triples, counting the 4,937 hits of a common word went from
  about 20 ms to about 2 ms per HTTP request, and joining them with a structural pattern
  went from about 20 ms to about 3 ms. These are medians of 40 requests on a busy machine,
  with the id cache warm. Without the cache, each distinct term costs a dictionary lookup
  of about half a microsecond. The `docstoreCompression` setting is still accepted, but it
  no longer changes the index size.

**Highlighting (2026-10-02).** Jena's `highlight:` argument works, with Jena's options
and defaults (`m:`, `z:`, `s:`, `e:`, `f:`, `jh:` and `jf:`). Sparkles does not use
Tantivy's `SnippetGenerator`, because it returns a single fragment and does not see the
terms of prefix, wildcard and fuzzy queries. `text/highlight.rs` follows Lucene's
`Highlighter` with a `SimpleFragmenter` instead. It cuts fragments at the same tokens,
scores them by their distinct query words, keeps the best, merges adjacent ones, and marks
a phrase only where it occurs. The query parser records what each word that is not
excluded matches, including the terms a fuzzy word expanded to. The literal's text comes
from the store's dictionary, so the index stores no text, and a search without
`highlight:` reads none. On the benchmark data, 14 highlighted queries returned the same
literals as Jena, fragments and marks included. One difference is deliberate. Jena drops
the language tag of a highlighted literal unless its index has a language field, while
Sparkles always keeps it. `GET /{ds}/text` from §2.2 returns ranked hits with HTML
snippets, escaped by Sparkles with the matches in `<mark>`. The full-text benchmark has a
sixth query for highlighting, on which Fuseki also runs.

**Stemming per language (2026-10-02).** Phase 2 planned stemmed fields per language. They
follow jena-text's `text:multilingualSupport` with a `text:langField`, where a tagged
literal is indexed a second time with its language's analyzer and a search with
`lang:xx` searches that field.

- *Configuration.* `languages` in `text.json` is `"all"`, a list of primary language
  tags, or a map from a tag to an analyzer name. `sparkles text-index --language`
  sets it. Each language gets a field `text_<tag>`, and the schema depends on the
  configuration. An index without languages keeps its schema and its configuration
  hash, so it is not rebuilt and no format change was needed. A change of languages
  changes the hash and rebuilds the index on open, as other configuration changes do.
- *Analyzers.* Tantivy's Snowball stemmers (the `stemmer` feature, `rust-stemmers`) and
  its stop word lists (the `stopwords` feature, which adds no crate) cover 18 languages.
  An analyzer lowercases, drops stop words and stems. Its stems are not folded to ASCII,
  as in Lucene. A first version folded them like the standard text, and the comparison
  below showed that folding merged words Lucene keeps apart, such as Swedish `städer`
  and `stad`. Removed stop words leave gaps in the positions, so phrases match as in
  Lucene.
- *Queries.* `lang:` and a tagged query string choose the field by the primary subtag,
  so `en-GB` literals are stemmed as English. Jena matches the tag exactly. The query
  parser of the Lucene syntax analyzes words with the field's analyzer and searches the
  field's terms. Prefixes, wildcards, fuzzy words, regular expressions and range bounds
  are lowercased but not stemmed, as Lucene's `QueryParser` normalizes multi-term
  queries. German ones also lose umlauts and ß, as Lucene's German normalization and
  the Snowball German stems do. Highlighting uses the same analyzer, so stemmed matches
  are marked.
- *Comparison with Jena.* `scripts/text-lucene-compare/compare.sh` loads 2,700 literals
  in nine languages (English, French, German, Spanish, Italian, Portuguese, Dutch,
  Russian and Swedish) into Sparkles and into Fuseki 5.1.0, whose jena-text has
  multilingual support. The literals mix inflected forms with stop words, and each word
  family is a set of forms that Lucene's analyzer or Snowball's stemmer reduces to one
  stem. Of 1,590 stemmed queries (words, conjunctions, stop words, prefixes, fuzzy
  words, wildcards and phrases), 1,302 match the same literals with the default
  Snowball analyzers. German, Dutch, Russian and Swedish agree on 636 of 671, English on
  172 of 191, and French, Spanish, Italian and Portuguese on 494 of 728, because Lucene
  uses Porter's stemmer for English and light stemmers for French, Spanish, Italian,
  Portuguese and German. With the analyzers that follow Lucene's, described below, 1,576
  queries agree.
- *Benchmark.* `scripts/bench-text.sh` indexes the English titles stemmed in both engines
  and has a seventh query, `"+theories +42"` with `lang:en`. On 210,509 triples Sparkles
  and Jena returned the same 10 literals, and every other query kept its answer.

**Rank output.** `text:query` has a sixth subject slot after Jena's five, the hit's rank
in the score order. It is one more than the number of hits with a higher score, so equal
scores share a rank. The rank exists for `spk:hybridSearch`
([F04](../F04-vector-search.md#outcome)), which fuses a text ranking with a vector ranking,
and is not produced when the query does not use it.

**CJK text** (2026-10-03). The `cjk` analyzer segments Chinese, Japanese and Korean
text without a dictionary, as Lucene's `CJKAnalyzer` does. Its tokenizer, in
`text/cjk.rs`, implements Tantivy's `Tokenizer` trait and adds no crate. A run of Han,
Hiragana, Katakana or Hangul characters becomes its overlapping bigrams, and a run of one
character a unigram. Other letters and digits form words, split as the standard
tokenizer splits them, and the Katakana middle dot separates words. Before that,
full-width ASCII is folded to ASCII and half-width Katakana to full-width, with a
following voiced or semi-voiced mark joined to its kana, as Lucene's `CJKWidthFilter`
does. The tokens are then lowercased. Unlike Lucene, the analyzer removes no stop words,
and `cjk-lucene`, described below, does.
The tags `zh`, `ja` and `ko` take it by default, `{"yue": "cjk"}` names it for another
tag, and `"all"` still means the stemmed languages, so an index configured with `"all"`
keeps its fields. A word in a query is the OR of its bigrams, as Lucene's query parser
reads a word the analyzer splits, and a phrase keeps them in order. `text_lang.rs`
searches Japanese, Chinese and Korean literals by words, phrases, folded widths and
half-width Katakana, and unit tests cover the bigrams, offsets and width folding.

**Not built.** Dictionary-based CJK segmentation and the other Phase 3 items that the
follow-ups below list.

**Phase 2 and 3 additions (2026-10-03).**
* *`text` on dataset creation.* `POST /$/datasets` takes `text`, `true` or a
  `TextConfig` in a JSON body and `true` or `false` in a form, as `geo` already was.
  A bad configuration is a `400` that leaves no dataset behind, and a build without
  the feature answers `501`. The UI's new dataset dialog has an "Enable full-text
  search" checkbox with an optional list of predicates, and the mock server accepts the
  option too.
* *Online rebuilds.* `POST /$/text/{ds}/rebuild` no longer holds the writer lock while
  the index is built. Under the lock, the rebuild starts a journal and takes a
  snapshot. It builds the new index from that snapshot with writes going on, while the
  current index keeps answering searches and every commit adds its quads to the
  journal. Under the lock again, it applies the journal to the new index, deleting each
  quad's document and adding it again when the head still has the quad, and swaps the
  index in. That commit has no merge policy, so writes do not wait for segment merges.
  The journal holds ids, so a compaction or bulk commit during the build, which
  renumbers them, makes the rebuild build again with writes waiting. A lock per index
  keeps two rebuilds apart, and enabling or disabling the index waits for a running
  rebuild. A bulk commit during a rebuild updates the current index by its changes, or
  leaves it behind until the rebuild ends. Reconfiguring an index still builds it with
  writes waiting.
* *Variable arguments.* The query string can be a variable that the rest of the group
  binds. The call is then planned like a vector search with a variable query: a node
  over the rest of its group that runs one search per distinct value (at most 1,000,
  else `507`) and joins each input row with the hits of its value. A value's language
  tag acts as `lang:`. A bound value that is not a string, or does not parse, matches
  nothing, as an unusable `spatial:` binding does, where the same constant is a `400`.
  The predicates, limit, `lang:` and highlighting must still be constants.
* *Bound-subject pushdown.* Without a limit and without a rank output, a search's hits
  outside the subjects of the join's other side are dropped by the join, so the search
  can leave them out. When a hash join's left side binds the call's subject in every
  row to at most 4,096 distinct values, the executor runs the search with a
  `TermSetQuery` of those subjects' keys as a filter that does not score. A call with a
  variable query string restricts each search to the subjects of its rows in the same
  way. The optimization is `text_subject_pushdown`.
* *Incremental bulk maintenance.* After a bulk commit, the index is brought from the
  state before it to the state after it by the documents that differ. The keys of the
  documents before are collected and sorted, the candidates after are compared with
  them, new documents are added and the keys left over are deleted, and the change is
  committed and synced at once, since the new generation's WAL does not hold it. When
  the commit adds more than one document per 8 that the index holds (at least 20,000),
  the single writer thread of this path would be slower than a rebuild with up to 8, so
  it gives up and the index is rebuilt.
* *Rebuilds over the literal range.* With every predicate indexed, a rebuild no longer
  reads every quad and looks up every object's key. It reads the OPS ranges of the base
  vocabulary's keys that start with `"` and of the delta vocabulary, and decodes each
  object once, since the quads come in object order. The new document order uncovered
  a bug in the sloppy phrase scorer, which could seek a word's postings backwards, and
  that is fixed.
* *Porter's stemmer.* The `porter` analyzer stems English as Lucene's
  `EnglishAnalyzer`, and so jena-text, does: the English stop words, then Porter's
  algorithm in the form of his reference implementations, with their two departures
  from the paper in step 2 (`bli` to `ble` and `logi` to `log`). It is written for
  Sparkles in `text/porter.rs` from the 1980 paper and adds no crate. No tag takes it by
  default, so an index selects it with `"languages": {"en": "porter"}`, and the English
  differences of the comparison above, such as `relativity`, which Porter stems to
  `rel` and Snowball to `relat`, go away for such an index. The tokens still come from
  the standard tokenizer, which splits a possessive `'s` off where Lucene removes it.
  `english-lucene`, described below, removes it.
* *Tests.* `crates/sparkles-core/tests/text.rs` covers a rebuild against the index that
  maintenance kept, bulk loads and replacements that update the index by their changes
  and survive a reopen, a rebuild running while writes go on (with a compaction during
  it in a second round), pushdown against the same queries without it, and query
  strings from `VALUES`, triple patterns and `BIND`. The server's router tests create
  datasets with `text` in JSON and form bodies, and a Playwright test enables full-text
  search from the new dataset dialog.
* *Performance.* The runs below were interleaved on a 16-core machine that other
  builds kept at a load average between 20 and 75. On 10.5M triples with every
  predicate indexed (1.54M documents), a rebuild took a median of 7.2 s of wall time
  and 24.2 s of CPU over 11 runs, against 12.6 s and 30.3 s when it read every quad. A
  bulk load of 315,519 triples into that store took a median of 23.6 s of CPU, against
  45.8 s with a rebuild after it. Its text step added 46,200 documents in 3.3 to 6.8 s,
  where the rebuild took 11.1 to 24.3 s. During each of 12 rebuilds started over HTTP
  on the same store, between 988 and 4,404 single-triple writes ran, with a median
  latency of 2 to 14 ms. The slowest write of a rebuild waited 0.4 to 6.5 s, for the
  last step, whose directory syncs are slow while other builds load the disk. With the
  old rebuild, one write waited for the whole rebuild, 11.6 to 50.5 s. The searches of
  `scripts/bench-text.sh` on 1.05M triples did not change with pushdown on or off, since
  none is a join of a search without a limit. Searching the names of one
  organization's employees for `ada`, with 2 hits, took a median of 3.9 ms against
  6.1 ms over 30 runs, and the titles of one organization's authors for `theory` 5.6 ms
  and 2.0 ms of CPU against 20.6 ms and 9.7 ms.

**Analyzers that follow Lucene's.** Ten analyzers reproduce the Lucene 9.11 analyzers
that jena-text's multilingual support picks for its languages. They are `english-lucene`,
`french-light`, `spanish-light`, `italian-light`, `portuguese-light`, `german-light`,
`dutch-lucene`, `russian-lucene`, `swedish-lucene` and `cjk-lucene`, selected per tag
in `languages` as `porter` is, for example `{"fr": "french-light"}`, or with
`sparkles text-index --language fr=french-light`. No tag takes them by default, so
existing indexes keep their configuration hash and their tokens.

- *Analysis.* The words come from `text/standard.rs`, which follows the JFlex grammar of
  Lucene's `StandardTokenizer` rule for rule over the character properties of Unicode
  12.1, the version that grammar is generated from. Letters around an apostrophe, period
  or colon stay one word (`aren't`, `l'homme`, `u.s.a`), and so do digits around a comma
  or period (`3,14`). An emoji sequence, a flag and a run of Thai, Lao, Khmer or Myanmar
  letters are words, and each Han ideograph and Hiragana character is a word of its own.
  A word longer than 255 UTF-16 code units is cut where the longest match inside those
  units ends, as Lucene cuts it, where the other analyzers drop tokens over 40 bytes. Lowercasing goes
  character by character, as Java's does, so `İ` becomes `i`. French and Italian remove
  elided articles (`l'`, `dell'`), and English removes a possessive `'s`. The stop words
  are Lucene's lists, which are the Snowball ones apart from Swedish `sitta`. The
  stemmers are Savoy's light stemmers for French, Spanish, Italian, Portuguese and
  German (`text/light.rs`, from "Report on CLEF-2001 experiments" and "Light stemming
  approaches for the French, Portuguese, German and Hungarian languages"), with German
  normalization of umlauts and `ae`, `oe` and `ue` before it. English uses Porter's
  stemmer, Dutch the Snowball stemmer behind Lucene's stem dictionary (`kind` is
  `kinder`, `ei` is `eier`), and Russian and Swedish the Snowball stemmers, with Russian
  `ё` read as `е`. `cjk-lucene` takes Lucene's typed words and pairs the characters of
  adjacent Han, Hiragana, Katakana and Hangul words, so a word that mixes Hangul with
  Latin letters stays whole, and then removes `CJKAnalyzer`'s stop words, which are
  English words and `s`, `t` and `www`. Prefixes, wildcards, fuzzy words and regular
  expressions are normalized as Lucene's analyzers normalize them, with elision for
  French and Italian and German normalization for German. No new crate is needed, as
  the Snowball stemmers are those of Tantivy's `rust-stemmers`.
- *Checks.* Lucene's own tokens of 15,917 lines are fixtures in
  `crates/sparkles-core/tests/data/lucene-analyzers`, made by
  `scripts/text-lucene-compare/tokens.sh` from words of the Snowball test vocabularies,
  of the OpenTaal Dutch word list and of Swedish inflections written for the tests, and
  from generated lines with punctuation, numbers and CJK text. Every analyzer produces
  the same tokens and positions as Lucene on every line. The tokenizer has a fixture of
  its own, 19,599 lines with Lucene's tokens, UTF-16 offsets and types, which covers
  every boundary of its character table in seven contexts. Over every code point in
  nine contexts, 10,008,558 lines, and 420,000 random lines of up to 600 characters, its
  tokens, offsets and types are those of Lucene. Over the full vocabularies and
  a Swedish dictionary, about 320,000 words, no stem differs.
- *Agreement.* With these analyzers selected, 1,576 of the 1,590 queries of the
  comparison above match the same literals as Fuseki. Each language agrees except on
  queries whose words are all stop words: `it`, `"as they"`, `seriez`, `"serai ces"`,
  `"quel aurait"`, `hayas`, `faremmo`, `"avessero contro"`, `delas`, `hin`,
  `"muss ihrem"`, `ben`, `говорил` and `hon`.
  jena-text then searches the language field alone and returns every literal of the
  language, where Sparkles answers `400` because no word is left to search for. This
  difference is kept on purpose.
- *Fuseki configurations.* `sparkles fuseki-config` maps `text:multilingualSupport` to
  the 20 languages that jena-text analyzes and Sparkles can, and a
  `text:LocalizedAnalyzer` to its language, each with the analyzer closest to Lucene's.
- *Decisions.* The plan named the light stemmers and `dutch-lucene`. German, Russian and
  Swedish have analyzers of their own as well, because Lucene's German analyzer uses
  the light German stemmer rather than Snowball's, which alone made 25 German queries
  differ, and because the Russian `ё` and the Swedish stop word would otherwise stay
  apart. The possessive filter and the CJK stop words went into the new
  `english-lucene` and `cjk-lucene` rather than into `porter` and `cjk`, since changing
  an analyzer under an existing index would leave its indexed tokens out of step with
  its queries without a change of its configuration hash.

**Dictionary analyzers.** `ja-dict`, `zh-dict` and `ko-dict` segment Japanese, Chinese
and Korean text into words with a dictionary. They are behind the cargo feature
`text-cjk-dict`, which the server's default build enables, and no tag takes them by
default, so `cjk` stays the analyzer of CJK text unless an index selects them with
`{"ja": "ja-dict"}` and the like.

- *Dictionaries.* None ships with Sparkles. `sparkles text-dict fetch` downloads a
  pinned source, checks its SHA-256, compiles it and installs it with its license in the
  dictionary directory, `DATA/text-dicts` for a server. `ja-dict` uses IPADIC
  2.7.0-20070801 with the `令和` entry that Lucene's build adds, `ko-dict` uses
  mecab-ko-dic 2.0.3-20170922, the version Nori is built from, and `zh-dict` uses jieba's
  word list. `sparkles text-dict install` compiles a local source and `list` checks the
  installed ones. A compiled dictionary is one file that starts with a magic number and a
  format version, which a reader refuses when it does not know it, and ends with the
  SHA-256 of what precedes it. They take 18 MiB for `ja-dict`, 58 MiB for `ko-dict` and
  12 MiB for `zh-dict`, and a process keeps each one it uses in memory once.
- *Configuration.* Enabling an index with a dictionary analyzer fails while its
  dictionary is missing. The configuration records each dictionary's SHA-256 in
  `dictionaries`, so the configuration hash covers the dictionary. When a store opens and
  an installed dictionary differs from the recorded one, the configuration takes the new
  hash and the index is rebuilt in the background, as for a damaged index. A missing
  dictionary at open leaves the index unavailable until it is installed.
- *Segmentation.* `text/dict/morph.rs` ports Lucene 9.11.1's `JapaneseTokenizer` in
  search mode and `KoreanTokenizer`, with the filters of `JapaneseAnalyzer` and
  `KoreanAnalyzer`. Both run the Viterbi search over the whole text and commit to the
  best path wherever one path is left. Unknown words are grouped by `char.def` and by
  Java's character properties of Unicode 15.0, which a generated table holds. Kuromoji
  scores the lattice under a long word again with its penalty, and its analyzer folds
  widths, replaces inflected words by their base form, drops the parts of speech of
  `stoptags.txt` and the words of `stopwords.txt`, removes the final `ー` of long Katakana
  words and lowercases. Nori charges for a word after a space that cannot start one,
  splits compounds and inflected words into their parts, drops its default stop tags and
  replaces Hanja by its Hangul reading. Removed words leave position gaps, as in Lucene.
  `zh-dict` cuts each run of CJK characters into the words whose summed costs, the
  negative logarithms of their frequencies, are lowest.
- *Checks.* Kuromoji and Nori produce the same tokens and positions as `ja-dict` and
  `ko-dict` on all 60,035 lines of the comparison corpora, which are sentences written
  for the tests, lines of words drawn at random from each dictionary, and generated lines
  that join such words with Latin, Kana, Hangul, Han, symbols and spaces. 10,038 of
  those lines are fixtures in `crates/sparkles-core/tests/data/lucene-dict-analyzers`,
  with 20 lines of runs longer than 1,024 UTF-16 units, where both tokenizers cap
  unknown words and commit to the cheapest path found so far. The fixtures are
  regenerated by `scripts/text-lucene-compare/tokens.sh`, and the ignored test
  `dictionary_analyzers_agree_with_lucene` checks them against installed dictionaries. The
  unit tests use small dictionaries written for them.
- *Decisions.* `zh-dict` segments by jieba's frequencies rather than reproducing
  SmartChineseAnalyzer. Fuseki bundles none of SmartChinese, Kuromoji and Nori, so no
  Fuseki configuration depends on their tokens, and SmartChinese's dictionaries are
  serialized Java objects inside its jar, segmented sentence by sentence with the JDK's
  `BreakIterator`. Item 250 records that work. `ko-dict` pins mecab-ko-dic 2.0.3 instead
  of the newer 2.1.1, because Nori's tokens come from 2.0.3. Java and the WHATWG EUC-JP
  decoder differ on seven JIS X 0208 characters, such as `〜` and `—`, and the compiler
  maps those back to Java's characters so that IPADIC's entries match Kuromoji's. A
  changed dictionary is adopted at open by rebuilding the index rather than by refusing
  to open it, since the change is the operator's and the RDF data stays usable during
  the rebuild.

### Roadmap follow-ups (2026-10-12)

**Full-text parity** (item 23, Tier 3, [ROADMAP](../../ROADMAP.md)). Each part below is
sized at about one to two weeks.

* **Lucene analyzer parity** is built, as *Analyzers that follow Lucene's* above
  describes.
* **CJK dictionary segmentation** is built, as *Dictionary analyzers* above describes.
  Chinese segmentation after SmartChineseAnalyzer, Kuromoji's normal and extended modes
  and user dictionaries for Kuromoji and Nori remain, as item 250.
* **Analyzers for the other languages of jena-text,** such as Greek, Arabic, Persian and
  Hindi, which use Snowball stemmers or none today. Item 251 records them.
* **Online enable and reconfigure.** `Store::enable_text` in
  `crates/sparkles-core/src/store.rs` takes the writer lock (`guarded_writer`) for the
  whole build, and *Online rebuilds* above records that reconfiguring still builds
  with writes waiting. Enabling and reconfiguring reuse the online rebuild: write
  `text.json` as pending, start the journal under the lock, build into `text.new`
  from a snapshot with writes going on, apply the journal under the lock and swap.
  Until the swap, a reconfigured index keeps serving searches with its old
  configuration, and a newly enabled one answers `503` with `text-unavailable`, the
  error a search gets today while startup recovery rebuilds the index. `GET /$/text/{ds}` shows the pending
  configuration beside the active one. Acceptance: enabling the index on 10.5M triples
  while single-triple writes run keeps their median latency within twice the latency
  without the build, as the rebuild measurement above does.
* **Search after uncommitted writes.** A `text:query` in a write transaction after the
  transaction changed data fails with `uncommitted_changes()` (`text.rs`, checked in
  `text/search.rs` through `view.uncommitted`). The transaction's own inserts and
  deletes are in its delta. A search in that state runs against the committed index,
  drops hits whose quads the transaction deleted, and adds hits from an in-memory
  Tantivy index (`RamDirectory`) of the transaction's inserted literals, built on the
  first search and extended on later ones. Scores of the two indexes are not
  comparable as BM25 statistics differ, so the merged hits are re-scored with the
  committed index's document frequencies, which the searcher exposes. A transaction
  whose inserted text passes a budget (by default 10,000 literals) keeps the current
  error. Acceptance: in one transaction, insert `ex:a rdfs:label "quokka"`, then
  `text:query "quokka"` returns `ex:a`; delete an existing indexed literal, and it
  is no longer returned.
* **Historical full-text.** A `text:query` at `?at=` answers `501`
  ([F06 §11.6](../F06-snapshots-and-point-in-time.md)). Two steps: first, a search at a
  past commit in the current generation runs against the head index and corrects it by
  the commits between, as the transaction case above does with the reversed changes
  from the change log (bounded by the same budget). Second, a pin can be created with
  `text: true`, which builds and keeps a Tantivy index for the pinned state in the
  pin's directory, charged to the pin's bytes in retention. Other past states keep
  answering `501`.

**Multi-field entity documents** (item 35, Tier 4, [ROADMAP](../../ROADMAP.md)). Documents
are per quad (`text.rs`: the key is a hash of the s, p, o and g keys), so a query cannot
require words from two properties of one entity, and BM25 scores one literal at a time.
jena-text's entity map and QLever's text records both group text by entity. An index
configuration gains `entities`: a list of fields, each a name, its predicates and a
boost, as in `{"fields": [{"name": "name", "predicates": ["rdfs:label"], "boost": 2},
{"name": "body", "predicates": ["schema:description"]}]}`. Each (subject, graph) with a
literal under a field's predicates gets one entity document with a field per name,
next to the per-quad documents, so existing queries are unchanged. A new property
function, `text:entity`, takes Lucene syntax with field names (`name:quokka AND
body:marsupial`) and binds the subject, the score and the rank. Maintenance re-indexes
an entity document when any of its quads changes: the commit path collects the touched
subjects of configured predicates and rebuilds their documents from the snapshot, which
costs a lookup per touched subject and field. Bulk maintenance and rebuilds read the
fields' predicates in PSO order, so an entity's literals come together. Acceptance: with
fields `name` and `body`, an entity whose label has `quokka` and description
`marsupial` is found by `name:quokka AND body:marsupial`; an entity with both words in
its label only is not.
