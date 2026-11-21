#!/usr/bin/env bash
# full3/run.sh: three-arm full-set check for row 1267 on forge. Under ~/lanes/.bench.lock and
# ~/lanes/.heavy.lock, runs the matrix harness (bench-executors.sh of src-8baa5686, unmodified)
# at 10.5M (people-1000000) with 12 threads and 1 round per invocation, six invocations in the
# palindrome b48 b49 b50 b50 b49 b48, so each arm gets 2 rounds (8 server epochs, 4 eager and
# 4 streaming) on one shared store per invocation, with the matrix's ORDER_DIGEST and WARM_DIGEST.
# Host state is snapshotted around each invocation and sampled every 15 s.
# Last line of run.log: FULL3-DONE ok, or FULL3-FAILED <reason>.
set -uo pipefail
L=$HOME/lanes
W=$L/matrix-b49-regress/full3
S=$L/forge-matrix-b50/src-8baa5686
DATA=$L/forge-matrix-b50/data
exec 8> "$L/.bench.lock"; flock 8
exec 9> "$L/.heavy.lock"; flock 9
echo "FULL3 locks held $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
for p in rustc cargo mbx java qlever-server oxigraph sparkles; do
  if pgrep -x $p > /dev/null; then ps -eo pid,args --sort=-pcpu | head -8 | cut -c1-150; echo "FULL3-FAILED busy $p"; exit 1; fi
done
mkdir -p "$W/bin"
declare -A SRC=([6dbf2a08]=$L/forge-matrix-b48/bin [b47b582b]=$L/forge-matrix-b49/bin [0779d666]=$L/forge-matrix-b50/bin)
for rev in 6dbf2a08 b47b582b 0779d666; do
  b=$W/bin/sparkles-$rev-matrix
  zstd -q -d -f "${SRC[$rev]}/$rev-matrix.zst" -o "$b" && chmod +x "$b" || { echo "FULL3-FAILED unpack $rev"; exit 1; }
  sha=$(sha256sum "$b" | cut -d' ' -f1)
  [ "$sha" = "$(cat "${SRC[$rev]}/$rev-matrix.sha256")" ] || { echo "FULL3-FAILED $rev sha256 $sha"; exit 1; }
  echo "binary $rev sha256 $sha"
done

snap() { # <label>
  echo "--- host $1 $(date -u +%FT%TZ)"
  df -h / | tail -1
  grep -E '^(MemFree|MemAvailable|Cached|Dirty|Writeback|SwapFree):' /proc/meminfo | tr -s ' ' | paste -sd' '
  for f in cpu io memory; do echo "psi $f: $(paste -sd' ' /proc/pressure/$f)"; done
  echo "governor $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor) no_turbo $(cat /sys/devices/system/cpu/intel_pstate/no_turbo) max_perf_pct $(cat /sys/devices/system/cpu/intel_pstate/max_perf_pct) min_perf_pct $(cat /sys/devices/system/cpu/intel_pstate/min_perf_pct) hwp_boost $(cat /sys/devices/system/cpu/intel_pstate/hwp_dynamic_boost)"
  echo "cur_freq 0-11 kHz: $(for c in $(seq 0 11); do cat /sys/devices/system/cpu/cpu$c/cpufreq/scaling_cur_freq; done | paste -sd' ')"
  echo "throttle core/pkg counts: $(cat /sys/devices/system/cpu/cpu*/thermal_throttle/core_throttle_count 2>/dev/null | awk '{s+=$1} END{print s+0}') $(cat /sys/devices/system/cpu/cpu0/thermal_throttle/package_throttle_count 2>/dev/null)"
  echo "rapl: $(for f in /sys/class/powercap/intel-rapl:0/constraint_*_power_limit_uw; do echo -n "$(basename $f)=$(cat $f 2>/dev/null) "; done)"
  echo "temps: $(for z in /sys/class/thermal/thermal_zone*; do echo -n "$(cat $z/type)=$(cat $z/temp) "; done)"
  echo "nvme: ps_max_latency_us $(cat /sys/module/nvme_core/parameters/default_ps_max_latency_us) aspm $(cat /sys/module/pcie_aspm/parameters/policy) pm $(cat /sys/class/nvme/nvme0/power/control) sched $(cat /sys/block/nvme0n1/queue/scheduler)"
  echo "nvme stat: $(cat /sys/block/nvme0n1/stat | tr -s ' ')"
  echo "load $(cat /proc/loadavg)"
}
snap start

# sampler, on CPU 19 (an efficiency core away from server 0-11 and client 12), at nice 19
( taskset -cp 19 $BASHPID > /dev/null; renice -n 19 $BASHPID > /dev/null
  printf 'utc\tload1\tfree_mib\tcached_mib\tdirty_kib\twriteback_kib\tpsi_io_some10\tpsi_cpu_some10\tmhz_avg_0_11\tpkg_temp_mC\tthrottle_core\tnvme_rd_ios\tnvme_rd_ticks\tspill_kib\n'
  while :; do
    m=$(awk '/^MemFree:/{f=$2} /^Cached:/{c=$2} /^Dirty:/{d=$2} /^Writeback:/{w=$2} END{printf "%d\t%d\t%d\t%d", f/1024, c/1024, d, w}' /proc/meminfo)
    pio=$(sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' /proc/pressure/io); pcpu=$(sed -n 's/^some avg10=\([0-9.]*\).*/\1/p' /proc/pressure/cpu)
    mhz=$(for c in $(seq 0 11); do cat /sys/devices/system/cpu/cpu$c/cpufreq/scaling_cur_freq; done | awk '{s+=$1} END{printf "%d", s/NR/1000}')
    tp=$(for z in /sys/class/thermal/thermal_zone*; do [ "$(cat $z/type)" = x86_pkg_temp ] && cat $z/temp; done | head -1)
    th=$(cat /sys/devices/system/cpu/cpu*/thermal_throttle/core_throttle_count 2>/dev/null | awk '{s+=$1} END{print s+0}')
    read -r rio _ _ rt _ < /sys/block/nvme0n1/stat
    sp=$(du -sk "$W"/inv-*/people-*/srv/.spill 2>/dev/null | awk '{s+=$1} END{print s+0}')
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%H:%M:%S)" "$(cut -d' ' -f1 /proc/loadavg)" "$m" "$pio" "$pcpu" "$mhz" "${tp:--}" "$th" "$rio $rt" "$sp" | tr ' ' '\t'
    sleep 15
  done ) > "$W/host-samples.tsv" 2>&1 &
SAMPLER=$!
trap 'kill $SAMPLER 2>/dev/null' EXIT

i=0; failed=""
for rev in 6dbf2a08 b47b582b 0779d666 0779d666 b47b582b 6dbf2a08; do
  i=$((i + 1)); out=$W/inv-$i-$rev
  echo "invocation $i $rev start $(date -u +%FT%TZ) load $(cat /proc/loadavg)"
  env -i PATH="$PATH" HOME="$HOME" LANG="${LANG:-C.UTF-8}" \
    bash "$S/scripts/bench-executors.sh" --out "$out" --commit "$rev" --data-dir "$DATA" \
    --scales 1000000 --threads 12 --rounds 1 \
    --build-config "$(cat "${SRC[$rev]}/$rev-matrix.build-config.json")" "$W/bin/sparkles-$rev-matrix" \
    > "$W/inv-$i-$rev.log" 2>&1
  rc=$?
  done_line=$(cat "$out/people-1000000-threads-12/results/done" 2>/dev/null || echo missing)
  echo "invocation $i $rev end $(date -u +%FT%TZ) exit $rc done '$done_line'"
  case $done_line in ok*) ;; *) failed="$failed $i-$rev"; ;; esac
  rm -rf "$out/people-1000000-threads-12/stores" "$out/people-1000000-threads-12/srv" "$out/people-1000000-threads-12/data.nt" \
    "$out/people-1000000-threads-12"/bin-*.zst "$out/people-1000000-threads-12/A" "$out/people-1000000-threads-12/B"
  snap "after-$i-$rev"
done
python3 - "$W" > "$W/summary.txt" 2>&1 <<'PY' || echo "summary step failed, see summary.txt"
# per mode and query: each arm's per-epoch medians (in run order) and median of epoch medians,
# then each arm over 6dbf2a08
import sys, glob, os, statistics as st, collections
W = sys.argv[1]
d = collections.defaultdict(list)   # (rev, mode, query, inv, epoch) -> exec_ms
for f in sorted(glob.glob(f"{W}/inv-*/people-1000000-threads-12/results/samples.tsv")):
    inv = f.split('/inv-')[1].split('/')[0]
    rev = inv.split('-', 1)[1]
    for k, line in enumerate(open(f)):
        if k == 0: continue
        r = line.rstrip('\n').split('\t')
        if r[5] == 'error': continue
        mode = 'eager' if r[2] == 'A' else 'streaming'
        d[(rev, mode, r[3], inv, int(r[1]))].append(float(r[5]))
revs = ['6dbf2a08', 'b47b582b', '0779d666']
for mode in ['eager', 'streaming']:
    print(f"== {mode} (exec ms, epoch medians in run order | median; ratio to 6dbf2a08)")
    for q in sorted({k[2] for k in d}):
        parts, meds = [], {}
        for rev in revs:
            eps = sorted(((k[3], k[4]), st.median(v)) for k, v in d.items() if k[0] == rev and k[1] == mode and k[2] == q)
            if not eps: continue
            meds[rev] = st.median([m for _, m in eps])
            parts.append(f"{rev[:4]} " + ' '.join(f"{m:.1f}" for _, m in eps) + f" | {meds[rev]:.1f}")
        base = meds.get('6dbf2a08')
        rat = ' '.join(f"{r[:4]}/6dbf {meds[r]/base:.3f}" for r in revs[1:] if base and r in meds)
        print(f"  {q:20} " + ' ; '.join(parts) + f" ; {rat}")
PY
if [ -z "$failed" ]; then echo "FULL3-DONE ok"; else echo "FULL3-FAILED$failed"; exit 1; fi
