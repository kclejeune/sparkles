#!/usr/bin/env bash
# Keep CI and the local gate on the same shell checks. Benchmark run captures retain
# the exact bytes used during measurement, including historical formatting and lint.
set -euo pipefail
cd "$(dirname "$0")/.."

scripts() {
  git ls-files -z -- '*.sh' scripts/cargo-sparkles-task ':(glob,exclude)docs/benchmarks/*/runs/**'
}

scripts | xargs -0 shfmt --diff --indent 2 --case-indent --space-redirects
scripts | xargs -0 shellcheck
