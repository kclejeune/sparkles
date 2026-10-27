# Hosted cache comparison

This opt-in experiment compares the current remote mbx object cache with a Swatinem dependency target cache and a combination of both. It runs only when the explicitly named `ci-cache-comparison-20261027` branch is pushed. It does not change the production workflow.

Every producer checks out source `55531c16e81f492b72f86f5f575f1659391cf7ac` at the normal repository path after staging the diagnostic helpers outside that checkout. Rust 1.99.1, mbx 1.28.0, cargo-nextest 0.9.146, the compiler profile, default job count, underlying linker and archive recipe match across arms. All Cargo builds and Swatinem Cargo metadata calls go through mbx.

| Arm | Cargo target transport | mbx object transport | Target views |
| --- | --- | --- | --- |
| remote | None | Remote | Enabled |
| target-local | Swatinem | Local only | Disabled |
| hybrid | The same Swatinem payload | Remote | Disabled |

One hybrid seed job creates a new branch-scoped target cache. Three warm jobs then run on separate fresh hosted runners, serially, and both target-cache arms must restore that exact seed payload. The seed and warm phases do not reuse a mutable target or local object store. Only the seed saves the target cache. A missing exact restore invalidates the warm sample. Cache keys include the experiment prefix and workflow run ID, so a later run cannot silently inherit an earlier experiment's target seed.

Swatinem owns only the dependency target and Cargo registry/git transport. It excludes workspace crates and installed Cargo tools, and never transports mbx's object store. mbx owns its object store. The remote URL, namespace and protected-branch write policy remain unchanged. An ordinary experiment branch reads the remote and cannot publish new remote objects. The 30 GiB mbx budget and 10 GiB free-space minimum match the production archive producer.

The two Swatinem arms disable managed target views so `target` remains an ordinary directory. They retain native link object caching, build-script execution caching and shared generated-source settings, matching the remote arm. This target-view difference is intentional and must be included when interpreting the result.

Swatinem's normal profile cleanup removes `.mbx-build-script-shims`, while cached dependency build-script launchers still refer to that directory. After restoration, the helper validates mbx 1.28.0's Linux launcher format and recreates only the referenced helper binaries from the verified installed mbx. It does not execute cached shell text or change launcher mtimes. Unknown launcher forms and paths escaping `target` invalidate the measurement. This repair has its own timing receipt.

The mbx action also offers a native GitHub target-cache mode. That mode already handles its own target pruning and helper hydration, and disables managed target views and native link object caching. It is a documented alternative rather than a fourth experiment arm. See the [pinned action documentation](https://github.com/jdx/mr-boxington-action/tree/v1.7.3#github-actions-cache).

The first run contains four archive producers with a 45-minute limit each. It has no automatic retry. Seed cost is reported separately. Expected total runner time is roughly 60–100 minutes based on prior jobs, with a 180-minute producer limit. Review the concrete branch before its first push because that push starts the experiment. The workflow does not need to be added to the default branch or dispatched there.

Each arm must build the workspace archive and pass workspace doctests. The warm jobs upload their real compressed archive with one-day retention so upload time reflects production work. An additional correctness phase lists tests from the archive and checks binary and test counts. Its extra extraction/listing time is reported separately from production-equivalent time. Diagnostic artifacts retain fixed-source/tool receipts, resource measurements, command logs, archive counts and listed test identities for seven days.

Compare source/control hashes, binary counts and test identity hashes before comparing time. GitHub job and step timestamps provide restore, setup, actual archive upload, post-save and whole-job durations. The Python phase receipts provide archive command, recompression, doctest and correctness durations plus CPU, RSS and filesystem I/O counters. The full mbx summary records hit, miss, bypass, failure and transfer outcomes. A cache hit rate alone is not a performance result.

A single warm sample establishes feasibility and exposes large differences. It does not support a precise performance claim or an automatic default switch. If correctness fails or the timing difference is small, inspect the retained evidence before proposing one additional reviewed run. The historical 7–10 minute target-cache jobs used an earlier compiler and a somewhat smaller workload, which this experiment controls by fixing one current source revision.

Local checks compile no Rust. An [actionlint](https://github.com/rhysd/actionlint) executable is required. Set `ACTIONLINT` to its verified path when it is not on `PATH`:

```sh
python3 -m unittest discover -s scripts/bench-tests -p test_ci_cache_comparison.py
"${ACTIONLINT:-actionlint}" .github/workflows/ci-cache-comparison.yml
python3 -m py_compile scripts/ci-cache-comparison.py
shellcheck scripts/ci-cache-comparison-tool
shfmt -d -i 2 -ci -sr scripts/ci-cache-comparison-tool
yq -o=json . .github/workflows/ci-cache-comparison.yml > /dev/null
```

The workflow-contract tests use `yq` when available. The semantic tests require actionlint and include a negative control for using the unavailable runner context in job-level environment settings. Runner-derived result paths are exported by a shell step through `GITHUB_ENV`. Python protocol and command-failure checks use only the standard library. The pinned [Swatinem source](https://github.com/Swatinem/rust-cache/tree/d1527c8f67ef9af357290475d4af4b0fab8f12e0), [mbx action source](https://github.com/jdx/mr-boxington-action/blob/v1.7.3/src/index.ts), and [mbx launcher implementation](https://github.com/jdx/mr-boxington/blob/v1.28.0/crates/mbx/src/build_script.rs) define the interoperability assumptions being tested.
