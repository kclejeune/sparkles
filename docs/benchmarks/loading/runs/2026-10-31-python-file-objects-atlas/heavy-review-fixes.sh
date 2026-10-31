#!/usr/bin/env bash
# Build both wheels, make the inputs and run the A/B under the shared heavy-job lock.
set -x
cd ~/lanes/python-reader-438
for side in main lane; do
  [ -d venv-$side ] && continue
  mkdir -p $side && zstd -dcq $side.tar.zst | tar -x -C $side
  (cd $side && mise trust -q . && mise exec github:PyO3/maturin@1.15.0 -- mbx sparkles-task -- maturin build --release --locked --manifest-path crates/sparkles-py/Cargo.toml --target-dir target --out ../wheels-$side) || exit 1
  python3 -m venv venv-$side && venv-$side/bin/pip install -q wheels-$side/*.whl || exit 1
done
mkdir -p data
python3 gen.py 5000000 data/data.nt
for n in 1 20000 80000 120000; do head -$n data/data.nt > data/$n.nt; done
mv data/1.nt data/one.nt; mv data/20000.nt data/20k.nt; mv data/80000.nt data/80k.nt; mv data/120000.nt data/120k.nt
gzip -k -1 data/data.nt
venv-lane/bin/python -c "
import sparkles
ds = sparkles.Dataset('conv')
ds.load(path='data/data.nt')
ds.dump('data/data.ttl', format='turtle')
"
rm -rf conv
uptime
ROUNDS=5 ./ab.sh
uptime
