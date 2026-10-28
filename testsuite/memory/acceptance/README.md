# Project handover memory corpus

This fictional project handover exercises the existing C17 and C18 memory APIs without
model calls, external feeds or network inference. `corpus.json` contains source text,
propositions and declared source lineage. `expected.json` independently records exact
source and proposition sets. The driver does not generate expected results from writes
or query responses.

The handover and an independently maintained roster put Ana on Platform. A mirror copies
the handover, while a stand-up puts Ana on Payments. A restricted personnel note gives a
third assignment. A separate note contains a quoted instruction with forged citation
syntax and a Unicode character. A corrected handover moves Ana to Payments, the stand-up
is withdrawn and a scratch branch proposes Research. The vocabulary and entity records
are the only data loaded outside the API workflow.

The router test `memory_acceptance_project_handover` registers sources over MCP, checks
a mismatched quotation is refused, and alternates MCP and HTTP fact writes. It compares
both recall surfaces against the expected source occurrences, checks conflicts under a
single-value constraint, checks citation identity against the write activity and
persisted reifier, and verifies a restricted reader sees no hidden fact or citation.
Retries preserve the original write and citations. Revised-source extraction retracts
stale facts while keeping other sources. Historical recall preserves the original
commit. Scratch writes remain separate from main. Retrieved instruction text remains
escaped data in the text response.

Run the corpus from the repository root with:

```sh
mbx test -p sparkles-server --bin sparkles memory_acceptance -- --nocapture
```

The module requires the `mcp`, `auth` and `shacl` features, which are enabled in the
standard build. It uses the existing whole HTTP router test transport. It does not
measure text search, vector retrieval, model accuracy, practical effectiveness or
latency. Explicit entity seeds make the expected retrieval deterministic.

The copied source remains separately identifiable, and its `copyOf` and
`independenceGroup` fields are corpus ground truth. The driver does not label the
server's source counts as independent corroboration. It does not authenticate lineage
or install the C25 profile through legacy asserting writes.

## Remaining adopted acceptance

The `futureGates` records are requirements for later integration, not passing tests or
ignored test cases. They identify these existing dependencies:

| Required behavior | Roadmap dependency |
|---|---|
| Authenticated source lineage and independent support assessment distinguish a mirror from an independent observation. | Item 81, C25 A4 and Phase 3. |
| Review and withdrawal maintain distinct accepted occurrence supports through retries and crashes. | Item 81, C25 A1, A5 and A14. |
| Server-owned availability establishes knowledge cutoffs, and valid-time expiry differs from recorded history. | Item 341, C27 A4, and C25 Phase 2. |
| A staffing decision and its dependent onboarding task need review after correction, while unrelated decisions stay fresh. | Item 340, F22, and item 339, C26. |
| Recall exposes authored record kinds, accepted review state, lineage and evidence paths through the shared profile. | Item 81, C25 Phase 3 and C17/C18 integration. |

This corpus verifies compatibility of current asserting writes and retained history.
It does not complete the shared evidence, valid-time or maintained-analysis contracts.
The [AI capability guide](../../../docs/ai/README.md) describes the current boundary.
