# Natural-language questions results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Natural-language questions

These numbers measure `sparkles ask` and global answering from community reports with
Anthropic models. They depend on the models and prompts more than on the engine.
`testsuite/ask/README.md` and `testsuite/graphrag/README.md` explain the harnesses, which
call the provider through a local proxy that holds every request against a cost cap.

| Measurement context | |
|---|---|
| Date and host | Ask measured on 2026-10-31 and 2026-11-01 on a development laptop that was running other work, and global answering on 2026-10-24, both with local data |
| Models | Ask uses Claude Haiku 5.5, Sonnet 5.5 and Opus 5.5 at effort `medium`, with a 200,000-token context. Global answering uses Sonnet 5.5 with Opus 5.5 as judge. |
| Builds | Ask from commit 7b3b6df5, global answering from commit 847d8d0b |
| Cost | $39.22 for the Ask baseline under a cap of $45. Global answering cost $11.25. |

### Ask on DBpedia

The questions run on the DBpedia 2022.12.01 database of 1.01 billion quads, against
answers recomputed on that release. When the pipeline asks which resource a name means,
the harness answers from the gold query by choosing one of the offered choices. An answer
is correct when its rows match the gold's on the projected columns, which is the
`rdf-term-set-projected-columns-v2` policy and the headline score. A label or count
beside the answer column does not count against an answer. The equal-width score is a
stricter secondary column that gives no credit to any extra column.

QALD-9-plus keeps 107 of its 150 test questions on this release. Those questions were used
while Ask grounding was tuned, so they are regression evidence. The Text2SPARQL'25 DBpedia
set keeps 74 of its 100 questions on this release. It was not used in that tuning, and its
gold answers were recomputed with the measured binary and matched the earlier recompute
exactly. Every gold query passed before any provider request.

| Set and model | Runs | Correct with projected columns | Correct with equal width | Clarifications answered | Cost per run |
|---|---|---|---|---|---|
| QALD-9-plus, Haiku 5.5 | 3 | 28, 29 and 27, a mean of 28.0 (26%) | 25, 25 and 23 | 84 | $0.31 |
| QALD-9-plus, Sonnet 5.5 | 2 | 32 and 34, a mean of 33.0 (31%) | 14 and 11 | 85 | $5.47 |
| QALD-9-plus, Opus 5.5 | 1 | 41 (38%) | 13 | 84 | $11.60 |
| Text2SPARQL'25 db25, Haiku 5.5 | 1 | 19 (26%) | 15 | 58 | $0.22 |
| Text2SPARQL'25 db25, Sonnet 5.5 | 1 | 32 (43%) | 15 | 58 | $3.35 |
| Text2SPARQL'25 db25, Opus 5.5 | 1 | 33 (45%) | 13 | 58 | $6.86 |

The Opus QALD answers come from an attempt that a host failure interrupted after 22
questions and from two later blocks with the same settings. Its cost includes $3.40 for
the interrupted attempts, and two of its answers are failures in which the generated
query ran the Ask process out of memory. Opus ran db25 in three blocks so that each block
passed the harness's cost projection. The campaign spent $39.22, which includes $5.34
for a Sonnet attempt that the same host failure interrupted. The machine was running
other work, so the record keeps wall times only as context and they are not latency
measurements.

Sonnet and Opus name each answer beside its IRI far more often than Haiku, so the equal
width column puts Haiku first on QALD, the reverse of the projected score. Opus leads
Sonnet by 8 points on QALD, which is at the edge of the noise floor below, and by one
question on db25. Its gain comes mainly where the needed predicate is missing from the
prompt. It answers 19 of 66 such QALD questions, where Sonnet answers 12 of 66 per run
and Haiku 6 of 66, and 12 of 34 such db25 questions, where Sonnet answers 9 and Haiku none.
When the prompt holds every gold resource and schema term, the three models answer a
similar share of about two thirds. Opus also refuses less, with 9 refusals against 15
for Sonnet and 43 for Haiku per run.

#### Run-to-run noise

Repeating a model on the same questions changes 4 to 10 answers out of 107. Haiku's
three runs differ by 4, 7 and 9 questions and Sonnet's two runs by 10. A Sonnet attempt
interrupted after 96 questions scored 34 of them, against 29 and 30 for the two full runs
on the same 96 questions. The 95% half-width of the paired difference between two single
runs is 3.7 to 5.8 points between repeats of one model and 6.8 to 8.9 points between
models on QALD, and 10.9 points between Haiku and Sonnet on db25. A variant therefore
needs a gain of about 6 points, or 6 to 7 questions, on QALD in one paired run, or two
runs for each variant to resolve a gain of about 4 points.

#### Why answers fail

A script classifies every wrong answer from its saved prompts, queries and results, and
question-level checks of the gold mark the broken gold answers. The table counts wrong
answers per run.

| Cause | Haiku QALD | Sonnet QALD | Opus QALD | Haiku db25 | Sonnet db25 | Opus db25 |
|---|---|---|---|---|---|---|
| A needed predicate or class is missing from the prompt | 46.0 | 25.0 | 17 | 27 | 18 | 9 |
| Wrong or unlinked resource | 16.0 | 25.0 | 15 | 17 | 9 | 9 |
| Clarification policy | 0 | 1.0 | 1 | 0 | 0 | 0 |
| Wrong query shape, or a refusal with the needed terms in context | 9.3 | 16.0 | 24 | 7 | 11 | 20 |
| A right answer that the scorer marks wrong | 0.3 | 0 | 1 | 3 | 3 | 2 |
| Gold answer wrong or broken on this release | 7.0 | 7.0 | 6 | 1 | 1 | 1 |
| Provider error, truncation or an out-of-memory failure | 0.3 | 0 | 2 | 0 | 0 | 0 |

Missing grounding is the largest cause for Haiku, ties with wrong resources for Sonnet on
QALD and leads for Sonnet on db25. For Opus, wrong query shape is the largest cause on
both sets. One example is "What is the highest mountain in Germany?", whose prompt lists
dozens of predicates that contain "highest" or "mountain" but not `dbo:elevation`, so Haiku and Sonnet answer that the data cannot
rank mountains. Entity linking is the second cause. The five candidates for "Mars",
"Texas" and "Scream" do not include `dbr:Mars`, `dbr:Texas` or `dbr:The_Scream`, and
"President Obama" offers only redirect pages. Query shape grows with model strength
because the stronger models answer instead of refusing. Their typical miss returns every
gold row plus extra rows, by uniting `dbo:` and `dbp:` variants of a predicate, which
happens in 21 wrong Opus answers and 10 to 12 wrong Sonnet answers on QALD.

#### Scorer and gold checks

Checks of 22 of the lowest-scoring questions found nine broken gold answers and four kinds
of scorer error. Three QALD gold queries cast an `xsd:gYear` to `xsd:date`, which SPARQL
1.1 refuses, so their gold is one row with no binding. One projects a variable that is
never bound, and one more, in db25, uses `SELECT *` and so demands the book beside each
author. One gold answer is the infobox text "SeeList of nicknames for San Francisco", and
three include an empty string beside the real answers. The four that have no binding or
an unbound column cannot be matched. The other five can be matched only by returning the
same defective data, which happened for one of them in every QALD run and for two in the
Opus run. Without these nine questions, the projected scores are 27, 28 and 26 of 99 for
Haiku, 31 and 33 for Sonnet and 39 for Opus on QALD, and 19, 32 and 33 of 73 on db25.

The scorer marks a right answer wrong when a count has another integer datatype, when it
names labels for gold IRIs, when the gold mixes a literal with IRIs, and when the gold is
one comma-separated literal that names the answer IRIs. Counting these as correct adds
0 to 3 answers per run. The record keeps this lenient count beside the strict one, and
it keeps answers that hold every gold row plus extra rows as a separate diagnostic that
never passes.

The projected policy can be gamed. It chooses the best answer column for each gold column
and compares the projected rows as a set, so a synthetic answer passes when it puts
competing guesses in separate columns, adds up to eight columns or repeats each answer
on many rows. No measured answer used `SELECT *`. Of the 121 answers that pass only by
projection, 85 add one column, 28 add two and 8 add three. Their extra columns hold
labels 65 times, numbers or dates such as the ranking key 34 times, related resources
42 times and predicates 19 times, the last from queries that try several predicates
in one `VALUES` list. Fifteen of them repeat an answer on several rows. A guard that caps
extra columns at three and reports separately any pass whose extra column holds
predicates or whose projection drops rows would close the many-column and repeated-row
cases and keep every label and ranking pass. It would flag 26 current passes, from none to
7 per run, with the most for Opus and Sonnet. It does not catch two competing guesses on
one row, so the guard also needs to report a pass whose matched column is not the first
column when another extra column holds values of the same kind as the gold column.

#### Memory

Generated queries ran the `sparkles ask` process out of memory three times. A
zero-or-more property path over redirects and disambiguations reached about 28 GB twice,
and `skos:broader?/skos:broader?` from an unbound category reached a 12 GB cap. Ask's
default query memory budget of 2048 MiB and 60-second timeout did not stop them. Ask
needs an enforced default memory budget and timeout that cover property-path evaluation,
which [ROADMAP](../../ROADMAP.md) item NEW-ITEM-1 tracks.

[ROADMAP](../../ROADMAP.md) item 263 holds the accuracy work. The
[paid record](../2026-11-01-ask-model-baseline-paid.json) gives per-run and per-case
results, failure causes, costs, source digests and the gold overrides.
[C18](../../specs/C18-natural-language-questions-and-ingest/OUTCOME.md#ask-model-baseline-item-263)
gives the proposed improvements and the split of the sets for tuning.

Current Ask grounding keeps half its schema budget for common vocabulary and fills the
rest with question matches from the full visible schema. In an offline check against
181 saved questions and an independently retrieved vocabulary of 58,564 predicates, this
retrieves 189 of 304 existing predicate IRI constants mentioned in the gold, compared with
151 from the count-only list. The check uses declared English or untagged property
labels and includes `a` as `rdf:type`. It does not measure variable predicates, model
answer accuracy or latency. The paid baseline above measures the complete pipeline
with this retrieval and conservative resource and category ranking. The
[offline record](../2026-10-25-ask-grounding-offline.json) gives the source
digests, per-question retrieval counts and the exact link to the old prompt's vocabulary.
The resource preference only orders offered choices. An ambiguous mention still asks for
a clarification, since the displayed candidate, type and alternate-label lists can be
truncated. The paid run does not isolate this ranking's contribution to answer accuracy.

### Global answering

Sonnet 5.5 answered 30 generated questions about the whole demo organisation graph four
ways, and Opus 5.5 judged each pair of answers in both orders on the criteria of Edge et
al. The table gives the win rate of global answering from the finest level, which holds
27 reports, against each other mode, with a tie counted as half.

| Against | Comprehensiveness | Diversity | Empowerment | Directness |
|---|---|---|---|---|
| Local Ask | 0.88 | 0.97 | 0.92 | 0.68 |
| BM25 over 20 documents | 1.00 | 1.00 | 0.98 | 1.00 |
| Global from the coarsest level | 0.62 | 0.57 | 0.55 | 0.79 |

A global answer took a median of 10.4 s against 4.7 s for local Ask. Every cited fact
exists, and the judge found 44% of the claims of fine answers fully supported and 48%
partly supported.
[C22](../../specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24)
gives the intervals and the other measures.

### VALUES tables in the memory reader's unions

The memory tools and Ask read the default graph and the named graphs through one UNION
and pass the terms they look up as VALUES tables. This A/B measured roadmap item 90, which
moves such tables into `GRAPH` patterns and through FILTERs on the tables' variables, on
10.1 million generated quads. Two million entities in the default graph and 300,000 in
each of ten named graphs have an English label and a type, and an inferred graph holds
100,000 more types. Each query looks up ten labels or ten entities.

| Measurement context | |
|---|---|
| Date and host | 2026-10-30 on atlas, an Intel Core i5-13500, with nothing else running |
| Builds | Commit 34b69139 against commit 4e21d168, both release builds in `nix develop` with rustc 1.99.1, Clang 23.1 and GCC 15.3 |
| Method | 12 fresh server processes in three rounds of A, B, B, A, each with 2 untimed and 10 timed requests per query, timed by the server's `execMs` with the result cache off |

| Query | Before (ms) | After (ms) | Answers |
|---|---|---|---|
| The reader's union with its graphs and labels in tables | 150 | 0.16 | 11 rows, the same |
| A branch FILTER on the entity over a two-pattern join | 417 | 0.22 | 42 rows, the same |
| A table and a UNION under one named GRAPH | 42 | 0.08 | 4 rows, the same |
| A table outside a `GRAPH ?g` that holds a UNION | 522 | 0.41 | 64 rows, the same |
| The reader's union with its labels in a table, which the earlier rewrite moved | 0.15 | 0.15 | 42 rows, the same |

Each round gave the same ratio within 0.0002 for the first four queries. The last query
takes under 0.3 ms, and its rounds ranged from 0.95 to 1.28 of the earlier build, which
is within the noise of a query that short.
