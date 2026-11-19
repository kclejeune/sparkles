# Attribution of the stage-3 drift

These files were made on the laptop, an Intel Core Ultra X7 358H whose performance cores are
CPUs 0 to 3, while it carried other work. They locate and name costs and do not size them.
For each rerun, create a fresh scratch directory and set `L` in copies of the
archived scripts to its absolute path. Each profile replaces its output directory, and
the count ladders write new logs under `L`. Stage the recorded dataset and query files
under `$L/insn` and the matrix binaries under `$L/bin`. The two screen ladders use
`~/.cache/sparkles-nsc/bin/<full-revision>/sparkles`. In all three ladders, set `W` to the
absolute path of the source worktree containing `scripts/request-insn.py`. Set the perf
wrapper's working directory to the absolute `$L/perf` path with copies of `profile.sh`
and `drive.py`, and set the same absolute `L` in `profile.sh`.

`request-insn/` holds per-request instruction counts from `scripts/request-insn.py` at
1.05M triples, streaming, one Rayon thread, the server on CPU 2 and the client on CPU 3, in
four rounds of A,B,B,A with 50 counted requests per query and process. The `matrix-*` files
compare consecutive matrix builds, whose hashes are in `../binaries.sha256` apart from
that of 8baa5686, 20a381e40aa22acf2b099436054025829a753a69ca618156342d8af9b1f59d03, which the
[batch-30 matrix record](../../2026-11-18-executor-matrix-forge-8baa5686/manifest.txt) holds. The `screen-*`
files compare cached builds of `scripts/nsc-build.sh` with GCC 16.2.0 and clang 21.1.8, which
are compared only with each other. `ladder.sh`, `ladder2.sh` and `ladder3.sh` ran them, and
`exec-ratios-stanley.txt` holds the execution-time round ratios of the same runs, which the
laptop's load makes unfit for sizing.

`perf/` holds flat user-space profiles from `profile.sh`, which loads each build's own store,
serves it on CPU 0 with one Rayon thread, sends 100 warm-up requests and then records a fixed
number of streamed requests from CPU 1, 4,000 for `optional-count` and 1,500 for the others.
`symtab.py` turns the samples into events per request by core type and symbol, and the first
line of each table names its event. The `instructions-*` tables sample `instructions:u` and
the `cycles-*` tables sample `cycles:u`, both every 200,003 events. `run-insn-cg.sh` ran the
instruction profiles as `profile.sh` with `REVS="cc94ac7f 71c277b0"`,
`QS="optional-count optional-chain"`, `EVENT=instructions:u` and `OUTP=insn-`, and the cycle
profiles ran `profile.sh` with `REVS="cc94ac7f 8baa5686 71c277b0"` and its defaults.
