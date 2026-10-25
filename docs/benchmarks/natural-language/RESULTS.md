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
| Date and host | Ask measured on 2026-10-25 and global answering on 2026-10-24, on a development laptop with local data |
| Models | Ask uses Claude Haiku 5.5 and Sonnet 5.5 at effort `medium`, with a 200,000-token context. Global answering uses Sonnet 5.5 with Opus 5.5 as judge. |
| Builds | Ask from commit 0d4fdd8e, global answering from commit 847d8d0b |
| Cost | $5.7450 for the full Ask run and $0.0397 for its separate pilot. Global answering cost $11.25. |

### Ask on DBpedia

The questions run on the DBpedia 2022.12.01 database of 1.01 billion quads, against
answers recomputed on that release. When the pipeline asks which resource a name means,
the harness answers from the gold query by choosing one of the offered choices. An answer
is correct when its rows match the gold's on the projected columns, and the equal-width
row gives no credit for extra columns such as a label beside an IRI.

| Measurement over 107 QALD-9-plus questions | Haiku 5.5 | Sonnet 5.5 |
|---|---|---|
| Correct with projected columns | 26 (24%) | 31 (29%) |
| Correct with equal width | 23 (21%) | 9 (8%) |
| Clarifications answered by the harness | 84 | 84 |
| Fresh cost including provider preflight | $0.2876 | $5.4574 |
| Cost per correct projected answer | $0.0111 | $0.1760 |
| Median wall time including clarification | 14.78 s | 14.98 s |
| p95 wall time including clarification | 29.22 s | 33.88 s |

These are repeated regression questions used during diagnosis, not an unseen test of
generalization. The ten-question Haiku pilot is separate from both 107-question
denominators. Every gold query passed before any provider request, and all 107 gold
relations matched the retained reference results. The
[paid record](../2026-10-25-ask-grounding-paid.json) gives both policies,
paired changes, fresh ledger costs, tokens, timings and artifact digests.

The result is mixed across models. Haiku answers more of the retained questions, while
Sonnet answers fewer under both policies. Both have higher median wall time, with nearly
unchanged p95. Provider variability and a GCC version difference prevent attributing
these changes to grounding alone. [C18](../../specs/C18-natural-language-questions-and-ingest.md#ask-grounding-accuracy-item-263)
records the comparison. [ROADMAP](../../ROADMAP.md) item 263 keeps the remaining accuracy work,
and item 318 tracks the median latency increase.

Current Ask grounding keeps half its schema budget for common vocabulary and fills the
rest with question matches from the full visible schema. In an offline check against
181 saved questions and an independently retrieved vocabulary of 58,564 predicates, this
retrieves 189 of 304 existing predicate IRI constants mentioned in the gold, compared with
151 from the count-only list. The check uses declared English or untagged property
labels and includes `a` as `rdf:type`. It does not measure variable predicates, model
answer accuracy or latency. The separate paid run above measures the complete pipeline
with this retrieval and conservative resource/category ranking. The
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
