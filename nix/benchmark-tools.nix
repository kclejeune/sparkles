# The auxiliary tools that the benchmark runners and test scripts take from the nixpkgs
# revision in flake.lock (`nixpkgs#<name>` in scripts/bench-tools.sh and the runners).
# The flake's `cacheable` output builds each of them so that the binary cache holds them,
# and scripts/bench-tests/test_bench_nix_closures.py fails when a script resolves a
# package that this list or nix/benchmarks.nix does not name.
[
  "apache-jena"
  "jdk"
  "jdk17_headless"
  "jdk8_headless"
  "lbzip2"
  "oha"
  "time"
  "zstd"
]
