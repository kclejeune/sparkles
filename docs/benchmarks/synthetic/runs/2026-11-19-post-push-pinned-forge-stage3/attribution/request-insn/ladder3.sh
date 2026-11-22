#!/usr/bin/env bash
# Instruction counts inside the screen-build ladder (cached nsc-build binaries, another toolchain than the matrix builds).
set -uo pipefail
W=.artifacts/worktrees/forge-backlog
L=.artifacts/lanes/forge-backlog
cd $W
while systemctl --user is-active -q forge-backlog-insn-ladder2; do sleep 10; done
for p in 06ca04ef57d92b04f91b0f6206897ae774585010:c2b5d7cfaf7b4705bebe7664f09917f0334c3d6e c2b5d7cfaf7b4705bebe7664f09917f0334c3d6e:adbe757c2a4cdc411a1edc9c6025d2c068120830 adbe757c2a4cdc411a1edc9c6025d2c068120830:65c570db95f6b1219d3e23b17232a7e5acfcc672 65c570db95f6b1219d3e23b17232a7e5acfcc672:838313bb6f9b45fd1628441eb947274bd054a578 838313bb6f9b45fd1628441eb947274bd054a578:f854145b801fbeaa23899027129f4c996636d5a8 f854145b801fbeaa23899027129f4c996636d5a8:8baa5686fae1fe235870c119e283a254c5e8372d; do
  a=${p%:*}; b=${p#*:}; o=$L/insn/nsc-${a:0:8}-${b:0:8}
  echo "LADDER3 $a $b $(date -u +%T)"
  python3 scripts/request-insn.py run --a ~/.cache/sparkles-nsc/bin/$a/sparkles --b ~/.cache/sparkles-nsc/bin/$b/sparkles     --data $L/insn/people-100000.nt.zst --queries $L/insn/queries --only lang-filter,regex-iri,optional-count --separate-stores --execution streaming     --rounds 4 --reps 50 --server-cpu 2 --client-cpu 3 --out $o > $o.log 2>&1; echo "exit $?"
done
echo LADDER3-DONE
