#!/usr/bin/env bash
# Instruction counts inside the screen-build ladder (cached nsc-build binaries, another toolchain than the matrix builds).
set -uo pipefail
W=.artifacts/worktrees/forge-backlog
L=.artifacts/lanes/forge-backlog
cd $W
while systemctl --user is-active -q forge-backlog-insn-ladder; do sleep 10; done
for p in 321508fe3fee83062160207a5843e04f0556d689:06ca04ef57d92b04f91b0f6206897ae774585010 06ca04ef57d92b04f91b0f6206897ae774585010:8baa5686fae1fe235870c119e283a254c5e8372d; do
  a=${p%:*}; b=${p#*:}; o=$L/insn/nsc-${a:0:8}-${b:0:8}
  echo "LADDER2 $a $b $(date -u +%T)"
  python3 scripts/request-insn.py run --a ~/.cache/sparkles-nsc/bin/$a/sparkles --b ~/.cache/sparkles-nsc/bin/$b/sparkles     --data $L/insn/people-100000.nt.zst --queries $L/insn/queries --separate-stores --execution streaming     --rounds 4 --reps 50 --server-cpu 2 --client-cpu 3 --out $o > $o.log 2>&1; echo "exit $?"
done
echo LADDER2-DONE
