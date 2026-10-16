#!/usr/bin/env bash
# Codex review of one lane (gpt-6.1-sol, high effort) in a scratch worktree detached at the
# lane's tip, so nothing touches the lane or the main checkout. The findings then go to an
# Opus 5.5 agent that validates them and writes the remediation plan, and a Sonnet 5.5 agent
# implements that plan. AGENTS.md describes the whole workflow.
#
#   scripts/codex-review.sh NAME TIP BASE "CONTEXT"
#
# NAME     short lane name, used for the output directory and the scratch worktree
# TIP      the lane's last commit or branch
# BASE     the commit the lane started from (the review covers BASE..TIP)
# CONTEXT  what the lane was meant to do: specs, scope, anything the reviewer needs
#
# Output goes to $AUDIT_DIR/NAME/ (default target/audits): review.md and review.log.
set -euo pipefail

NAME=$1 TIP=$2 BASE=$3 CONTEXT=$4
ROOT=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
OUT=${AUDIT_DIR:-$ROOT/target/audits}/$NAME
SCRATCH=$ROOT/../sparkles-audit-$NAME
MODEL=(-m gpt-6.1-sol -c model_reasoning_effort="high")
mkdir -p "$OUT"

git -C "$ROOT" worktree add --detach "$SCRATCH" "$TIP" > /dev/null 2>&1 ||
  git -C "$SCRATCH" checkout --detach "$TIP" > /dev/null
cleanup() { git -C "$ROOT" worktree remove --force "$SCRATCH" > /dev/null 2>&1 || true; }
trap cleanup EXIT

PROSE='Docs prose standard: plain human technical writing in full sentences, with no fragment
openers, no label-colon openers, no chains of colons, parentheses or semicolons, and no filler.'

# 1. Review
(cd "$SCRATCH" && codex exec review "${MODEL[@]}" -o "$OUT/review.md" - \
  > "$OUT/review.log" 2>&1) << EOF
This is Sparkles, a Rust RDF store compatible with Jena and Fuseki that uses a QLever-style
index. Review the changes shown by "git diff $BASE..$TIP" adversarially, for correctness
first and then for quality. The commits are listed by "git log $BASE..$TIP". The work was
written by another model. What it was meant to do: $CONTEXT

Read the specs it names. Look for wrong results, data loss, durability or concurrency
bugs, security holes, behaviour that contradicts the spec, tests that would pass without
the fix, missing important test cases, performance regressions in hot paths, code that
does not match the surrounding idioms, and docs that contradict the code. $PROSE

For each finding give the severity (high, medium or low), file:line, a concrete failing
scenario and a suggested fix. Say plainly if you find nothing serious.
EOF

echo "codex-review $NAME: $OUT/review.md"
