# SP²Bench results

This report gives complete-response query measurements over the SP²Bench publication
corpus. Read the [reference setup and answer rules](../reference/README.md) with each
result. Each record retains its actual host, binary, query and timing settings.

<a id="sp2bench"></a>
<a id="sp2bench-at-10m-triples"></a>

## Comparison summary

The Forge benchmark answers `q4` in 30.626 s and `q6` in 978.4 ms with Sparkles at
`6b7f32ce`. Both complete answers match independent reconstruction from the source data.
The benchmark gives Sparkles a 4 GiB response allowance; the production default remains
1 GiB. Sparkles is the only engine with a completed `q5a` measurement under the published
600 s request budget. QLever is faster on the checked `q11` comparison. Fluree's native response encoding is
qualified below.

## SP²Bench on Forge

SP²Bench v1.01 generates bibliographic data in the style of DBLP. The corpus contains
10,000,091 triples in a 1.49 GB N-Triples file. Times are mean ± sample standard deviation
in milliseconds and consume the full HTTP response. The server cap is 12 GiB and the
request budget is 600 s. Engines run separately for answer checking and timing.

SELECT timings request `text/tab-separated-values`. Fluree returns a plain TSV dialect
with unquoted string fields, rather than SPARQL RDF-term TSV. Its typed correctness
captures use SPARQL JSON. These native endpoint times include different serialization
costs, so Fluree's SELECT figures do not establish format-matched latency ratios. ASK
timings request SPARQL JSON on every engine.

The recovered Sparkles `q4` and `q6` cells and completed QLever and Fluree cells use two
warm-ups and ten timed requests. The paired Sparkles and Jena `q12a` cells and Jena's `q12b` use thirty
requests after ten warm-ups. Sparkles and Oxigraph `q12b` pool two reversed-order runs of
sixty requests after fifteen warm-ups per engine. Remaining cells retain their recorded
full-suite settings. `mise run bench:sp2b 10m` runs the suite with the repaired defaults.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| q1 | **3.5 ± 0.1** | 8.3 ± 0.8 | 9.9 ± 1.6 | 3.7 ± 0.2 | 29.6 ± 5.7 |
| q2 | **771.6 ± 11.2** | 35233.4 ± 1583.9 | 3232.6 ± 20.3 ‡ | 3235.6 ± 38.1 | 40256.7 ± 93.7 |
| q3a | **45.1 ± 4.9** | 1281.1 ± 497.7 | 236.8 ± 8.2 | 2519.7 ± 26.2 | 2868.4 ± 35.4 |
| q3b | **5.2 ± 0.2** | 515.3 ± 40.1 | 9.9 ± 0.9 | 2088.3 ± 41.3 | 2035.7 ± 15.5 |
| q3c | **4.6 ± 0.4** | 472.2 ± 10.5 | 7.3 ± 0.7 | 2161.0 ± 46.9 | 2030.6 ± 15.0 |
| q4 | **30626.0 ± 34.5** | timeout | error | error | timeout |
| q5a | **3946.0 ± 665.9** | timeout | timeout | timeout | timeout |
| q5b | **234.9 ± 23.4** | 7036.6 ± 80.5 | 611.1 ± 21.3 | 2708.5 ± 20.3 | 21273.6 ± 22.8 |
| q6 | **978.4 ± 18.5** | 108648.1 ± 454.0 | wrong answer | timeout | timeout |
| q7 | **145.0 ± 7.5** | timeout | 304.8 ± 14.2 | 5460.1 ± 77.1 | timeout |
| q8 | **8.3 ± 1.0** | 35.2 ± 2.9 | error | 187.5 ± 5.4 | 1911.6 ± 52.6 |
| q9 | **158.5 ± 33.5** | 4227.3 ± 15.1 | 284.0 ± 6.8 | 6436.7 ± 23.0 | 5202.0 ± 22.1 |
| q10 | **3.8 ± 0.2** | 7.3 ± 0.5 | 7.1 ± 0.7 | 4.2 ± 0.2 | 7.6 ± 0.9 |
| q11 | 103.1 ± 9.0 | 375.8 ± 21.4 | **43.5 ± 3.3** | 52.2 ± 1.6 | 31619.1 ± 97.9 |
| q12a | **3.8 ± 0.1** | 9.0 ± 1.1 | 248.7 ± 7.4 | 1694.3 ± 22.0 | 6794.7 ± 61.5 |
| q12b | 4.0 ± 0.1 | 9.7 ± 1.3 | error | 186.7 ± 5.8 | **3.7 ± 0.1** |
| q12c | **3.6 ± 0.1** | 6.7 ± 0.2 | 5.2 ± 1.5 | 3.8 ± 0.1 | 3.7 ± 0.1 |

The [raw Sparkles record](runs/2026-10-25-error-recovery-forge/sparkles-record.json) retains its original samples and
`agrees=null`. Its [derived record](runs/2026-10-25-error-recovery-forge/sparkles-qualified-record.json) and
[validation receipt](runs/2026-10-25-error-recovery-forge/validation.json) bind those samples to independent source
reconstruction, rather than a majority vote. Sparkles's binary is `6b7f32ce`, SHA-256
`cf6f4cf5fd754c8344fd0f0c02b81dce5016c8b8ccfcbe034f5fe7927cfd46c8`.
Its compiler identities and effective limits are retained with the record.

The fresh [QLever record](runs/2026-10-25-error-recovery-forge/peers/qlever-record.json) and
[Fluree record](runs/2026-10-25-error-recovery-forge/peers/fluree-record.json) supply their completed cells. Their
[receipt](runs/2026-10-25-error-recovery-forge/peers/validation.json) compares complete row counts, value fingerprints
and ordering fingerprints with each other and retained source-matched captures. This is
majority screening. The raw Fluree record also carries earlier QLever rows; only its
Fluree rows are used here. A `‡` retains equal values returned as different RDF terms.

Sparkles and Oxigraph `q12b` come from the
[pure ASK prefix record](../2026-10-25-pure-ask-prefix-q12b-forge.json), with Sparkles at
`b26ce6a9`. Their 7.8% mean difference is inside the ten-percent tie band. The paired
`q12a` cells and Jena's `q12b` come from the
[paired record](../2026-10-25-competitive-query-pairs-forge.json), with Sparkles at
`2ec8a266`. Other retained Sparkles cells use `6699f27d`. The
[full-suite record](../2026-10-23-sp2b-10m-forge.json) supplies the retained peer cells
and their original failure statuses. Its original answer check kept all servers up,
which killed QLever and Fluree during `q4`; fresh isolated checks replace that missing
answer evidence for the completed peer cells above.

### Complete answers and remaining limits

The `q4` answer contains 41,073,618 rows and 1,955,839,207 bytes of TSV. The benchmark's
`--max-result-mb 4096` removes the 1 GiB response refusal without truncating the answer.
The `q6` answer contains 906,313 rows. Both match the source oracle under the explicit
`row-sha256-v1` algorithm, which normalizes RDF terms and projected column names, hashes
every row, and sorts all row hashes while preserving duplicate multiplicities. The
checker uses bounded buffers and a 64 MiB external sort. Full-answer checking is separate
from the timed requests, which consume TSV into `/dev/null`.

QLever's complete Forge `q6` response contains 1,256,087 rows and disagrees with the
source oracle, so its latency is excluded. The [retained witness](runs/2026-10-25-error-recovery-forge/peers/qlever-q6-witness.json)
contains actual returned publications of an author after that author's earliest year,
plus the earlier source publication and typed integer year. QLever's `q8` and `q12b`
requests exceed its 8 GiB query allowance on Forge. Raising it to 10 GiB produces
incorrect answers before later requests exhaust the 12 GiB server cap; those
[probe bodies and failures](runs/2026-10-25-error-recovery-forge/peers/budget-10g) are retained separately.

Sparkles's 385,208 `q5a` rows match an independent data oracle. Other engines' `q5a`
timeouts remain missing comparisons. Fluree and Oxigraph `q6`, and Jena and Oxigraph
`q7`, also retain their bounded timeout statuses rather than partial answers.

QLever's checked `q11` mean is 43.5 ms and Fluree's is 52.2 ms, against Sparkles's
103.1 ms. These complete answers and their ordering match retained references. Remaining errors, unsupported formats, wrong answers and bounded timeouts stay
explicit rather than receiving inferred timings.

## Separate Wally memory profile

These measurements use a Core i7-12800H with 31,766 MiB of RAM, a 24 GiB server cap and
no swap. QLever's query allowance is 20 GiB; Jena uses a 16 GiB heap and 256 MiB thread
stack. Every engine runs alone with the same data and unchanged query text. Power-state,
compiler and binary receipts are retained. The laptop was on AC power with a full battery,
a `powersave` governor and turbo enabled. These results have no cross-host speedup ratio.

| Query, mean ± σ ms | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|
| q4 | 15115.9 ± 41.9 | timeout | wrong answer | OOM | timeout |
| q8 | **9.8 ± 0.5** | 53.2 ± 17.4 | wrong answer | 232.5 ± 13.2 | 1993.2 ± 15.8 |
| q12b | 5.7 ± 0.3 | 12.0 ± 1.1 | wrong answer | 223.8 ± 14.6 | **5.2 ± 0.3** |

The [suite receipt](runs/2026-10-25-error-recovery-wally/suite-validation.json) binds the raw records, actual answer
bodies and timing files. Correct answers alone received timings, with two warm-ups and
three samples for `q4`, and two warm-ups and ten samples for `q8` and `q12b`.
The [Sparkles `q4` proof](runs/2026-10-25-error-recovery-wally/validation.json) uses the same independent source
oracle. Fluree exceeds the 24 GiB cap on `q4`; Jena and Oxigraph exceed the 600 s request
budget. A separate [Oxigraph extended check](runs/2026-10-25-error-recovery-wally/oxigraph-extended/validation.json)
verifies all 41,073,618 `q4` rows under a 1,800 s request budget. It is an answer-only
check, with no warm latency estimate.

QLever's `q4` has the expected row count but a different full digest. Its
[actual-row witness](runs/2026-10-25-error-recovery-wally/raw/qlever-q4-witness.json) includes name pairs that violate
the query's Unicode codepoint string comparison, as specified by the
[SPARQL operator rules](https://www.w3.org/TR/sparql11-query/#OperatorMapping).
Its `q8` body is a valid zero-row SELECT result where the checked peers return 656 rows.
Its `q12b` body contains `false` where the checked peers return `true`. These are
semantic disagreements, not timing results. Tiny dataset-independent
[control queries](runs/2026-10-25-error-recovery-forge/peers/diagnostic-controls) reproduce the string comparison and
OPTIONAL disagreements on QLever; Sparkles and Jena return the expected answers.

## Loading

The full-suite loading record measures Sparkles at 3.93 s, against 8.90 s for Fluree,
9.70 s for QLever, 20.49 s for Oxigraph and 44.88 s for TDB2. Its index occupies 307 MiB,
against QLever's 294 MiB. Sparkles's loading peak is 2,318 MiB, against Fluree's 1,835 MiB
and QLever's 1,879 MiB. These loading measurements keep their own recorded binaries and
settings; the targeted query reruns do not replace them.
