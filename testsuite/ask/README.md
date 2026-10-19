# Ask evaluation

`scripts/eval-ask` runs `sparkles ask` on a question set and scores each answer against
gold results. The demo set `demo.json` holds 68 questions over `org.ttl`. There are 59
answerable questions, four clarification questions and five unanswerable questions. The
set does not stand in for QALD or Text2SPARQL'25. Three converters build public sets in
the same format, and `--loc DIR` runs a set against a database.

## Public sets

`scripts/qald9plus-convert` converts the 150 questions of the QALD-9-plus DBpedia test
split. `scripts/text2sparql-convert --set db25` converts the 100 DBpedia questions of
Text2SPARQL'25, and `--set ck25` the 50 questions over CK25, the challenge's corporate
graph. `scripts/lcquad2-convert` converts the 6,046 questions of the LC-QuAD 2.0 test
split over Wikidata, and `--sample N` keeps a fixed sample of them. Each converter pins
its source files by commit and SHA256 and fetches them, or reads local copies with
`--input`. None of the data is kept in the repository.

Given `--sparkles` and a database with `--loc`, a converter runs every gold query on that
database and drops the questions whose gold returns nothing, fails or cannot be scored.
The set counts them by reason. The set also compares each recomputed answer with the
answer the source publishes, by lexical value, and lists the questions that differ.
`--manifest` copies a JSON record of how the database was loaded into the set. The CK25
set recomputes on its own graph, which the converter writes beside the set file and the
set loads with full-text search, as the demo set does. QALD-9-plus and Text2SPARQL'25 ran
on older DBpedia releases, and LC-QuAD 2.0 needs a Wikidata database that holds
statements and qualifiers, which the truthy dump leaves out. The scorer compares rows as
sets, so a question that the source marks as order-sensitive is scored without its order,
and its score carries `orderNotScored`.

The DBpedia converters also make Virtuoso-dialect gold standard SPARQL. They declare the
prefixes that DBpedia's endpoint predeclares, such as `dbo:` and `xsd:`, and put
parentheses and a variable around a bare projection such as `SELECT DISTINCT COUNT(?x)`.
The rewrites never look inside a literal, an IRI or a comment, and the new variable is
one the query does not use. The set keeps the source query as `sourceGold` and lists the
rewrites. The DBpedia sets cannot run yet. On the full DBpedia database every `sparkles
ask` takes about 100 seconds and most checks fail on the 2 GiB query memory budget. The
likely cause is the schema report of the whole dataset that `check_query` builds in each
process, and roadmap item 89 addresses it.

## What is scored

Every pair of a provider and a model is one whole pipeline, which supplies the draft,
repair and summary roles together. The report has one row per pair and, for two or more
pairs, a cascade row that it computes from the pair runs. A question moves to the next
pair when the server itself sees a failed Ask, never by comparing with gold, and the row
charges the cost of every pair it tried. These rows do not measure repair or summary
accuracy on their own.

A SELECT answer is a set of rows compared under one column mapping, with exact terms.
Datatypes, language tags and lexical forms are kept, and numbers are never rounded. An
answer with the gold's width is compared whole. An answer with more columns, such as a
name beside each person, is projected onto one distinct answer column for each gold
column, and the projected set of rows is compared with the gold, so a stray or missing
row still costs precision or recall. An answer with fewer columns than the gold is
wrong. Blank nodes, more than eight columns and more than 1000 rows are unsupported.
Gold that needs them fails before any model is called. The scorer refuses SERVICE before
it starts a query. This is the policy `rdf-term-set-projected-columns-v2`, a strict
RDF-term policy and not an official QALD score. The first policy compared whole rows
only, and it scored most answers of Sonnet 5.5 and Opus 5.5 in the first paid run as
wrong because they named each answer beside its IRI.

`scripts/eval-ask-rescore --report OUT/SET/report.json --sparkles BIN --out DIR` scores
the saved answers of a run again under the current policy. It runs only `sparkles query`
on the set's data, refuses a set or binary that differs from the run's, and writes a
report with each row's execution accuracy before and after.

Answerable accuracy, clarification precision and refusal accuracy have separate
denominators. Only the `unanswerable` outcome is a successful refusal. A provider error,
a refusal by the model, an Ask timeout or an unusable draft is a failed answer with the
status `provider_error`, `refusal`, `timeout`, `budget_exceeded` or `invalid_output`,
and the run goes on. A case that the cost cap refused is `budget_refused`. It leaves the
denominators, marks its row incomplete and ends the run. A harness fault stops a run,
such as failing gold, a wrong binary hash or a child with an unknown exit status.

Each child has capped output, its own process group and cleanup. A matrix has no
wall-clock deadline unless `--phase-deadline` sets one. Ask gets 300 seconds, a query
5 seconds and an Ask token budget of 32768 unless `--deadline`, `--timeout` and
`--max-tokens` change them. Reports go to a new directory outside the repository. A run
names its binary by absolute path and SHA256.

## Checking without a provider

```
python3 -I -B -m unittest discover -s testsuite/ask -p 'test_eval*.py'
python3 -I -S -B scripts/eval-ask --self-test --sparkles /ABSOLUTE/sparkles
scripts/eval-ask-anthropic --run RUN.json --dry-run
```

The self-test drives the real CLI through the proxy against a local fake API. It covers
chunked and close-delimited replies, structured-output retries, a downgrade, a
rate limit, an overload that clears and a cascade. `--dry-run` runs a whole run file
against the fake, reads no key and reports request sizes. Neither contacts a provider.

## The cost cap

`scripts/eval-ask-anthropic` starts a proxy on a loopback port and gives the children a
random token in place of the key. Only the proxy reads `ANTHROPIC_API_KEY`. It sends
each request once to `api.anthropic.com` with the standard library, so Content-Length,
chunked and close-delimited replies all work. A reply that comes back gzip or deflate
encoded, although the request asks for none, is decoded. It sets `stream` to false and
the effort from the run file, and it refuses tools, cache control and any model outside
the run. The key is stripped once when it is read, and a key with whitespace or control
characters inside is refused. An error inside the request keeps only its class, so the
key cannot reach a message. The key does stay in the proxy's initial process
environment, which another process of the same user can read, so the machine should be
one only the operator uses.

A JSON-lines ledger at the run file's `ledger` path holds the cap. Before each request the
proxy writes a reserve line with an upper bound from the request size and `max_tokens`,
and the reply writes a settle line with the cost from its usage. A request is refused
when the spent total plus its bound would pass `localCapNanodollars`. The spent total
includes the carryover, so the cap covers the carryover as well, and new requests can
spend the cap minus the carryover. The run file is also refused when the cap plus the
carryover passes USD 25, which counts the carryover twice and keeps the run on the safe
side. An error reply is free, a lost reply costs the bound, a reply without usage costs
the bound, and any other fault of the request costs the bound. Replaying the ledger
counts every unsettled reserve at its bound, and the pilot's unresolved $1.002560 is the
carryover line. Reopening a ledger resumes it. Nothing resets it. The ledger's first line
holds its format version, 2, and a reader refuses any other version.

Replay is strict. It refuses a cost or bound that is not a nonnegative integer, a request
ID that is not a positive integer, a settle without its reserve, a repeated reserve, a
carryover anywhere but right after the header, and a reconcile that breaks any rule
below or takes the ledger past USD 25. A refused ledger stops the run. A ledger from an
older format, an empty file and a ledger cut off between its header and its carryover
are refused the same way, and need a new ledger directory or an edit by hand.

A charge that stands at an upper bound can be reconciled with the amount the provider
billed. Such charges are the carryover, a reserve that a crash left without a settle
line, and a request settled at its bound because its reply was lost or its usage could
not be read. `--held` lists them with each request's model, case, time and request ID.
`--reconcile TARGET --billed-nanodollars N --evidence TEXT` appends a reconcile line
that replaces one of them, where TARGET is `carryover` or a request ID. The evidence is
10 to 1000 printable characters that name the source of the amount, such as the
Console's cost for one day of an API key that only the run used. The held charge is an
upper bound, so an amount above it is refused unless `--allow-raise` is given, and the
line then records the raise. The command prints the previous and the new amount. Each
charge is reconciled once, and replay checks every reconcile line against the charge it
replaced. Neither command reads a key or sends a request, and both create the ledger
with its carryover when it does not exist yet. Both take the ledger's lock, so they
cannot run while a paid run holds it.

```
scripts/eval-ask-anthropic --run RUN.json --held
scripts/eval-ask-anthropic --run RUN.json --reconcile carryover \
  --billed-nanodollars 1600000 --evidence "Console cost of key K on 2026-10-15"
```

The local ledger is the authority. A Console workspace spend limit of $24 is a backstop
against a defect, and it can lag billing, so the run does not rely on it.

Before spending, the run counts tokens, which is free, and sends each model of the run
one structured-output request of at most 256 tokens. It records the status and headers
of the replies in `preflight.json`, without rate-limit, request, organization, workspace
and cookie headers. It stops if any step fails. A block of a set and a model runs only
when the projected cost fits the cap. The projection is the case count times the p90
measured case cost of that model, or a prior from the run file, times a margin.

The proxy appends every forwarded request and its reply, with the key redacted, to
`replies.jsonl` in the output directory, so an answer can be diagnosed later without
calling a model again. The first paid run predates this file, so its empty drafts
cannot be traced to the reply that produced them.

## A paid run

`run.example.json` shows the run file. Copy it and set the paths. It holds the sets and
the models of each set, the effort, `maxOutputTokens`, the request and Ask timeouts, the
query timeout, the cap, the carryover, the ledger, the output directory and the binary
with its SHA256. The carryover is required, and the cap plus the carryover cannot pass
USD 25. A real run refuses a second `.jsonl` file in the ledger directory, so the ledger
file stays the account of record for the whole spend.

`askTokenBudget` is the total token limit of one Ask. The default of 32768 ends most
repaired or DBpedia-grounded Asks after one or two calls, so the example sets 100000. The
budget is checked before each model call, so a case can pass it by one call. An Ask that
spends it is a `budget_exceeded` failure and not an API error. `queryTimeoutSecs` must be
at least the timeout that a converted set's gold answers were made with. Children get ten
seconds beyond the Ask deadline and the query timeout.

The preflight sends the run's own structured-output request to every model, and a run
also stops as incomplete after five provider errors in a row. The projection of a block
borrows the measured cost of the same model from earlier sets, so the first block of a new
set uses demo costs until it has its own.

A binary built from a named commit outside `docs/plans` is the one to use. After the dry
run passes, one command starts the run.

```
fnox exec -- scripts/eval-ask-anthropic --run /ABSOLUTE/PATH/TO/run.json
```

A crash such as SIGKILL leaves `ledger.jsonl.lock` holding the owner's PID. The next start
replaces it when that process is gone and refuses while it runs. A restart needs a new
`out` directory because the old one stays as the record. The ledger resumes and counts
every unsettled reserve at its bound. After the first real preflight, copy
`anthropic-reply-headers.json` from the output directory to
`testsuite/ask/fixtures/`, which enables the framing test on a real reply.

The pilot of 2026-10-15 ran 17 of the 68 demo questions on three pairs and a real cascade,
which made 68 planned executions. It stopped on its first request and scored nothing. Its
wallet, grant claim and report stay as they were. Its gate accepted only a reply with a
single Content-Length, and it stopped with `upstream_framing` 7.3 seconds after the
first Haiku request left, so the model had run and the reply most likely came back in
chunks. The reserve was never settled and stayed at the gate's bound of $1.002560, a
million input tokens at a price above Haiku 5.5's. The request itself held a grounding
of at most 8,192 tokens and at most 1,024 output tokens, which costs under a cent. The
current proxy settles such a reply at its exact cost, and a test replays one.
