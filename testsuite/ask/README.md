# Ask evaluation

`scripts/eval-ask` runs `sparkles ask` on a question set and scores each answer against
gold results. The demo set `demo.json` holds 68 questions over `org.ttl`. There are 59
answerable questions, four clarification questions and five unanswerable questions. The
set does not stand in for QALD or Text2SPARQL'25. Three converters build public sets in
the same format, and `--loc DIR` runs a set against a database.

## Public sets

`scripts/qald9plus-convert` converts the 150 questions of the QALD-9-plus DBpedia test
split, and `--split train` converts the 408 questions of its training split, whose IDs
start with `qald9p-train-`. `scripts/text2sparql-convert --set db25` converts the 100
DBpedia questions of Text2SPARQL'25, and `--set ck25` the 50 questions over CK25, the
challenge's corporate graph. `scripts/lcquad2-convert` converts the 6,046 questions of
the LC-QuAD 2.0 test split over Wikidata, and `--sample N` keeps a fixed sample of them.
Each converter pins its source files by commit and SHA256 and fetches them, or reads
local copies with `--input`. None of the data is kept in the repository.

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
rewrites. The DBpedia sets run with `--loc` against a database of the release their gold
answers were recomputed on.

## Synthetic DBpedia sets

`synth/v3` holds 461 synthetic questions over DBpedia 2022.12.01, in a train split of
186, a validation split of 134 and a test split of 141. `synth/synth.py` writes them
from six templates over well-known instances of eleven DBpedia classes: the value of a
relation, the instances that share a value, their count, a yes or no question, the
instance with the largest value of a number, and two hops through a value. Each relation
has its phrasings written by hand in `synth/phrases.py`. A subject whose page title needs
a parenthesized qualifier is left out, since the question would name it ambiguously. A
list or count is kept only when instances of the class reach the value through the
relation and at most half again as many reach it through any other property. The SHA-256
of each question's ID chooses its split, so the splits are fixed before any run. The
generator executes every gold query and keeps a question only when the answer is bounded
and not empty. `synth/check.py` runs every gold again and writes `check.json`, and none
of the committed questions returns nothing. No question comes from a QALD-9-plus or
Text2SPARQL'25 question, and with `--exclude` no resource that a gold query of those
sets names becomes a question's subject.

The synthetic questions are easier than the public sets. On the validation split Haiku
5.5 answers about 82% and GPT-6 Luna about 88%, against 45% to 63% on 51 QALD-9-plus
validation questions, so a gain on them is a coarse guide. A gain must also hold on the
public sets. The generator needs a DBpedia database:

```
python3 testsuite/ask/synth/synth.py --bin target/release/sparkles --loc DBPEDIA_DB \
  --exclude QALD_TEST.json --exclude QALD_TRAIN.json --exclude DB25.json testsuite/ask/synth/v3 4
python3 testsuite/ask/synth/check.py --bin target/release/sparkles --loc DBPEDIA_DB testsuite/ask/synth/v3
```

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
RDF-term policy and not an official QALD score. The first policy,
`rdf-term-set-global-columns-v1`, compared whole rows only, and it scored most answers of
Sonnet 5.5 and Opus 5.5 in the first paid run as wrong because they named each answer
beside its IRI. The projected policy was chosen after those results were seen, and it
favors an answer with more columns, since each extra column is another chance to match.
Each report row therefore also gives `executionAccuracyEqualWidth` and
`correctByProjection`, and `--scoring-policy rdf-term-set-global-columns-v1` scores under
the first policy.

Every case is also scored under a guarded secondary policy,
`rdf-term-set-projected-guarded-lenient-v3`, which `eval_support/guarded.py` defines.
The strict policy above stays the score of record, and the report gives both side by
side. The guarded score repairs evaluation defects that checks of the paid baseline
demonstrated, and it closes the ways the projected policy can be gamed.

- The gold errata `gold-errata-v1` take a question out of the guarded denominator when
  its gold uses `SELECT *`, when a gold column is unbound in every row, as after a cast
  that fails, or when `gold-errata-v1.json` names the question with the SHA256 of its
  gold text and a reason.
- A row that holds an empty string is dropped from the gold and from the answer.
- Two literals with XSD numeric datatypes match when their values are equal.
- A literal in the answer matches a gold IRI when its text is an `rdfs:label` or
  `skos:prefLabel` of that IRI and of no other gold IRI. A literal in a gold column that
  also holds IRIs matches an answer IRI the same way.
- An answer with more than three columns beyond the gold's is wrong.
- A pass whose extra column holds only predicates of the data, or whose projection drops
  answer rows, is flagged, and `executionAccuracyGuardedUnflagged` leaves it out. A pass
  whose matched column is not the first while another extra column holds terms of the
  same kind is flagged as a rival column and only counted, since a related resource
  beside the answer raises that flag as well.

Each case keeps its guarded detail under `guarded`, and each row gives
`executionAccuracyGuarded`, `executionAccuracyGuardedUnflagged`, the excluded questions,
the flagged and lenient passes and the count of each flag.

`scripts/eval-ask-rescore --report OUT/SET/report.json --sparkles BIN --out DIR` scores
the saved answers of a run again under the current policy, or the first one with
`--scoring-policy`. It runs only `sparkles query` on the set's data. It refuses a set, a
data file or a binary whose SHA256 differs from the run's, and a case file whose
question differs from its report row. A run on a database directory is rescored with
`--loc` at the path the run used. A directory has no digest, so the caller vouches that
nothing wrote to it since the run. The new report records the query timeout, each
case file's SHA256 and each row's execution accuracy before and after.

Every query the evaluator runs starts with PREFIX lines for the well-known prefixes of
C20 §3.1, such as `rdfs:`, `skos:` and `dcterms:`, followed by the set's own prefixes,
which replace a well-known one of the same name. Ask runs its drafts with the well-known
prefixes and `sparkles query --loc` does not, so before 2026-10-24 a correct answer that
wrote `rdfs:label` without declaring it failed when it was scored. The DBpedia sets
declare no prefixes of their own, and the first paid run of them was scored again with
these lines.

When an answerable question ends at a clarification, the evaluator answers it as a person
who means the gold query would, and scores a second Ask that carries the answer. The
answer is always the value of one of the choices the pipeline offered, so nothing from the
gold that the pipeline did not already propose reaches it. A choice whose value is an IRI
that the gold query names wins. Otherwise the choice whose label and value share the most
words with the gold's IRIs and literals wins, a tie goes to the earliest tied choice, and
with no shared word the first choice is taken. The saved answer keeps the clarification,
the choice, its basis and the first Ask's execution and usage, and each row counts
`clarificationsAnswered`, `clarificationBasis` and `correctAfterClarification`. A
question still counts as asked for clarification precision, and a cascade row counts a
question as asked when any pair it tried asked. A relative IRI in the gold resolves
against its BASE before the choices are compared. A question of kind `clarify`
or `unanswerable` is never answered, and `--clarifications score` scores every
clarification as a failure, as the first paid run did.

Answerable accuracy, clarification precision and refusal accuracy have separate
denominators. Only the `unanswerable` outcome is a successful refusal. A provider error,
a refusal by the model, an Ask timeout or an unusable draft is a failed answer with the
status `provider_error`, `refusal`, `timeout`, `budget_exceeded` or `invalid_output`,
and the run goes on. An Ask child that ends by SIGKILL, as when the kernel's
out-of-memory killer or a memory cap stops it, or by SIGABRT after a failed allocation
is the failed case `killed`, since its generated query is the likely cause. A case that
the cost cap refused is `budget_refused`. It leaves the denominators, marks its row
incomplete and ends the run. A harness fault stops a run, such as failing gold, a wrong
binary hash or a child with an unknown exit status.

Each child has capped output, its own process group and cleanup. A matrix has no
wall-clock deadline unless `--phase-deadline` sets one. Ask gets 300 seconds, a query
5 seconds and an Ask token budget of 32768 unless `--deadline`, `--timeout` and
`--max-tokens` change them. Reports go to a new directory outside the repository. A run
names its binary by absolute path and SHA256.

## Checking without a provider

```
python3 -I -B -m unittest discover -s testsuite/ask -p 'test_eval*.py'
python3 -I -S -B scripts/eval-ask --self-test --sparkles /ABSOLUTE/sparkles
scripts/eval-ask-paid --run RUN.json --dry-run
```

`eval_support/grounding_recall.py SET RUN_DIR` measures grounding recall from a run's
saved prompts. For each answerable question it takes the last first-draft prompt, after
any clarification, and checks whether every schema term and every resource of the gold
query occurs in it, by full IRI or by a prefixed name under the prompt's own prefixes.
A dry run gives the same prompts as a paid run, so the metric costs no provider call.
It reports the questions whose prompt holds every gold term and the prompt size. With
`--resource-namespace IRI`, such as `http://dbpedia.org/resource/`, or a set's
`resourceNamespaces`, it also splits the gold terms into resources under those namespaces
and schema terms, and reports each kind apart.

The self-test drives the real CLI through the proxy against a local fake API. It covers
chunked and close-delimited replies, structured-output retries, a downgrade, a
rate limit, an overload that clears and a cascade. `--dry-run` runs a whole run file
against the fake, reads no key and reports request sizes. Neither contacts a provider.

## Batch runs

`sparkles ask --questions FILE` reads JSON lines of `{"id", "question",
"clarification"?, "tryHarder"?, "modelSecrets"?}`, or standard input for `-`. It opens
the store and the model configuration once and prints one JSON result with its `id` per
line, in input order. `--jobs N` runs up to N questions at once on the shared state.
Every other flag applies to each question, and each question keeps its own deadline,
token budget and query memory budget. A line that is invalid, fails or panics gets its
own error result while the rest go on. A line's `modelSecrets` replace the
`--model-secret` of the same name for that question. The question still counts against
the provider's concurrency, request spacing and daily token limits that every other
line shares.

`scripts/eval-ask-paid` asks each phase's cases through one such child, run in its
own systemd user scope with 16G of memory and no swap. `--no-batch` asks each case in
its own child as earlier runs did, and `scripts/eval-ask --batch` chooses the batch
directly. Each case's line names its own gate token through a secret file, so the proxy
books every request to the case that sent it. Before the runner sends a case's line,
the ledger holds the case's projected cost, the 90th percentile of the model's case
costs so far times the projection margin, and no request may spend what another case
in flight still holds. A case whose hold does not fit waits for the cases in flight,
and with none in flight it ends the run as the cap would. The runner keeps at most
`--jobs` lines in flight, sends a second line with the chosen answer for a clarified
case, and holds each case's reply lines until that case and every earlier one are done,
so the report, the case files and `replies.jsonl` keep question order. A batch child
holds a database directory's lock, so the cases are scored after it ends. A line that
the child's input does not take, because the input closed or the child stopped
reading, goes to the next child, and the results of the lines the child did take are
still read to its end. When the kernel or a memory cap kills the child, or a line
passes its deadline, the cases in flight that got no result fail as `killed` or
`timeout` and a new child asks the rest. A child that ends in any other way while lines
are in flight, or prints a result line longer than 4 MiB, ends the run.
Writes, waits and the proxy's drain of a case's requests all end at the phase
deadline, and a run that ends with an error keeps every case that finished, unscored.
A case's wall time is its result's `usage.elapsedMs`.

The store's caches stay warm from one question to the next within a phase, as a running
server's do. This lowers latency against one child per case but leaves the answers
unchanged, and a dry run of the demo set gives the same results and the same replies in
both modes. A query's memory budget applies to each query, but one runaway query that
escapes it ends the whole batch child and fails the cases in flight with it.

## The cost cap

`scripts/eval-ask-paid` starts a proxy on a loopback port and gives the children a
random token in place of the key. `scripts/eval-ask-anthropic` is the same command under
its earlier name. Only the proxy reads `ANTHROPIC_API_KEY`, or `OPENAI_API_KEY` for a
run file whose `provider` is `openai`, and neither key reaches a child. It sends each
request once to `api.anthropic.com`, or to `api.openai.com` for OpenAI, with the
standard library, so Content-Length, chunked and close-delimited replies all work. A
reply that comes back gzip or deflate encoded, although the request asks for none, is
decoded. It sets `stream` to false and the effort from the run file, and it refuses
tools, cache control and any model outside the run. The key is stripped once when it is
read, and a key with whitespace or control characters inside is refused. An error inside
the request keeps only its class, so the key cannot reach a message. The key does stay
in the proxy's initial process environment, which another process of the same user can
read, so the machine should be one only the operator uses.

A JSON-lines ledger at the run file's `ledger` path holds the cap. Before each request the
proxy writes a reserve line with an upper bound from the request size and `max_tokens`,
and the reply writes a settle line with the cost from its usage. A request is refused
when the spent total plus its bound would pass `localCapNanodollars`. The spent total
includes the carryover, so the cap covers the carryover as well, and new requests can
spend the cap minus the carryover. The run file is also refused when the cap plus the
carryover passes the hard cap of one ledger, USD 105 for Anthropic and USD 50 for OpenAI,
which counts the carryover twice and keeps the run on the safe side. The person who pays sets each campaign's total, which can
span several ledgers in separate directories. An error reply is free, a lost reply costs the bound, a reply without usage costs
the bound, and any other fault of the request costs the bound. Replaying the ledger
counts every unsettled reserve at its bound, and the pilot's unresolved $1.002560 is the
carryover line. Reopening a ledger resumes it. Nothing resets it. The ledger's first line
holds its format version, 3, and the provider whose wallet it holds. A reader reads a
version 2 ledger, which predates OpenAI runs, as an Anthropic ledger, refuses a ledger
of another provider than the run's and refuses any other version.

Replay is strict. It refuses a cost or bound that is not a nonnegative integer, a request
ID that is not a positive integer, a settle without its reserve, a repeated reserve, a
carryover anywhere but right after the header, and a reconcile that breaks any rule
below or takes the ledger past the hard cap. A refused ledger stops the run. A ledger from an
older format, an empty file and a ledger cut off between its header and its carryover
are refused the same way, and need a new ledger directory or an edit by hand.

A charge that stands at an upper bound can be reconciled with the amount the provider
billed. Such charges are the carryover, a reserve that a crash left without a settle
line, a request settled at its bound because its reply was lost or its usage could not
be read, and a reply of another model, whose charge is an estimate. `--held` lists them
with each request's model, case, time and request ID. `--reconcile TARGET
--billed-nanodollars N --evidence TEXT` appends a reconcile line that replaces one of
them, where TARGET is `carryover` or a request ID. The evidence is 10 to 1000 printable
characters that name the source of the amount, such as the Console's cost for one day of
an API key that only the run used. The held charge is an upper bound, so an amount above
it is refused unless `--allow-raise` is given, and the line then records the raise. The
command prints the previous and the new amount. Each charge is reconciled once, and
replay checks every reconcile line against the charge it replaced. Neither command reads
a key or sends a request, and both create the ledger with its carryover when it does not
exist yet. Both take the ledger's lock, so they cannot run while a paid run holds it.

```
scripts/eval-ask-paid --run RUN.json --held
scripts/eval-ask-paid --run RUN.json --reconcile carryover \
  --billed-nanodollars 1600000 --evidence "Console cost of key K on 2026-10-15"
```

The local ledger is the authority. A Console workspace spend limit below the account's
balance is a backstop against a defect, and it can lag billing, so the run does not rely
on it.

The Anthropic Console cost export of 2026-10-03 to 2026-11-01 was checked against every
campaign ledger of that period. The ledgers match the export to the cent for each model
on the full days of 2026-10-19, 2026-10-24, 2026-10-25 and 2026-10-31. On 2026-11-01,
up to 12:00 UTC where the export ends, every Opus and Haiku rate matches. Sonnet 5.5
cache reads were billed at $0.10 per million tokens against the $0.20 in the proxy's
table, so the ledger charged $0.57 more than the bill, and the table now holds $0.10.
The Haiku ledger stands about $0.05 above the bill, which is one request that timed out
upstream and was settled at its bound but never billed. Existing ledgers keep the costs
they recorded, and only new settlements use the corrected rate.

Before spending, the run counts tokens, which is free, and sends each model of the run
one structured-output request of at most 256 tokens. It records the status and headers
of the replies in `preflight.json`, without rate-limit, request, organization, workspace
and cookie headers. It stops if any step fails. A block of a set and a model runs only
when the projected cost fits the cap. The projection is the case count times the p90
measured case cost of that model, or a prior from the run file, times a margin.

The proxy appends every forwarded request and its reply, with the key redacted and the
request ID, to `replies.jsonl` in the output directory, so an answer can be diagnosed
later without calling a model again. The file is created readable by its owner only. It
holds every prompt in full, with the grounding drawn from the dataset, and every reply,
so it must not be kept for a private graph beyond the diagnosis and must never be
copied into the repository as a fixture. The first paid run predates this file, so its empty drafts
cannot be traced to the reply that produced them.

## OpenAI runs

A run file with `"provider": "openai"` asks `gpt-6-luna`, `gpt-6.1-terra` or
`gpt-6.1-sol` through the Chat Completions API. The children get an `openai` provider
with the run's effort as `reasoningEffort`, so their requests carry
`reasoning_effort` and limit the output, reasoning included, with
`max_completion_tokens`. The proxy accepts text messages and a JSON Schema or JSON
object format and nothing else, so it refuses tools and explicit cache options. It asks
for the default service tier and relies on OpenAI's default prompt caching, whose
writes last 30 minutes. Ask's prompts keep the stable grounding before the question, so
repairs read the prefix that the first draft wrote. One ledger holds at most USD 50 of
the OpenAI wallet, apart from any Anthropic ledger.

The rates come from OpenAI's pricing page and prompt caching guide of 2026-11-01. A
reply is charged its uncached input, its cache writes at 1.25 times input, its cache
reads and its output, reasoning included, at the rate above 272,000 prompt tokens when
the prompt passes that. A reply that does not report its writes is charged every
uncached input token as a write. A reply from another tier than the default is charged
at least its bound. OpenAI names the dated snapshot that served a model, so a reply
must name the model asked for or that name followed by a date. A reply from another
model, for either provider, fails as a provider error and is charged at least its bound
and its usage at the served model's rates. A model the rate table does not know is
charged the largest premium over the dearest model's bound. Cache counts that pass the
prompt leave the usage unknown, so the reply is charged its bound. The preflight sends one
structured-output request to each model, since Chat Completions has no free token
count.

`scripts/provider-spend openai --since DAY --ledger LEDGER` reads OpenAI's organization
cost and usage reports by day and model and prints them beside the ledger's settlements.
After the Luna baseline phases of 2026-11-01, OpenAI reported $0.8165 for `gpt-6-luna`
in 704 requests against $0.8544 in the ledger, and the smoke requests of Terra and Sol
matched to the cent. Luna's replies reported 4.58 million cache-write tokens of 5.07
million prompt tokens, and the ledger charges them at 1.25 times input. It stands about
4.6% above the bill, which suggests that some of those writes were billed at a lower
rate. The provider never reported more than the ledger.

The OpenAI wallet of 2026-11-01 began at USD 40, and the person who pays raised it to
USD 50 the same day after the Luna baselines. A ledger's cap is fixed in its header, so
the first ledger closed with USD 0.8548 settled and nothing unresolved, and a second
ledger holds the rest with a cap of USD 49.1452. The two caps together equal the wallet.
The Anthropic wallet of the same campaign was raised the same way from USD 50 to 95 with
the approval of the person who pays, so that every model could run its baseline and its
final build on the same three sets. Its first ledger closed with USD 30.7865 of upper
liability, and the second has a cap of USD 64.2135. The person who pays raised the
wallet again to USD 105 on 2026-11-02, so that Opus 5.5 could answer the held-out set
with the same final build as the cheaper models. The second ledger closed with
USD 53.5667 of upper liability, USD 0.0632 of it one request held at its bound, and the
third has a cap of USD 20.6468. The ledgers stay the record of
Anthropic spend until the person who pays reconciles them against the Console's usage
export, which stays outside the repository. Only the reconciled totals are recorded.

## A paid run

`run.example.json` shows the run file. Copy it and set the paths. It holds the sets and
the models of each set, the effort, `maxOutputTokens`, the request and Ask timeouts, the
query timeout, the cap, the carryover, the ledger, the output directory and the binary
with its SHA256. The carryover is required, zero for a fresh authorization, and the cap
plus the carryover cannot pass the provider's hard cap. `clarifications` is `answer`
unless the run file sets `score`. `contextTokens` gives every model its context window.
Without it the server assumes 8192 tokens, treats each model as a small one and trims
the grounding to 30 classes, 60 predicates, two examples and three candidates per
mention, which is how the first paid run asked. A real run refuses a second `.jsonl`
file in the ledger directory, so the ledger file stays the account of record for the
whole spend.

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
fnox exec -- scripts/eval-ask-paid --run /ABSOLUTE/PATH/TO/run.json
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
