#!/usr/bin/env bash
# Run the Gradle build in jvm/ on a native library built in the same managed-target lease
# (`mise run jvm:build`, `jvm:test` and `jvm:sample`). The tasks run this script under
# `mbx sparkles-task`, so collection cannot remove the library or the generated bindings
# between scripts/jvm-native.sh and Gradle.
#
# scripts/jvm-native.sh deletes and regenerates target/jvm/uniffi. The checkout's JVM lock
# is held from that build until Gradle exits, so two JVM tasks at once cannot regenerate
# the bindings while the other's Gradle reads them.
#
# Usage: scripts/jvm-gradle.sh GRADLE_TASK... [GRADLE_ARG...]
set -euo pipefail

cd "$(dirname "$0")/.."
if command -v flock > /dev/null; then
  mkdir -p .artifacts
  exec 8> .artifacts/jvm-native.lock
  flock 8
  export SPARKLES_JVM_LOCKED=1
fi
scripts/jvm-native.sh
cd jvm
# Gradle and its daemon do not need the lock descriptor.
./gradlew --console=plain "$@" 8>&-
