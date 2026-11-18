# Pre-decode of aggregate arguments on forge

This manual record times roadmap item 1098 on forge, an Intel Core i5-13500 with kernel
7.2.9, against main and against the build before the grouping operator of item 826.
[record.json](record.json) holds the binary and data identities, the settings and each
query's medians and per-round ratios. The raw samples, answer hashes, scripts and queries
are kept under [raw](raw).

The three arms are 59d29b40 with the fix, bccc5f7b as main with item 826, and 92371f0f as
the main before item 826 merged. All three were built on forge by
[run.sh](raw/run.sh.txt) with `mbx build --release -p sparkles-server --bin sparkles` and
the same Clang 21.1.8, GCC 16.2.0, rustc 1.99.1 and LLD 23.1.1 compilers. The data is
1,000,000 generated people, 10,527,319 quads, loaded once by the 92371f0f binary into one
store that every server opened.

[ab.sh](raw/ab.sh.txt) ran two pairs, main against the fix and the earlier build against
the fix, each in three rounds of A,B,B,A. Each round started a fresh server per arm with
one query thread and the result cache off. Each server ran two untimed and seven timed
requests per query and mode, so each arm has 42 samples. The servers were not pinned to
CPUs, no spinners ran, and steal was not recorded. The run held the host's heavy lock, so
no build or other measurement overlapped it. Every query returned the same sorted answer
in every server, mode and arm.

Streamed `sum-if` and `bind-twin` take about 13.5 ms in most server processes and 17 to
22 ms in some, and the slow processes occur in all three builds. In the second pair both
fix servers of round 3 fell in the slow mode, which gives the pair's streamed medians of
1.348 and 1.318. Their per-round ratios fall below 1.0 in round 2, so they are noise of
the comparison, and the first pair reads 1.003 and 0.995 for the same binary.
