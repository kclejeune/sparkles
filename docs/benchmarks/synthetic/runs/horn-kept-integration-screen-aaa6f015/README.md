# Horn classification and kept-frame integration screen

This manual record preserves one generated Namespace comparison. It is independent
of the generated record tables selected by [report.json](../../report.json). The compared
source revisions are `7070fa7c1d01cf19d08895ce591dbffd2c6e7d34` for A and
`aaa6f015d813816e1572a1fd2d021caae570b0cf` for B. The record supports only that
comparison and does not qualify a later integration build.

Both release binaries were built on atlas. Their SHA-256 values are
`abe9f7014f36a5fb141073fcfdc2a24b07d43f845b1a9112e2486c8cd93c6058` for A
and `5a3201786097e0fec47741e8b2b80d3700774046aec1e3c8f49b84bb181b4acf` for B.
The [generated summary](summary.md.gz) retains the matching Rust, linker, C and C++
compiler identities. The [manifest](manifest.txt) records the build hosts and
Namespace instance. The binaries themselves remain external to this record.

The engines ran alone in alternating processes on the same 8-vCPU, 16-GiB AMD
EPYC Namespace instance. The server used CPU 2 and one Rayon thread, and the
client used CPU 4, with spinners enabled. The input was generated for 100,000
people. Four A,B,B,A rounds gave each query three warm-ups and 20 timed requests
per process. The retained [harness](stage/nsc-bench-remote.sh),
[run configuration](stage/run.env) and [query archive](stage/queries.tar)
identify the timed method and inputs.

The unchanged [raw result archive](results.tgz) contains per-request samples,
environment and load records, server logs and sorted TSV answer fingerprints.
Both engines returned the same answer fingerprint for every one of the 30
queries. No median ratio in either timing measure reaches 1.10, so this screen
selected no longer threshold rerun. The
[machine-readable summary](summary.json) retains the unrounded ratios and
between-process spreads. The [copy receipts](SHA256SUMS) verify that the archived
inputs and generated outputs retain their original bytes. The summary is stored
with gzip compression to preserve its original whitespace, and its
[decompressed receipt](summary.md.sha256) checks the original Markdown bytes.

Server execution and complete HTTP wall time are distinct measures. Eight
queries have a positive median difference in either measure, and their process
spreads exceed those differences. This does not establish a regression, a
mechanism, an Intel cost or the absence of cost. Roadmap item 752 retains all
eight leads and any additional accepted positive medians of the final source
comparison for quiet pinned Intel confirmation and attribution. The
[qualified result](../../RESULTS.md#horn-classification-and-kept-frame-screen-leads)
keeps them separate from peer-engine comparisons.
