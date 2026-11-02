#!/usr/bin/env bash
# Run the workspace's nextest tests in shares on several machines from one build.
#
# The script builds every test binary once here with `mbx nextest archive`, runs one share
# of the tests here and sends the archive to each remote machine, where cargo-nextest runs
# another share with `--partition KIND:K/N`. Each share's JUnit report, log and exit code
# land in the results directory. At the end the script checks that the shares together ran
# exactly the tests an unpartitioned run would, and exits 0 only when every share passed.
# Doctests cannot be archived, so `mise run test` still runs them.
#
# Usage: scripts/test-partitioned.sh [options] [-- nextest filter arguments]
#   --hosts "A B"    remote machines. An entry is an ssh host, or nsc or nsc:MACHINE for an
#                    ephemeral Namespace instance that the script creates and destroys
#                    (MACHINE as nsc create takes it, default 16x32)
#   --no-local       run no share on this machine
#   --partition K    hash, count or slice (default slice)
#   --transport T    ssh (default) or hc, which starts each ssh host's share as a named hc
#                    terminal that can be watched there. Files always travel over ssh.
#   --busy MODE      skip (default) leaves out an ssh host that is busy or short of disk,
#                    and use runs a share there anyway
#   --out DIR        results directory (default .artifacts/test-partitioned/<UTC time>)
#   --keep           leave each remote machine's run directory in place
#   --reuse DIR      take the archive and test list of an earlier run's results directory
#                    instead of building, for a rerun of the same build on remote machines
#                    only (with --no-local) and with the same nextest arguments
#
# Arguments after -- go to both `nextest list` and `nextest run`, so they are filters such
# as `-E 'binary(w3c)'`, test name patterns or `--run-ignored all`.
#
# The hosts come from --hosts, then TEST_PARTITION_HOSTS, then a TEST_PARTITION_HOSTS line
# in ${SPARKLES_TEST_PARTITION_CONFIG:-~/.config/sparkles/test-partition.env}. With no
# hosts every test runs here in one share. The same file may set TEST_PARTITION_TRANSPORT,
# TEST_PARTITION_MIN_FREE_GB, TEST_PARTITION_NSC_DURATION and TEST_PARTITION_ENV_FILE.
#
# Test binaries keep the absolute paths of the checkout that built them, from
# CARGO_MANIFEST_DIR, CARGO_BIN_EXE_* and CARGO_TARGET_TMPDIR, so every share runs at
# the same absolute path as this checkout. On each remote machine the script creates that
# directory, marks it as a share directory and refuses a machine where the path already
# exists without the mark, so it never writes into another checkout. It copies the tracked
# files, ui/build and the conformance suite directories this checkout's tests read, and it
# extracts the archive's target directory there. A suite directory that already exists on
# the machine is used as it is. A machine needs bash, tar and zstd. A Namespace instance
# gets a C compiler, Python and Node.js from its package manager. On an ssh host the share
# sources TEST_PARTITION_ENV_FILE when that file exists there, uses the checkout's mise
# tools when mise is installed, waits for ~/lanes/.heavy.lock when that file exists, and
# runs under a systemd memory cap when systemd-run --user works.
#
# An ssh host counts as busy when another process holds ~/lanes/.heavy.lock, a gate-* unit
# is active, its load average exceeds half its processors, or it has less than
# TEST_PARTITION_MIN_FREE_GB (default 40) GiB free under its home directory.
set -euo pipefail

main() {
  cd "$(dirname "$0")/.."
  # The run holds one lease on the checkout's managed target from the build to the end of
  # the local share, so that a collection between the steps cannot remove the binaries.
  if [ -z "${SPARKLES_CI_LEASED:-}" ]; then
    SPARKLES_CI_LEASED=1 PATH="$PWD/scripts:$PATH" exec mbx sparkles-task -- "$PWD/scripts/test-partitioned.sh" "$@"
  fi
  root=$(pwd -P)

  config=${SPARKLES_TEST_PARTITION_CONFIG:-$HOME/.config/sparkles/test-partition.env}
  if [ -f "$config" ]; then
    while IFS='=' read -r k v; do
      case $k in
        TEST_PARTITION_HOSTS | TEST_PARTITION_TRANSPORT | TEST_PARTITION_MIN_FREE_GB | TEST_PARTITION_NSC_DURATION | TEST_PARTITION_ENV_FILE)
          [ -n "${!k+x}" ] || export "$k=$v"
          ;;
      esac
    done < <(grep -E '^[A-Z_]+=' "$config")
  fi

  hosts=${TEST_PARTITION_HOSTS:-}
  transport=${TEST_PARTITION_TRANSPORT:-ssh}
  min_free=${TEST_PARTITION_MIN_FREE_GB:-40}
  nsc_duration=${TEST_PARTITION_NSC_DURATION:-2h}
  env_file=${TEST_PARTITION_ENV_FILE:-}
  local_share=1
  kind=slice
  busy=skip
  keep=
  reuse=
  out=
  args=()
  while [ $# -gt 0 ]; do
    case $1 in
      --hosts)
        hosts=${2?--hosts needs a list}
        shift
        ;;
      --no-local) local_share= ;;
      --partition)
        kind=${2:?--partition needs hash, count or slice}
        shift
        ;;
      --transport)
        transport=${2:?--transport needs ssh or hc}
        shift
        ;;
      --busy)
        busy=${2:?--busy needs skip or use}
        shift
        ;;
      --out)
        out=${2:?--out needs a directory}
        shift
        ;;
      --keep) keep=1 ;;
      --reuse)
        reuse=$(realpath "${2:?--reuse needs a results directory}")
        shift
        ;;
      -h | --help)
        sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
        exit 0
        ;;
      --)
        shift
        args=("$@")
        break
        ;;
      *) die "unknown argument $1" ;;
    esac
    shift
  done
  case $kind in hash | count | slice) ;; *) die "bad --partition $kind" ;; esac
  case $transport in ssh | hc) ;; *) die "bad --transport $transport" ;; esac
  case $busy in skip | use) ;; *) die "bad --busy $busy" ;; esac
  case $min_free in '' | *[!0-9]*) die "bad TEST_PARTITION_MIN_FREE_GB $min_free" ;; esac

  case $root in *[!A-Za-z0-9._/-]*) die "the checkout path $root has characters the remote commands do not quote" ;; esac
  out=$(realpath -m "${out:-.artifacts/test-partitioned/$(date -u +%Y%m%dT%H%M%SZ)}")
  mkdir -p "$out"
  log=$out/run.log
  : > "$log"
  start=$SECONDS
  note "checkout $root at $(git rev-parse HEAD), results in $out"

  # ---------------------------------------------------------------- remote machines
  # Each share is one entry of these arrays. Share 1 is this machine when it runs one.
  share_host=()
  share_kind=()
  trap cleanup EXIT
  trap 'exit 130' INT TERM

  for h in $hosts; do
    case $h in
      nsc | nsc:*)
        share_host+=("$h")
        share_kind+=(nsc)
        ;;
      *)
        if [ "$busy" = skip ]; then
          p=$(probe "$h")
          note "host $h: $p"
          case $p in
            idle*) ;;
            *)
              note "host $h is left out ($p)"
              continue
              ;;
          esac
        fi
        share_host+=("$h")
        share_kind+=(ssh)
        ;;
    esac
  done
  n=${#share_host[@]}
  [ -n "$local_share" ] && n=$((n + 1))
  [ "$n" -gt 0 ] || die "no share can run: --no-local and no usable host"

  # ---------------------------------------------------------------- build
  archive=$out/tests.tar.zst
  t0=$SECONDS
  nextest=$(command -v cargo-nextest) || die "cargo-nextest is not on PATH (mise install)"
  if [ -n "$reuse" ]; then
    [ -f "$reuse/tests.tar.zst" ] && [ -f "$reuse/list.json" ] || die "$reuse has no tests.tar.zst and list.json"
    # The coverage check counts the earlier run's selection, so it must be this run's.
    [ -z "$local_share" ] || die "--reuse needs --no-local, since this checkout's build may differ from the archive"
    if [ -f "$reuse/args" ]; then saved=$(cat "$reuse/args"); else saved=; fi
    [ "$saved" = "$(printf '%q ' "${args[@]}")" ] || die "--reuse needs the same nextest arguments as the earlier run (${saved:-none})"
    ln -f "$reuse/tests.tar.zst" "$archive" 2> /dev/null || cp "$reuse/tests.tar.zst" "$archive"
    cp "$reuse/list.json" "$out/list.json"
    note "reusing the archive and test list of $reuse"
  else
    # As in `mise run test`, the server's unit-test binary builds alone first, because it
    # and the server binary each need about 10 GB of rustc memory.
    mbx test --workspace --no-run --bin sparkles > "$out/build.log" 2>&1 || fail_log "the server test build failed" "$out/build.log"
    # The tests an unpartitioned run selects, for the coverage check at the end. The archive
    # comes after the listing, which builds the binaries, since an archive build followed by
    # a listing built some of them again.
    mbx nextest list --workspace --message-format json "${args[@]}" > "$out/list.json" 2>> "$out/build.log" ||
      fail_log "listing the tests failed" "$out/build.log"
    mbx nextest archive --workspace --archive-file "$archive" >> "$out/build.log" 2>&1 || fail_log "mbx nextest archive failed" "$out/build.log"
    note "phase build and archive: $((SECONDS - t0)) s, $(du -h "$archive" | cut -f1)"
  fi
  printf '%q ' "${args[@]}" > "$out/args"
  expected=$(
    python3 - "$out/list.json" << 'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
n = 0
for suite in doc["rust-suites"].values():
    for case in suite["testcases"].values():
        if case["filter-match"]["status"] == "matches":
            n += 1
print(n)
PY
  )
  note "tests selected: $expected in $n shares, partitioned by $kind"

  # ---------------------------------------------------------------- start the shares
  declare -g -A share_pid=()
  k=0
  if [ -n "$local_share" ]; then
    k=1
    mkdir -p "$out/share-1"
    (
      cd "$root"
      set +e
      mbx nextest run --workspace --no-fail-fast --partition "$kind:1/$n" "${args[@]}" > "$out/share-1/share.log" 2>&1
      echo $? > "$out/share-1/exit"
      for f in target/nextest/*/junit.xml; do [ -f "$f" ] && cp "$f" "$out/share-1/junit.xml"; done
    ) &
    share_pid[1]=$!
    echo local > "$out/share-1/host"
    note "share 1/$n: this machine"
  fi
  i=0
  for h in "${share_host[@]}"; do
    k=$((k + 1))
    mkdir -p "$out/share-$k"
    echo "$h" > "$out/share-$k/host"
    run_remote "$k" "$n" "$h" "${share_kind[$i]}" > "$out/share-$k/driver.log" 2>&1 &
    share_pid[$k]=$!
    note "share $k/$n: $h"
    i=$((i + 1))
  done

  # ---------------------------------------------------------------- wait and collect
  rc=0
  for k in $(seq 1 "$n"); do
    wait "${share_pid[$k]}" || true
    code=$(cat "$out/share-$k/exit" 2> /dev/null || echo missing)
    note "share $k/$n on $(cat "$out/share-$k/host") ended: exit $code"
    [ "$code" = 0 ] || rc=1
  done

  python3 - "$out" "$n" "$expected" << 'PY' | tee -a "$log" || rc=1
import sys, xml.etree.ElementTree as ET
from pathlib import Path
out, n, expected = Path(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])
total = failed = 0
slow = []
ok = True
for k in range(1, n + 1):
    d = out / f"share-{k}"
    host = (d / "host").read_text().strip()
    j = d / "junit.xml"
    if not j.exists():
        print(f"share {k} ({host}): no JUnit report, see {d}")
        ok = False
        continue
    root = ET.parse(j).getroot()
    tests = int(root.get("tests", 0))
    fails = int(root.get("failures", 0)) + int(root.get("errors", 0))
    secs = float(root.get("time", 0))
    total += tests
    failed += fails
    for case in root.iter("testcase"):
        slow.append((float(case.get("time", 0)), host, case.get("classname"), case.get("name")))
    print(f"share {k} ({host}): {tests} tests, {fails} failed, {secs:.0f} s")
print(f"all shares: {total} tests run, {failed} failed, {expected} selected")
if total != expected:
    print(f"coverage check failed: the shares ran {total} tests and an unpartitioned run selects {expected}")
    ok = False
for t, host, cls, name in sorted(slow, reverse=True)[:10]:
    print(f"  {t:7.1f}s {host} {cls} {name}")
sys.exit(0 if ok and failed == 0 else 1)
PY
  note "wall $((SECONDS - start)) s"
  echo "$rc" > "$out/exit"
  exit "$rc"
}

note() { echo "$(date -u +%FT%TZ) $*" | tee -a "$log" >&2; }
die() {
  echo "test-partitioned.sh: $*" >&2
  exit 2
}
fail_log() {
  tail -n 40 "$2" >&2
  die "$1, see $2"
}

ssh_opts=(-o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=30)

# "idle" or the reason a host is busy.
probe() {
  # shellcheck disable=SC2016
  ssh "${ssh_opts[@]}" "$1" "min=$min_free"'
    if [ -e "$HOME/lanes/.heavy.lock" ] && ! flock -n "$HOME/lanes/.heavy.lock" true; then echo "busy: heavy lock held"; exit; fi
    u=$(systemctl --user list-units "gate-*" --state=active --plain --no-legend 2> /dev/null | wc -l)
    [ "$u" = 0 ] || { echo "busy: $u gate units"; exit; }
    load=$(cut -d" " -f1 /proc/loadavg); cpus=$(nproc)
    if awk -v l="$load" -v c="$cpus" "BEGIN { exit !(l > c / 2) }"; then echo "busy: load $load on $cpus processors"; exit; fi
    d=$(df -BG --output=avail "$HOME" | tail -n1 | tr -dc 0-9)
    [ "$d" -ge "$min" ] || { echo "busy: $d GiB free, $min needed"; exit; }
    echo "idle: load $load on $cpus processors, $d GiB free"' 2> /dev/null || echo "unreachable"
}

# The suite directories the tests read, as absolute paths that exist here: those named by
# the suite variables, then the default checkouts next to this one.
suite_dirs() {
  local v d
  for v in SPARKLES_W3C_DIR SPARKLES_RDF_TESTS_DIR SPARKLES_SHACL_TESTS SPARKLES_SHEX_TESTS SPARKLES_OXIGRAPH_DIR; do
    d=${!v:-}
    [ -n "$d" ] && [ -d "$d" ] && realpath "$d"
  done
  for d in apache/jena/jena-arq/testing apache/jena/jena-shacl/src/test/files \
    apache/jena/jena-shex/src/test/files oxigraph/oxigraph/testsuite; do
    [ -d "$root/../../$d" ] && realpath "$root/../../$d"
  done
  return 0
}

# The suite variables set here, as KEY=VALUE words for the remote environment.
suite_env() {
  local v
  for v in SPARKLES_W3C_DIR SPARKLES_RDF_TESTS_DIR SPARKLES_SHACL_TESTS SPARKLES_SHEX_TESTS SPARKLES_OXIGRAPH_DIR; do
    [ -n "${!v:-}" ] && printf '%s=%q ' "$v" "${!v}"
  done
  return 0
}

# run_remote K N HOST KIND: copy the share's inputs to HOST, run share K of N there, and
# bring back its report. KIND is ssh or nsc. Writes exit, junit.xml and share.log into
# the share's results directory. Every remote command is one string for the remote shell,
# and the paths in it are this checkout's, which have no spaces (checked in main).
run_remote() {
  local k=$1 n=$2 h=$3 how=$4 id='' machine d runas=
  d=$out/share-$k
  local s=$root/.artifacts/test-share
  if [ "$how" = nsc ]; then
    machine=${h#nsc}
    machine=${machine#:}
    machine=${machine:-16x32}
    if ! nsc create --bare --machine_type "$machine" --duration "$nsc_duration" \
      --purpose "sparkles test share $k/$n" --cidfile "$d/instance.id" > "$d/create.log" 2>&1; then
      [ -s "$d/instance.id" ] && cat "$d/instance.id" >> "$out/nsc-ids"
      echo 97 > "$d/exit"
      return
    fi
    id=$(cat "$d/instance.id")
    echo "$id" >> "$out/nsc-ids"
    echo "nsc instance $id ($machine)" > "$d/host"
    rsh() { nsc ssh -T "$id" -- "$1"; }
    # A bare instance has no C compiler, which the crash tests use, no Python, Node.js or
    # rustfmt, and no patchelf, which the share script uses to give the binaries the
    # instance's loader. Its shell is root's, and tests of permission checks fail as root,
    # so the share runs as the unprivileged user share.
    rsh "apk add -q gcc python-3 nodejs patchelf rustup && adduser -D share" < /dev/null > "$d/packages.log" 2>&1 || true
    runas="su share -c"
  else
    # shellcheck disable=SC2029
    rsh() { ssh "${ssh_opts[@]}" "$h" "$1"; }
  fi

  # The run directory has the checkout's absolute path. Refuse one that is not ours.
  if ! rsh "if [ -e $root ] && [ ! -e $root/.sparkles-test-share ]; then echo '$root exists and is not a test-share directory'; exit 1; fi
    mkdir -p $s && touch $root/.sparkles-test-share && rm -rf $s/* $root/target/nextest" < /dev/null > "$d/prepare.log" 2>&1; then
    echo 96 > "$d/exit"
    return
  fi
  # Tracked files and the built UI, then each suite directory the machine lacks.
  if ! {
    git -C "$root" ls-files -z
    if [ -d "$root/ui/build" ]; then (cd "$root" && find ui/build -type f -print0); fi
  } | tar -C "$root" --null -T - -cf - | rsh "tar -C $root -xf -"; then
    echo "copying the checkout's files failed" >&2
    echo 95 > "$d/exit"
    return
  fi
  local sd
  while read -r sd; do
    if rsh "test -d $sd" < /dev/null 2> /dev/null; then
      echo "suite $sd: present" >> "$d/prepare.log"
    else
      echo "suite $sd: copied" >> "$d/prepare.log"
      tar -C / -cf - "${sd#/}" | rsh "tar -C / -xf -" || {
        echo "copying $sd failed" >&2
        echo 95 > "$d/exit"
        return
      }
    fi
  done < <(suite_dirs)
  # The archive is extracted as it arrives, so the machine never stores it whole. nextest
  # writes GNU tar entries that BusyBox tar refuses, so a machine without GNU tar, such as a
  # bare Namespace instance, extracts it with Python.
  local untar="if tar --version 2> /dev/null | grep -q GNU; then tar -C $root -xf -; else python3 -c 'import sys, tarfile; t = tarfile.open(fileobj=sys.stdin.buffer, mode=\"r|\"); t.extractall(sys.argv[1], filter=\"tar\") if hasattr(tarfile, \"tar_filter\") else t.extractall(sys.argv[1])' $root; fi"
  if ! rsh "zstd -dcq | { $untar; }" < "$archive" || ! rsh "cat > $s/cargo-nextest && chmod +x $s/cargo-nextest" < "$nextest"; then
    echo "copying the archive or cargo-nextest failed" >&2
    echo 95 > "$d/exit"
    return
  fi

  # The share script, started detached so that it outlives a dropped connection.
  local extra
  extra=$(printf '%q ' "${args[@]}")
  rsh "cat > $s/run.sh" << EOF
#!/usr/bin/env bash
set -uo pipefail
cd $root
env_file=$(printf '%q' "$env_file")
if [ -n "\$env_file" ] && [ -f "\$env_file" ]; then set -a; . "\$env_file"; set +a; fi
if command -v mise > /dev/null; then
  mise trust -q . > /dev/null 2>&1
  mise install -q > /dev/null 2>&1
  eval "\$(mise env -s bash 2> /dev/null)"
fi
lock=()
[ -e "\$HOME/lanes/.heavy.lock" ] && lock=(flock "\$HOME/lanes/.heavy.lock")
cap=()
if systemd-run --user --scope -q true 2> /dev/null; then
  mem=\$(awk '\$1 == "MemTotal:" { m = int(\$2 / 1048576) - 2; if (m > 24) m = 24; print m }' /proc/meminfo)
  cap=(systemd-run --user --scope -q -p MemoryMax=\${mem}G -p MemorySwapMax=2G -p OOMPolicy=continue --)
fi
# A binary built against a loader that this machine lacks, such as one in a Nix store,
# gets the machine's own loader, as scripts/nsc-bench.sh runs its servers.
if command -v patchelf > /dev/null; then
  find target/debug -maxdepth 2 -type f -perm -u+x | while read -r f; do
    i=\$(patchelf --print-interpreter "\$f" 2> /dev/null) || continue
    # some binaries arrive read-only, such as target/debug/sparkles
    [ -e "\$i" ] || { chmod u+w "\$f" && patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 "\$f"; }
  done
fi
# A test runs rustfmt. A machine without it gets the pinned toolchain's through rustup.
if ! rustfmt --version > /dev/null 2>&1 && command -v rustup > /dev/null; then
  channel=\$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
  rustup toolchain install "\$channel" --profile minimal -c rustfmt > /dev/null 2>&1
  PATH=\$(dirname "\$(rustup which rustfmt --toolchain "\$channel")"):\$PATH
fi
# Tests take their temporary directories from TMPDIR, and some, such as the spill default,
# behave differently on a file system in memory. Where /tmp is one, they use the disk.
if [ "\$(stat -f -c %T /tmp 2> /dev/null)" = tmpfs ]; then
  export TMPDIR=$root/target/share-tmp
  mkdir -p "\$TMPDIR"
fi
date +%s > $s/started
env $(suite_env)"\${lock[@]}" "\${cap[@]}" $s/cargo-nextest nextest run \\
  --binaries-metadata $root/target/nextest/binaries-metadata.json \\
  --cargo-metadata $root/target/nextest/cargo-metadata.json \\
  --workspace-remap $root --target-dir-remap $root/target --no-fail-fast --partition $kind:$k/$n $extra > $s/share.log 2>&1
echo \$? > $s/exit.tmp
mv $s/exit.tmp $s/exit
EOF
  local addr=
  if [ "$how" = ssh ] && [ "$transport" = hc ]; then
    addr=$(hc run --bg --on "$h" --json --no-parent --cwd "$root" --name "test-share-$k-of-$n" \
      -- bash "$s/run.sh" | python3 -c 'import json, sys; print(json.load(sys.stdin)["address"])') || addr=
    [ -n "$addr" ] && echo "$addr" > "$d/hc-address"
  fi
  if [ -z "$addr" ]; then
    if [ -n "$runas" ]; then
      rsh "chown -R share $root && nohup setsid $runas 'bash $s/run.sh' > /dev/null 2>&1 < /dev/null &" < /dev/null
    else
      rsh "nohup setsid bash $s/run.sh > /dev/null 2>&1 < /dev/null &" < /dev/null
    fi
  fi
  # Poll for the exit file. A lost connection only delays the collection.
  until rsh "test -e $s/exit" < /dev/null 2> /dev/null; do sleep 20; done
  rsh "cd $root && tar -cf - .artifacts/test-share/exit .artifacts/test-share/share.log \$(ls target/nextest/*/junit.xml 2> /dev/null)" < /dev/null |
    tar -C "$d" -xf -
  mv "$d/.artifacts/test-share/exit" "$d/exit"
  mv "$d/.artifacts/test-share/share.log" "$d/share.log"
  for f in "$d"/target/nextest/*/junit.xml; do [ -f "$f" ] && mv "$f" "$d/junit.xml"; done
  rm -rf "$d/.artifacts" "$d/target"
  if [ -n "$addr" ]; then hc rm "$addr" > /dev/null 2>&1 || true; fi
  if [ -z "$keep" ] && [ "$how" = ssh ]; then
    rsh "test -e $root/.sparkles-test-share && rm -rf $root" < /dev/null || true
  fi
}

# Stop local shares and destroy every Namespace instance this run created.
# shellcheck disable=SC2329
cleanup() {
  local pid id
  for pid in $(jobs -p); do kill "$pid" 2> /dev/null || true; done
  if [ -f "${out:-/nonexistent}/nsc-ids" ]; then
    while read -r id; do
      [ -n "$id" ] && nsc destroy --force "$id" > /dev/null 2>&1 || echo "test-partitioned.sh: destroying $id failed; run nsc destroy $id" >&2
    done < "$out/nsc-ids"
    rm -f "$out/nsc-ids"
  fi
}

main "$@"
