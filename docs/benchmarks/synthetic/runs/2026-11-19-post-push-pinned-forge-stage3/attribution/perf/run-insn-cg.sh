#!/usr/bin/env bash
set -euo pipefail
cd .artifacts/lanes/forge-backlog/perf
REVS="cc94ac7f 71c277b0" QS="optional-count optional-chain" EVENT=instructions:u OUTP=insn- ./profile.sh
REVS="cc94ac7f 71c277b0" QS="optional-chain" EVENT=cycles:u PERIOD=2000003 CALLG="--call-graph dwarf,16384" OUTP=cg- ./profile.sh
