# GraphRAG evaluation

`scripts/eval-graphrag` runs the acceptance examples of
[C22](../../docs/specs/C22-graph-rag-community-summaries.md) that need a real model, and the
evaluation of its §9, against Anthropic models under a local cost cap. It uses the loopback
proxy and the append-only ledger of `scripts/eval-ask-anthropic`, which
[the Ask evaluation](../ask/README.md#the-cost-cap) describes, so only the proxy reads the
key and every request is reserved against the cap before it leaves.

## Phases

The `acceptance` phase builds reports for two clusters of ten people joined by one edge and
checks A1, A3, A4 and A8 on them. A1 checks that every cited fact lies inside its
community, which means its subject is a member and so is its object unless that is a
literal. A3 adds one fact and checks that a refresh keeps both communities and writes
nothing. A4 adds seven more and checks that the refresh rewrites exactly one report, which
revises the old one. A manual refresh compares digests when it runs, so the result records
whether `status` showed the report stale beforehand rather than requiring it. A8 sets `tokensPerBuild` just above one report's reservation and checks
that the build stops with a community pending. On the demo graph it checks A6, a global
question classified `global` whose answer carries point markers, each resolving to a point
whose findings cite facts or child findings, and A7, which answers in local mode without
reviewed reports and from reviewed reports alone once three are marked reviewed.

The `generate` phase describes the demo graph from its classes, properties and labels and
asks the generator model for five users, three tasks for each and two global questions for
each task, as Edge et al. do. The questions are checked and kept in `questions.json`.

The `evaluate` phase answers every question of `questions.json` four ways. Ask answers in
local mode. A text baseline retrieves the eight best paragraphs of the documents of
`testsuite/ingest/sample.json` with BM25 and has the answering model answer from them.
Global answering runs on a build with `levels` 0, which keeps only the coarsest level, and
on a build with `levels` 3, where Ask reads the finest level with at most 100 reports. The
report lists the levels each mode answered from. The judge model
compares every pair of answers in both orders on comprehensiveness, diversity, empowerment
and directness, without citation markers, and the report gives each pair's win rate with a
95% bootstrap interval over questions and the share of judgements that agree when the order
is swapped. The phase also measures the classification of §7.1 on the global questions and
a fixed sample of answerable demo questions in `auto` mode, where an Ask that never chose a
scope, because it stopped at a mention clarification or an error, counts as unclassified. It checks the citations of every global answer. Each
sentence takes the markers that follow it, the findings of its points are resolved through
child findings to the facts they cite, every cited fact is looked up in the projection,
and the judge rates each claim against that evidence, with a missing fact marked as not in
the data. A claim is valid when the judge finds it supported and every fact behind it
exists. A reply that does not give exactly
one verdict per claim is asked once more and otherwise counted as incomplete. The phase
also records latency and build tokens, and it refuses a questions file without a global
question before any paid request. The demo graph is
`testsuite/ask/org.ttl` with the gold facts of the labelled sample folded in, as if the
documents had been ingested without error.

The summary also measures the byte bound of §3.5. For every community request it compares
the input tokens the provider reported with the request's serialized size plus the default
allowances.

## Running

```
python3 -I -B -m unittest discover -s testsuite/graphrag -p 'test_*.py'
SPARKLES_BINARY=/ABSOLUTE/sparkles python3 -I -B -m unittest discover -s testsuite/graphrag
scripts/eval-graphrag --run RUN.json --dry-run
fnox exec -- scripts/eval-graphrag --run RUN.json --phase acceptance
```

The run file holds the binary and its SHA256, the ledger, the output directory, the cap and
the carryover, the answering, judge and generator models, the effort, the output limit, the
context window, the Ask deadline, the questions file, the number of local questions, the
phases, and prior costs for a local Ask, a global Ask, a direct model call and a build
request. The judge must be a different model from the answering one. Before each block the
harness projects its cost from the priors and the request counts of the builds' own
estimates, times `projectionMargin`, and stops when that passes what the cap leaves. The
evaluation admits both of its builds together with its answers and judgements. `--dry-run` answers
every request with an instance of its JSON schema from a local fake and reads no key.
