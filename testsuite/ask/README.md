# Ask demo evaluation

The demo contains 68 questions over `org.ttl`. It includes 59 answerable
questions, four clarification questions and five unanswerable questions. Public
QALD and Text2SPARQL datasets need separate licensed data, pinned gold results and
reference metric alignment. This fixture does not provide that coverage.

`scripts/eval-ask` reports one `demo-pipeline` row per configured pair and a
separate real cascade run. Each pair supplies the draft, repair and summary roles
together. These rows measure the final pipeline. They do not measure independent
repair or summary accuracy. Citation checks remain unjudged diagnostics. Whole-
pipeline token and estimated cost totals appear once. Unknown estimated cost stays
unknown.

The report uses schema 2 and `rdf-term-set-global-columns-v1`. SELECT answers are
sets of full row tuples with an explicit unbound cell. A single column bijection
applies to the whole relation. URI lexical identity, literal datatype and lexical
identity, and language tags are preserved. Language tags compare after ASCII
lowercasing. Plain literals and explicit `xsd:string` literals compare equally.
Numbers are not rounded or approximately compared. Duplicate rows are counted as
diagnostics and do not change set scores. Blank nodes, more than eight columns,
more than 1000 rows and searches beyond one million tuple cells are unsupported.
Gold that needs these forms fails preflight before a model is called. Offline
scoring also refuses SERVICE before spawning a query child. Its conservative lexer
distinguishes comments, quoted text, IRIs, variables and prefixed names. Comments
end at CR, LF or CRLF, so SERVICE after any of those endings is refused before
gold or first-draft execution. An excluded first draft remains unknown and stays
outside the conditional failed-first denominator. Unicode
escapes and uncertain lexical forms are unsupported, rather than executed. The
retained CLI's explicit outbound allowlist is additional protection. It is not
claimed to be a deny-all CLI option.

Precision and recall use intersecting whole rows after the best consistent column
mapping. The report keeps macro values, micro numerators and denominators, and ASK
exact match separately. Empty versus empty scores one. A one-empty mismatch scores
zero. Answerable execution accuracy, all-kind success, clarification precision and
refusal accuracy have separate denominators. Only the explicit `unanswerable`
outcome is a successful refusal. The repair rate divides repaired-correct answers
by all eligible answerable questions. Its conditional counterpart divides them by
failed-first questions. The first retained draft is checked with the same bounded
read-only query path and validated gold. A gold-correct empty first result does
not become a failed first draft merely because repair ran. An incomplete first
probe is unknown and stays outside the conditional failed-first denominator.
Repair invocation is a separate diagnostic. This strict RDF-term policy is not an
official QALD score.

Every selected gold query runs before the first Ask. Reports retain bounded raw
query results and process status for recomputation. A failed, malformed, truncated
or unsupported gold result is a preflight failure. Candidate query failure is a
separate result status. A valid JSON object cannot hide a nonzero process exit,
noise before JSON or multiple final objects. Each child has incremental 4 MiB
stdout and 1 MiB stderr caps, an owned process group and a bounded two-second
cleanup allowance. The initial phase has one monotonic 900-second deadline. Ask
receives deadline 30, query timeout 5, total token limit 32768 and row limit 1000.
Reports require a new directory outside the repository, an explicitly selected
absolute binary and its SHA256. Existing outputs and fallback binaries are
refused.

The credential-free check uses the exact retained CLI admitted for this work:

```
python3 -I -S -B scripts/eval-ask --self-test --sparkles /ABSOLUTE/PINNED/sparkles --jobs 1 --deadline 30
python3 -I -B -m unittest discover -s testsuite/ask -p 'test_eval*.py'
```

The self-test pins the retained binary hash in its source. It exercises loopback
fake Anthropic responses, actual structured retries, auto fallback, cascade and
transport retries through the real CLI. A labelled test-only HTTP status adapter
makes the actual Rust client attempt three requests after an unresolved first
failure. The real gate refuses later upstream dispatch. This adapter does not
change the production gate's terminal refusal statuses or prove vendor behavior.
These checks make no real provider requests and use no actual credentials.

`scripts/eval-ask-anthropic` is the private initial spending owner. It requires a
separate SHA-bound root runtime admission. No admission or real credential is
shipped here. The admission binds reviewed source hashes, the exact binary, data,
question selection, model and billing authorities, runtime, wallet and grant-claim
paths, a root-verified local filesystem and a fresh output directory. The admitted
pilot contains exactly 17 distinct question IDs and 68 planned Ask executions.
Root must assign the same single claim path to the entire grant. This trusted root
decision is not inferred from a field in an untrusted model response.

The owner alone reads its admitted key environment variable. Leaf CLIs receive an
explicit environment with only an unpredictable local gate token and runtime
settings. Their rendered configuration contains one loopback provider with three
allowed models and fixed JSON-schema output, 1024 output tokens and a 15-second
request timeout. Explicit thinking and sampling controls are omitted. This leaves
vendor-default adaptive thinking and effort in effect. Thinking is already inside
the output token limit and is never charged twice.

The gate injects standard-only tier, global geography and nonstreaming requests.
It rejects unknown billing extensions, tools and cache controls. Request bodies
are capped at 128 KiB before parsing. Upstream responses are capped at 1 MiB.
The cumulative raw status and header bytes across all informational and final
responses are bounded at 64 KiB, including line terminators. The gate enforces
that budget while reading, before accumulating an oversized header section.
Error diagnostics are separately bounded at 64 KiB. Settlement requires a status
that permits response content, one valid Content-Length within the body cap and
exactly that many body bytes. The gate refuses 204 and 205 before reading a body
because neither can provide its required usage response. A 204 ends at its
headers and prohibits Content-Length. A 205 prohibits content even when its
Content-Length is zero.
Duplicate lengths, conflicting framing and premature EOF keep the full
reservation unresolved and stop further dispatch. This transport supports only
Content-Length framing. It refuses even valid chunked responses and refuses
close-delimited responses with the same unresolved liability and terminal stop.
Nonstreaming requests do not guarantee Content-Length responses from a provider.
The only real upstream is the
standard global Anthropic Messages HTTPS endpoint. It uses certificate
verification, an explicit header allowlist, no redirects, no proxy discovery and
one dispatch invocation per durable reservation. The three conservative attempt
bounds are 1002560000, 4010240000 and 8020480000 USD nanodollars. They cover one
million input tokens at the highest applicable input or cache-write rate plus 1024
output tokens. Settled usage applies exact rates once to input, cache-write
subtypes, cache reads and output. Haiku's higher rate starts above 100000 prompt
tokens. Provider-default adaptive thinking remains part of output usage.

The SQLite schema 2 wallet uses FULL synchronization, an exclusive owner lock and
a separately durable exclusive grant claim. It syncs the wallet directory after
every committed transaction, before a reservation can authorize a send. One
aggregate 25000000000-nanodollar cap covers all models, processes, attempts and
cascade. Every dispatch follows a committed unresolved reservation. A complete
validated response settles that reservation once. Missing, inconsistent or
ambiguous usage keeps its full bound and stops admission. A failed settlement,
including a post-commit directory sync failure, keeps the full original bound in
the live report. That report labels uncertain settlement IDs separately from the
retained raw database state. A persisted settled row alone never authorizes
recovery. No retry, process restart or fresh wallet can reuse the same grant
claim. Recovery is deliberately not automated. An incomplete campaign may retain
completed observations but cannot claim a complete matrix or choose model
defaults. The gate's settled spend and unresolved upper liability are the money
authority. CLI estimated costs are diagnostics. An unconfigured SDK price of zero
is marked unknown in the pipeline report. Its raw SDK value remains in the
captured output. The pipeline's token totals are explicitly labelled as CLI-
reported values.

Separate source review, fresh validation and root runtime admission are required
before paid execution. Roadmap item 6 remains open for public corpus evaluation
and independent role experiments.
