set -u
cd /root/b; chmod +x incr_bench-*
nproc; grep -m1 "model name" /proc/cpuinfo; free -g | head -2
for r in 1 2 3; do
  for v in base tip; do
    echo "=== $v pass round $r"
    timeout 1800 taskset -c 2-7 /lib64/ld-linux-x86-64.so.2 ./incr_bench-$v --ignored --nocapture --test-threads 1 --exact pass_against_recompute 2>&1
  done
done
for r in 1 2 3; do
  echo "=== tip annotation_cost round $r"
  timeout 2400 taskset -c 2-7 /lib64/ld-linux-x86-64.so.2 ./incr_bench-tip --ignored --nocapture --test-threads 1 --exact annotation_cost 2>&1
done
echo REMOTE-DONE
