# Rebuilt midpoint and repeatability limits

This manually maintained supplement retains two later midpoint checks. D is
`e951e2c3`, the classifier merge between cached midpoint M and B. It was built
on wally with mbx 1.29.1, learned incremental disabled and the native toolchain
that matches the original executables. Its SHA-256 is
`86fc34d32ad1cd17cb27387fd86425305bd2015d47335dd182b64cf40995b9a2`.
The historical Nix shell instead selected GCC 15.3 and Clang 23, so that first
build was excluded without timing. The accepted build records GCC 16.2.0,
Clang 21.1.8, rustc 1.99.1 and LLD 23.1.1, as do A, M and B.

The accepted D executable and B have the same `.text` size of `0x6568f2d`.
All four selected hot functions retain their normalized instructions across
A, M, B and D. D's selected function addresses are B's addresses plus
`0x2000`. The retained disassemblies show the comparison without asserting
that every function in the executable is unchanged.

The later checks reuse A, M and B's existing stores, and D shares B's store.
The production changes do not change the query engine or its store format.
No new load or load timing is performed. A requested pre-load trim required
administrator authentication unavailable to the unattended command, so the
initial load-trim limitation remains. The same server/client CPU placement,
spinners, three warmups, 100 measured requests, counters, answer checks and
plan normalization apply.

Each check runs four rounds in each order, A,M,D,B,B,D,M,A and
B,D,M,A,A,M,D,B. The short check has only `regex-iri` and `types-grouped`.
The second check restores the original six-query mix. This table reports
each variant's regex time divided by A's time within that check.

| Check | A ms | M/A | D/A | D/A 95% interval | B/A | B/A 95% interval |
|---|---:|---:|---:|---|---:|---|
| Two queries | 3.85006 | 1.00020 | 0.99542 | 0.98565 to 1.00503 | 1.00381 | 0.99875 to 1.00963 |
| Six queries | 3.80813 | 1.00574 | 1.01417 | 1.00486 to 1.03016 | 1.01535 | 1.00592 to 1.02800 |

The base itself takes longer than the initial comparison's 3.46750 ms. Neither
later check reproduces the full initial A/B gap, so these checks do not establish
a stable six-percent latency change at the classifier merge. In the six-query
check D and B repeat the other signatures, with `exists-join` at 1.024 of A
and `star-join` at 0.926 and 0.928. The regex instruction counts remain
75.202 million per request for every variant in both checks. All timed native
and TSV answers agree, all normalized plans agree, and all accepted counter
intervals have equal enabled and running time.

The source comparison confines the remaining production changes to the
classifier merge, and the counters establish no additional query work in the
measured cases. The first run and the unchanged hot instructions are consistent
with a layout effect. The absolute-time drift and weaker later contrasts leave
the responsible cache or process state and a stable timing attribution to a
particular merge unconfirmed. Recovery requires reproducing those conditions
against current main. These records do not claim a repair.
