set -euo pipefail
export RAYON_NUM_THREADS=4
cat evidence-8822/measure.rs >> crates/algo/src/matting.rs
cat evidence-8822/capture.rs >> crates/ui-egui/src/canvas/select_mask_view.rs
cargo test --release --locked -p photocraft-algo --lib --no-run --message-format=json > "$RUNNER_TEMP/algo-build.json"
cargo test --release --locked -p photocraft-ui-egui --lib --no-run --message-format=json > "$RUNNER_TEMP/ui-build.json"
python3 - <<'PY'
import json, os
from pathlib import Path
for name in ['algo','ui']:
    rows=[json.loads(s) for s in Path(os.environ['RUNNER_TEMP'],f'{name}-build.json').read_text().splitlines() if s.startswith('{')]
    exe=next(r['executable'] for r in reversed(rows) if r.get('executable'))
    Path(os.environ['RUNNER_TEMP'],f'{name}-bin').write_text(exe)
PY
algo_bin=$(cat "$RUNNER_TEMP/algo-bin")
ui_bin=$(cat "$RUNNER_TEMP/ui-bin")
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
EVIDENCE_DIR="$RUNNER_TEMP/after" "$ui_bin" issue_8822_capture --ignored --nocapture --test-threads=1
git fetch --no-tags origin 5d815c5a4bf5da23534a9afe7aea1f38610d8ec4
git show 5d815c5a4bf5da23534a9afe7aea1f38610d8ec4:crates/ui-egui/src/canvas/select_mask_view.rs > "$RUNNER_TEMP/old-view.rs"
python3 - <<'PY'
import os
from pathlib import Path
p=Path('crates/ui-egui/src/canvas/select_mask_view.rs')
s=p.read_text()
old=Path(os.environ['RUNNER_TEMP'],'old-view.rs').read_text()
a=s.index('/// Sparse float coverage'); b=s.index('/// The view parameters',a)
c=old.index('/// A one-channel coverage');d=old.index('/// The view parameters',c)
s=s[:a]+old[c:d]+s[b:]
s=s[:s.index('#[test]\nfn sparse_edge_band_matches_dense_reference')]+Path('evidence-8822/capture.rs').read_text()
p.write_text(s)
PY
cargo test --release --locked -p photocraft-ui-egui --lib --no-run --message-format=json > "$RUNNER_TEMP/ui-before-build.json"
python3 - <<'PY'
import json, os
from pathlib import Path
rows=[json.loads(s) for s in Path(os.environ['RUNNER_TEMP'],'ui-before-build.json').read_text().splitlines() if s.startswith('{')]
Path(os.environ['RUNNER_TEMP'],'before-bin').write_text(next(r['executable'] for r in reversed(rows) if r.get('executable')))
PY
EVIDENCE_DIR="$RUNNER_TEMP/before" "$(cat "$RUNNER_TEMP/before-bin")" issue_8822_capture --ignored --nocapture --test-threads=1
python3 - <<'PY'
import base64,os
from pathlib import Path
root=Path(os.environ['RUNNER_TEMP'])
for side in ['before','after']:
    for p in sorted((root/side).glob('*.png')):
        print('PNG_EVIDENCE '+side+'/'+p.name+' '+base64.b64encode(p.read_bytes()).decode())
PY
