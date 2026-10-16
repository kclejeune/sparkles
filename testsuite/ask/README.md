# Ask evaluation

`scripts/eval-ask` runs `sparkles ask` on a question set and scores each answer against
gold results. The demo set `demo.json` holds 68 questions over `org.ttl`. There are 59
answerable questions, four clarification questions and five unanswerable questions. The
set does not stand in for QALD or Text2SPARQL'25. `scripts/qald9plus-convert` builds a
QALD-9-plus set in the same format, and `--loc DIR` runs a set against a database.

## What is scored

Every pair of a provider and a model is one whole pipeline, which supplies the draft,
repair and summary roles together. The report has one row per pair and, for two or more
pairs, a cascade row that it computes from the pair runs. A question moves to the next
pair when the server itself sees a failed Ask, never by comparing with gold, and the row
charges the cost of every pair it tried. These rows do not measure repair or summary
accuracy on their own.

A SELECT answer is a set of whole rows compared under one column mapping, with exact
terms. Datatypes, language tags and lexical forms are kept, and numbers are never
rounded. Blank nodes, more than eight columns and more than 1000 rows are unsupported.
Gold that needs them fails before any model is called. The scorer refuses SERVICE before
it starts a query. This is a strict RDF-term policy and not an official QALD score.

Answerable accuracy, clarification precision and refusal accuracy have separate
denominators. Only the `unanswerable` outcome is a successful refusal. A provider error,
a refusal by the model, an Ask timeout or an unusable draft is a failed answer with the
status `provider_error`, `refusal`, `timeout`, `budget_exceeded` or `invalid_output`, and the run goes on.
A case that the cost cap refused is `budget_refused`. It leaves the denominators, marks
its row incomplete and ends the run. A harness fault stops a run, such as failing gold,
a wrong binary hash or a child with an unknown exit status.

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
chunked and close-delimited replies all work. It sets `stream` to false and the effort
from the run file, and it refuses tools, cache control and any model outside the run.

A JSON-lines ledger at the run file's `ledger` path holds the cap. Before each request the
proxy writes a reserve line with an upper bound from the request size and `max_tokens`,
and the reply writes a settle line with the cost from its usage. A request is refused
when the spent total plus its bound would pass `localCapNanodollars`. An error reply is
free, a lost reply costs the bound and a reply without usage costs the bound. Replaying
the ledger counts every unsettled reserve at its bound, and the pilot's unresolved
$1.002560 is the carryover line. Reopening a ledger resumes it. Nothing resets it.

The local ledger is the authority. A Console workspace spend limit of $24 is a backstop
against a defect, and it can lag billing, so the run does not rely on it.

Before spending, the run counts tokens, which is free, and sends one 16-token Haiku
request. It records the status and headers of both replies in `preflight.json`. It stops
if either step fails. A block of a set and a model runs only when the projected cost fits
the cap. The projection is the case count times the p90 measured case cost of that model,
or a prior from the run file, times a margin.

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
wallet, grant claim and report stay as they were.
