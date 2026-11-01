set -euo pipefail
export RAYON_NUM_THREADS=4
cat evidence-8822/measure.rs >> crates/algo/src/matting.rs
cargo test --release --locked -p photocraft-algo --lib --no-run --message-format=json > "$RUNNER_TEMP/algo-build.json"
python3 - <<'PYTHON'
import json,os
from pathlib import Path
rows=[json.loads(s) for s in Path(os.environ['RUNNER_TEMP'],'algo-build.json').read_text().splitlines() if s.startswith('{')]
Path(os.environ['RUNNER_TEMP'],'algo-bin').write_text(next(r['executable'] for r in reversed(rows) if r.get('executable')))
PYTHON
algo_bin=$(cat "$RUNNER_TEMP/algo-bin")
for rep in 1 2 3; do
  for case in small24 solid24 diagonal24; do
    for impl in dense sparse; do
      REFINE_CASE="$case" REFINE_IMPL="$impl" /usr/bin/time -f 'PEAK_RSS_KIB=%M ELAPSED_S=%e' "$algo_bin" issue_8822_measurement --ignored --nocapture --test-threads=1
    done
  done
  for case in solid100 diagonal100; do
    REFINE_CASE="$case" REFINE_IMPL=sparse /usr/bin/time -f 'PEAK_RSS_KIB=%M ELAPSED_S=%e' "$algo_bin" issue_8822_measurement --ignored --nocapture --test-threads=1
  done
done
