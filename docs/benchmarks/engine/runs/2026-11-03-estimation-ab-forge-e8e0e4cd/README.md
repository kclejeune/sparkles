# Answer-check limitation of the estimation comparison

This manual note qualifies the correctness evidence in the immutable
[run record](record.json) and [commands](run.sh.txt). It changes no timing sample,
q-error, plan, source or executable identity, per-round ratio, rerun or raw answer
marker. The timing comparison remains the historical comparison of `125edd41` and
`e8e0e4cd`, rather than a measurement of later query optimizations.

The retained `abtime.py` digest hashes only the response's SELECT `rows` member.
CONSTRUCT and DESCRIBE responses hold their RDF graph in `triples` and `quads`,
which this digest does not inspect. The comparison's matching graph-query markers
therefore compare empty SELECT tables. They do not prove that the RDF graph outputs
agree, and do not establish that an output was wrong either.

The collection contains 228 SELECT queries and six graph queries. BSBM Explore
`q9-1`, `q9-2` and `q9-3` are DESCRIBE, and `q12-1`, `q12-2` and `q12-3` are
CONSTRUCT. Its SELECT fingerprint comparisons remain evidence of agreement under
the recorded checker. The six graph-query markers must be treated as unverified.
The estimates and actual rows in the associated q-error report came from
`scripts/bench-estimates/qerror.py`, whose historical digest likewise omitted graph
triples and quads. Its graph markers have the same limitation.

Later comparisons that inspect actual graph responses provide new evidence for
their own source revisions and retained inputs. They do not retroactively validate
these older graph markers. Complete graph coverage requires checking each of the
six queries, with graph-set and blank-node semantics, rather than treating an empty
SELECT result or a matching triple count as equality.
