# Natural-language questions results

This is a [manual report](../README.md#evidence-and-generated-reports). Read the
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
three include an empty string beside the real answers. No correctly shaped answer matches
the four that have no binding or an unbound column, although a query that fails the same
way would. The other five can be matched only by returning the same defective data, which happened for one of them in every QALD run and for two in the
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
default query memory budget of 2048 MiB and 60-second timeout did not stop them, because
eager property path evaluation neither charged its closure state nor checked the
deadline. Property path closures now run within both limits
([X08](../../specs/X08-disk-spill-and-server-memory.md#property-path-closures-2026-10-31)),
and the `why_empty` checks of the three queries finish in under 8 seconds on the same
data. The runs recorded here predate that change.

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

#### Ask grounding from predicate use

Ask grounding lists the predicates that the question's linked entities and best-matching
classes use in the data, links with popularity ordering and tells the model that the
context is partial and to use one predicate per relation.
[C18](../../specs/C18-natural-language-questions-and-ingest/OUTCOME.md#ask-grounding-from-predicate-use-item-263)
describes the changes. The table compares it with the baseline on the same questions and
settings. The QALD rows split into the 72 training questions that tuned the changes and
the 35 validation questions. The fresh set is 150 questions from the QALD-9-plus
training split that no tuning step read, chosen by the SHA-256 of a fixed seed and the
question id after six duplicates of QALD test or db25 questions were dropped.

| Measurement context | |
|---|---|
| Date and host | 2026-11-01 on a development laptop that was running other work, with binaries built on atlas |
| Builds | Commit 570d7062 for the changes, and 30b0a968 for two Haiku QALD runs, which builds the same prompts on this database. The Haiku baselines and the fresh baselines use commit 2d43aa5a, and the Sonnet and Opus QALD and db25 baselines are the 7b3b6df5 runs above. |
| Settings | Effort `medium`, a 200,000-token context and answered clarifications, as above |
| Cost | $26.74 in 3,696 requests under a cap of $50 |

| Set and model | Baseline | With the changes |
|---|---|---|
| QALD-9-plus training, Haiku 5.5 | 18 and 15 [16 and 14 of 68] | 29, 30 and 28 [29, 29 and 30] |
| QALD-9-plus training, Sonnet 5.5 | 23 and 23 [21 and 19 of 68] | 34 [34] |
| QALD-9-plus training, Opus 5.5 | 26 [23 of 68] | 39 [39] |
| QALD-9-plus validation, Haiku 5.5 | 8 and 7 [7 and 7 of 34] | 14, 12 and 13 [14, 12 and 13] |
| QALD-9-plus validation, Sonnet 5.5 | 9 and 11 [7 and 7 of 34] | 14 [13] |
| QALD-9-plus validation, Opus 5.5 | 15 [13 of 34] | 18 [18] |
| QALD-9-plus, all 107, Haiku 5.5 | 26 and 22 [23 and 21 of 102] | 43, 42 and 41 [43, 41 and 43] |
| QALD-9-plus, all 107, Sonnet 5.5 | 32 and 34 [28 and 26 of 102] | 48 [47] |
| QALD-9-plus, all 107, Opus 5.5 | 41 [36 of 102] | 57 [57] |
| db25, Haiku 5.5 | 21 [23 of 73] | 31 [33] |
| db25, Sonnet 5.5 | 32 [31 of 73] | 41 [42] |
| db25, Opus 5.5 | 33 [30 of 73] | 42 [43] |
| Fresh, Haiku 5.5 | 37 [37 of 150] | 77 [75] |
| Fresh, Sonnet 5.5 | 64 [59 of 150] | 86 [82] |
| Cost per run, Haiku, Sonnet and Opus | | $0.15 to $0.28, $2.13 to $4.28 and $4.28 to $6.60 |

Each cell gives the strict projected score of record. The secondary score in brackets is
`rdf-term-set-projected-guarded-lenient-v3` over the questions its gold rules keep, with
the passes that rely on a predicate column or on dropped rows left out. The paired gain
on the fresh set is 26.7 points with a 95% half-width of 7.6 for Haiku, which gained 42
questions and lost 2, and 14.7 points with a half-width of 7.5 for Sonnet, which gained
29 and lost 7. On db25 it is 13.5 points for Haiku against the same starting commit and
12.2 points for Sonnet and for Opus, each with a half-width of about 10, and on QALD it
is 13 to 20 points for every model.

The gain is largest on the fresh set, so it does not come from tuning to the QALD and
db25 questions. Predicate-use grounding alone took Haiku from 24.0 to 35.0 on
QALD over two runs each, and the prompt rules added about 7 more, from 35.0 to 42.5. In
dry runs with the fake provider, the share of training prompts that hold every gold term
rose from 20 to 39 of 72 and of validation prompts from 8 to 14 of 35. The prompt rules
cut Haiku's refusals on QALD from about 48 to 22 per run and its UNION or `VALUES`
queries from about 12 to 2. Sonnet's UNION or `VALUES` queries fall from about 40 to 3,
so its equal-width score rises from about 12 to 29. A first-draft prompt grows by about
8%, from about 29,800 to 32,400 characters, and each Ask runs two to three more bounded
queries per linked entity and one per matched class. A fresh-set run costs 5 to 8% more,
and the median Ask time stays within about a second of the baseline on this loaded host.
With the changes, a gold resource missing from the prompt causes about half of the wrong
answers and wrong query shape most of the rest.

Haiku at commit 2d43aa5a scored 26 and 22 on QALD, against a mean of 28.0 at 7b3b6df5,
and 21 on db25 against 19. The difference is within the noise of so few runs, and it may
come from the grounding reordered for prompt caching. [ROADMAP](../../ROADMAP.md) item
594 tracks it. A provider outage stopped the first Haiku fresh baseline after 42
of 150 questions, and the record keeps that attempt and its cost beside the full rerun.
The [paid record](../2026-11-01-ask-hillclimb-paid.json) gives per-run and per-case
results under both scores, every case the secondary score changes, costs, token counts
and source digests.

#### Ask repair options

Four repair turns of Ask were measured as separate options on Haiku 5.5. The exploration
drafts once more after a draft gives no query or an empty result, with further link
candidates and the predicates the entities use. The shape check repairs once when the
result does not fit the question's form. Multi-draft voting asks three first drafts and
keeps the one whose checked, complete result most of them share. Type hints link each
mention with the classes the words around it name. Each option ran three times on an
enlarged validation set of 51 questions, the 35 validation questions of the QALD split
and the 16 training questions that no tuning step had inspected case by case. Every
other training question had been read during tuning, so the set could not reach the
intended 105. An option is on by default only when its mean gain passes the spread of
the three baseline runs.

| Measurement context | |
|---|---|
| Date and host | 2026-11-01 on a development laptop that was running other work, with binaries built on atlas |
| Build | Commit `dfc799dc`, one batch child per run with six questions at once |
| Settings | Effort `medium`, a 200,000-token context and answered clarifications |
| Questions | 51, with the moved IDs and the rule that chose them in the [paid record](../2026-11-01-ask-hillclimb-paid.json) |

| Option | Strict score in three runs | Mean | Cost per question | Default |
|---|---|---|---|---|
| None | 25, 26 and 24 | 25.0 | $0.0019 | |
| Exploration | 29, 28 and 27 | 28.0 | $0.0022 | On |
| Shape check | 25, 25 and 25 | 25.0 | $0.0020 | Off |
| Three drafts with a vote | 26, 25 and 27 | 26.0 | $0.0031 | Off |
| Type hints | 23, 21 and 24 | 22.7 | $0.0021 | Off |
| Exploration, shape check and three drafts | 29, 26 and 28 | 27.7 | $0.0035 | |

The guarded secondary score equals the strict score in every run. The exploration gains
3.0 questions, and its lowest run is above the highest baseline run, at about 16% more
cost per question. It turns refusals into answers, which fall from 24 to 8 over the
three runs. The shape check fires on at most 3 of the 51 questions in a run, so it
cannot move the score much on this set, and it may help on data whose questions more
often ask for counts or dates. Voting gains 1.0 question, inside the baseline's spread
of 2, for 63% more cost per question. It may help a weaker model or a dataset whose
drafts disagree more often. Type hints lose 2.3 questions, past the spread. More of
their queries add a class constraint, and their empty results double from 11 to 22 over
the three runs. They may help on data whose labels are shared by many entities of
different classes. The exploration with the shape check and voting scores no higher than
the exploration alone and costs 59% more per question. Every option stays available
through `assistant.json`, the ask request and the command line.

#### Ask across providers

Six models answered the same three sets with the baseline build and the final build:
Claude Haiku 5.5, Sonnet 5.5 and Opus 5.5, and OpenAI GPT-6 Luna, GPT-6.1 Terra and
GPT-6.1 Sol. The baseline is commit `2d43aa5a`, before the grounding changes above, and
the final build is commit `dfc799dc` with the exploration on and the other repair
options off. The OpenAI builds add the OpenAI request support and the follow-up form of
repairs, which repeats the first draft's message so that OpenAI's prompt cache serves
the repair, and they make no other change. Unseen is 150 QALD-9-plus training questions
that no tuning step read, disjoint from the fresh set, and it is the primary held-out
measure. QALD and db25 shaped the changes, so they are checks.

| Measurement context | |
|---|---|
| Date and host | 2026-11-01 and 2026-11-02 on a development laptop that was running other work, with binaries built on atlas |
| Builds | Anthropic `2d43aa5a` and `dfc799dc`. OpenAI `dd0d5430` and `3a138901`, which are the same commits with the OpenAI support and the follow-up repair form. |
| Settings | Effort `medium`, a 200,000-token context, answered clarifications and one question per Ask child for the baselines, with six questions at once for the final builds |
| Cost | $51.72 for the Anthropic runs and $33.95 for the OpenAI runs in the tables |
| Not run | Opus on unseen with the baseline build, deferred for budget |

The strict projected score is the headline. Guarded is the secondary
`rdf-term-set-projected-guarded-lenient-v3` score over the questions its gold rules
keep. Opus answered unseen in three blocks of 50 questions, because the harness admits a
block only when the 90th-percentile case cost of the model times the block's questions
fits what the cap leaves. The blocks use the same build, settings and questions, and
their reports combine into one.

QALD-9-plus test, 107 questions

| Model | Build | Strict | Guarded | Correct after clarification | Macro-F1 | Cost per question | Cost per correct answer |
|---|---|---|---|---|---|---|---|
| Haiku 5.5 | Baseline | 26 (24%) | 26 of 102 | 19 | 0.28 | $0.0017 | $0.0072 |
| Haiku 5.5 | Final | 46 (43%) | 48 of 102 | 38 | 0.47 | $0.0023 | $0.0054 |
| Sonnet 5.5 | Baseline | 35 (33%) | 34 of 102 | 26 | 0.40 | $0.0274 | $0.0838 |
| Sonnet 5.5 | Final | 55 (51%) | 56 of 102 | 46 | 0.59 | $0.0296 | $0.0576 |
| Opus 5.5 | Baseline | 39 (36%) | 41 of 102 | 30 | 0.51 | $0.0619 | $0.1700 |
| Opus 5.5 | Final | 60 (56%) | 62 of 102 | 48 | 0.66 | $0.0660 | $0.1177 |
| GPT-6 Luna | Baseline | 28 (26%) | 27 of 102 | 22 | 0.27 | $0.0018 | $0.0069 |
| GPT-6 Luna | Final | 47 (44%) | 49 of 102 | 37 | 0.50 | $0.0023 | $0.0051 |
| GPT-6.1 Terra | Baseline | 30 (28%) | 29 of 102 | 24 | 0.36 | $0.0168 | $0.0599 |
| GPT-6.1 Terra | Final | 52 (49%) | 54 of 102 | 43 | 0.58 | $0.0183 | $0.0376 |
| GPT-6.1 Sol | Baseline | 33 (31%) | 30 of 102 | 27 | 0.35 | $0.0312 | $0.1013 |
| GPT-6.1 Sol | Final | 52 (49%) | 53 of 102 | 44 | 0.58 | $0.0345 | $0.0711 |

Text2SPARQL'25 db25, 74 questions

| Model | Build | Strict | Guarded | Correct after clarification | Macro-F1 | Cost per question | Cost per correct answer |
|---|---|---|---|---|---|---|---|
| Haiku 5.5 | Baseline | 21 (28%) | 23 of 73 | 16 | 0.34 | $0.0017 | $0.0061 |
| Haiku 5.5 | Final | 37 (50%) | 39 of 73 | 28 | 0.58 | $0.0020 | $0.0039 |
| Sonnet 5.5 | Baseline | 32 (43%) | 33 of 73 | 24 | 0.55 | $0.0281 | $0.0649 |
| Sonnet 5.5 | Final | 45 (61%) | 47 of 73 | 33 | 0.70 | $0.0287 | $0.0473 |
| Opus 5.5 | Baseline | 34 (46%) | 36 of 73 | 25 | 0.59 | $0.0579 | $0.1260 |
| Opus 5.5 | Final | 46 (62%) | 48 of 73 | 35 | 0.70 | $0.0613 | $0.0986 |
| GPT-6 Luna | Baseline | 22 (30%) | 23 of 73 | 16 | 0.34 | $0.0019 | $0.0062 |
| GPT-6 Luna | Final | 40 (54%) | 42 of 73 | 29 | 0.62 | $0.0021 | $0.0038 |
| GPT-6.1 Terra | Baseline | 25 (34%) | 27 of 73 | 20 | 0.40 | $0.0163 | $0.0483 |
| GPT-6.1 Terra | Final | 43 (58%) | 45 of 73 | 32 | 0.67 | $0.0171 | $0.0295 |
| GPT-6.1 Sol | Baseline | 28 (38%) | 30 of 73 | 22 | 0.41 | $0.0302 | $0.0798 |
| GPT-6.1 Sol | Final | 42 (57%) | 44 of 73 | 31 | 0.66 | $0.0325 | $0.0572 |

Unseen, 150 QALD-9-plus training questions

| Model | Build | Strict | Guarded | Correct after clarification | Macro-F1 | Cost per question | Cost per correct answer |
|---|---|---|---|---|---|---|---|
| Haiku 5.5 | Baseline | 41 (27%) | 42 of 150 | 35 | 0.30 | $0.0017 | $0.0063 |
| Haiku 5.5 | Final | 95 (63%) | 97 of 150 | 79 | 0.69 | $0.0020 | $0.0032 |
| Sonnet 5.5 | Baseline | 72 (48%) | 73 of 150 | 64 | 0.55 | $0.0264 | $0.0550 |
| Sonnet 5.5 | Final | 106 (71%) | 106 of 150 | 92 | 0.76 | $0.0288 | $0.0407 |
| Opus 5.5 | Baseline | Not run, deferred for budget | | | | | |
| Opus 5.5 | Final | 111 (74%) | 112 of 150 | 93 | 0.78 | $0.0624 | $0.0843 |
| GPT-6 Luna | Baseline | 41 (27%) | 41 of 150 | 36 | 0.30 | $0.0019 | $0.0069 |
| GPT-6 Luna | Final | 106 (71%) | 107 of 150 | 88 | 0.76 | $0.0021 | $0.0029 |
| GPT-6.1 Terra | Baseline | 42 (28%) | 43 of 150 | 34 | 0.34 | $0.0169 | $0.0604 |
| GPT-6.1 Terra | Final | 108 (72%) | 109 of 150 | 92 | 0.77 | $0.0170 | $0.0237 |
| GPT-6.1 Sol | Baseline | 44 (29%) | 44 of 150 | 36 | 0.32 | $0.0313 | $0.1068 |
| GPT-6.1 Sol | Final | 113 (75%) | 114 of 150 | 96 | 0.79 | $0.0329 | $0.0437 |

The final build gains 12 to 69 questions over the baseline for every model and set.
Luna rises from 41 to 106 on unseen and Haiku from 41 to 95. The OpenAI baselines score
lower than the Claude baselines of a similar price, and the final build closes that gap.
With the follow-up form, an OpenAI repair reads the first draft's prompt from the cache,
and a final run costs 1 to 28% more per question than its baseline.

##### The default model

The default is the cheapest model per correct answer on unseen whose score is within
the noise of the best model's. Within the noise means that the paired bootstrap 95%
interval of the difference between the best model and the candidate, over the 150
questions with 10,000 resamples, contains zero. Sol scores best with 113, and Opus scores
111. Luna's interval against Sol runs from 0.7 points below to 10.0 points above, and
against Opus from 2.7 points below to 9.3 points above, so Luna passes. Its cost per
correct answer is $0.0029, the lowest of the six. Haiku's interval against Sol runs from
5.3 to 19.3 points, so Haiku fails. GPT-6 Luna is therefore the default model of the
`openai` preset. Within the Claude models, the same rule picks Sonnet 5.5, which is
within the noise of Opus while Haiku is not, so the `anthropic` preset keeps Sonnet.

The tradeoff is plain on unseen. Sol answers 7 more of 150 than Luna at 15.7 times the
cost per question, Opus 5 more at 29.7 times, Terra 2 more at 8.1 times, and Sonnet the
same number at 13.7 times. The check sets favour the larger models more. On QALD Luna
answers 47 of 107 against Opus's 60, and on db25 40 of 74 against Opus's 46, and both
gaps are outside the noise, so a dataset like the tuning sets gains more from a larger
model. The [paid record](../2026-11-02-ask-fair-matrix-paid.json) holds every run, the
per-case results, the bootstrap intervals and the ledgers.

#### Ask on the cheapest models

A second round aimed at the cheapest tier, GPT-6 Luna and Claude Haiku 5.5 at about
$0.002 per question. Its bar was that the better of the two answers at least 105 of the
150 unseen questions strictly, and that its paired bootstrap 95% interval against Sol, the
strongest model of the provider comparison, contains zero. Every change was generic to
any dataset and was judged on both models by the mean of three runs on the 51 validation
questions. The two robustness fixes were kept unless they clearly hurt a model, and the
other changes were kept only when they gained.

| Measurement context | |
|---|---|
| Date and host | 2026-11-02 on a development laptop that was running other work, with binaries built on atlas |
| Builds | `27459d58` as the base, the same commit with each change applied for the trials, and `ace00e4b` for the finals |
| Settings | Effort `medium`, a 200,000-token context and answered clarifications, as in the provider comparison. The finals and the later trials asked eight questions at once in each Ask child, and the earlier trials one. |
| Data | The DBpedia 2022.12.01 database of the provider comparison. The finals of the two providers ran at once, the Claude models on a hardlinked copy of the database that changes no file. |
| Cost | $9.23 for the Claude finals, $7.73 for the OpenAI finals and $4.48 for the trials |

The changes are these, in the order they were added.

- The draft check's lookup of whether a class has a predicate now writes the two terms
  into its ASK. Passed as bindings, the same lookup exceeded the 2 GiB query memory
  estimate on DBpedia for a class such as `dbo:River`, and the check then failed the
  draft. A lookup that still exceeds a memory or row budget is now skipped.
- A draft that uses a well-known prefix such as `dbo:` or `geo:` without declaring it
  gets the declaration added, unless the dataset or the query binds that prefix itself.
- The class profile that the check suggests ranks a class's predicates by how many
  sampled instances use them and how rare they are in the whole store, with the draft's
  own namespace first.
- The draft prompt asks for only the variables that answer the question, with no label,
  predicate or helper column unless the question asks for one.

Strict answers on the 51 validation questions, three runs per arm unless shown

| Arm | Haiku 5.5 | Mean | GPT-6 Luna | Mean |
|---|---|---|---|---|
| Base | 28, 27, 23 | 26.0 | 29, 29, 28 | 28.7 |
| Check lookup fix, two runs | 27, 26 | 26.5 | 31, 31 | 31.0 |
| Plus prefix completion and the ranked profile | 26, 26, 29 | 27.0 | 27, 32, 28 | 29.0 |
| Plus the projection rule, the final build | 29, 27, 30 | 28.7 | 30, 29, 30 | 29.7 |

The mechanism counts carry more signal than the scores, whose spread across three runs
of one arm reaches 5 questions. Across three base runs, 7 Haiku drafts and 10 Luna drafts
failed the check on the memory estimate, and none did after the lookup fix. Drafts that
failed to parse fell from 9 and 6 to none once prefix completion was added. Answers with
columns the gold lacks fell from 27 to 8 for Haiku and from 59 to 10 for Luna with the
projection rule. The cost per question stayed between $0.0020 and $0.0023, apart from a
few Luna runs that cost less than half as much because they started while OpenAI's
prompt cache still held the prompts of the run before.

Unseen, 150 QALD-9-plus training questions, final build

| Model | Strict | Without the 7 overlapping questions | Before this round | Cost | Cost per correct answer |
|---|---|---|---|---|---|
| GPT-6 Luna | 101 (67%) | 96 of 143 | 106 | $0.29 | $0.0028 |
| Claude Haiku 5.5 | 102 (68%) | 97 of 143 | 95 | $0.29 | $0.0029 |
| GPT-6.1 Terra | 111 (74%) | 106 of 143 | 108 | $2.51 | $0.0227 |
| GPT-6.1 Sol | 109 (73%) | 104 of 143 | 113 | $4.93 | $0.0452 |
| Claude Opus 5.5 | 114 (76%) | 110 of 143 | 111 | $8.93 | $0.0784 |

The overlapping questions are the 7 unseen questions whose wording shares at least half
its tokens with a synthetic training question, so the synthetic sets could have taught
them. The column before this round gives the final build of the provider comparison.

The bar is not met. Haiku is the better cheap model with 102, three short of 105. Its
interval against Sol runs from 2.0 points below to 11.3 points above, and Luna's from 0.7
below to 11.3 above, so both are within the noise of Sol. Against Opus with the same
build, Haiku's interval runs from 2.0 to 14.0 points and Luna's from 2.0 to 15.3 points,
so neither is. Against the Opus run of the provider comparison, Haiku's interval runs from
0.7 points below to 12.7 above and Luna's from 0.0 to 13.3, which both contain zero.
Without the overlapping questions the conclusions are the same, except that Luna's
interval against that earlier Opus run, from 0.7 to 14.7 points, no longer contains zero.

The changes did not move the held-out scores beyond the noise. Haiku gained 7 questions,
Terra and Opus 3 each, Sol lost 4 and Luna lost 5, a net gain of 4 over the five models,
and each model's paired interval against its earlier run contains zero. The robustness
fixes stay because they remove failures of the check and the parser. Luna stays the
model of the `openai` preset, since one run per model moved none of them beyond the noise
of the provider comparison on which the preset was chosen. When accuracy matters more
than cost, choose Terra, which answers 74% of the held-out questions strictly, within the
noise of Opus, at about 9 times Luna's cost.

##### Effort for the cheapest models

Haiku and Luna then ran at effort `high` with the final build, three times each on the 51
validation questions, against the three `medium` runs of the final build above. Haiku
ran with 16,384 output tokens and a 180-second request timeout, as Luna does, so that
neither a token limit nor a timeout could cut a longer answer short, and no reply in any
run stopped on its token limit.

| Model | Medium | Mean | High | Mean | Cost per run at high |
|---|---|---|---|---|---|
| Claude Haiku 5.5 | 29, 27, 30 | 28.7 | 26, 27, 29 | 27.3 | $0.122 to $0.124, against $0.105 to $0.109 |
| GPT-6 Luna | 30, 29, 30 | 29.7 | 31, 28, 30 | 29.7 | $0.116 with a cold cache, against $0.101 to $0.104 |

High effort gains nothing on either model. Over the three runs of each arm Haiku writes
42% more output tokens and Luna 64% more reasoning tokens, and the runs take 23% and 56%
longer. An arm was to be confirmed on the held-out set only if its mean rose beyond the
spread of the medium runs, so neither ran there, and the held-out comparison above
stands. There both cheap models are within the noise of Sol and neither is within the
noise of Opus with the same build. At high effort the 150 held-out questions would cost
about $0.36 for either model, against $2.51 for Terra, so cost is not what rules high
effort out. Effort stays `medium` for the cheapest models. The [effort
record](../2026-11-03-ask-effort-paid.json) holds the runs.

##### Synthetic questions

The synthetic DBpedia sets in `testsuite/ask/synth` have gold queries that were executed
when they were generated, but they are far easier than QALD. Haiku answers 109 and 110 of
the 134 synthetic validation questions, about 82%, against 30 and 26 of the 51 QALD
validation questions, about 55%, with the same build. Luna answers 118, about 88%,
against 27, about 53%. The synthetic sets therefore only rank changes coarsely, and item
595 tracks harder ones.

##### Experiments not kept

A library of 99 stored queries, written from the templates of the synthetic training
split and kept in a copy of the database, gave the draft model worked examples of the
data's shapes. Its superlative template was removed after the first runs. Haiku rose
from 110 and 109 to 121 and 119 of the 134 synthetic validation questions but fell to 25
of the 51 QALD questions, against 30 and 26 without it. Luna answered 29 of 51 against 27
and 119 synthetic questions against 118, inside its spread. The library teaches the shape
of the synthetic questions rather than the data. Its stored query for the languages of a
country reads `dbo:language`, which reaches 1 of the 11 languages that the QALD gold
lists for its question, and drafts copied that shape. The library was not kept. The
[paid record](../2026-11-02-ask-cheap-tier-paid.json) holds every run of this round, the
per-case results, the mechanism counts, the intervals and the ledgers.

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
