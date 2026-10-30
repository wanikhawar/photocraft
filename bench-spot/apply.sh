#!/usr/bin/env bash
# Usage: apply.sh <tree> <after:0|1>: adds the spot benchmarks to a checkout.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
t="$1"
{ cat "$here/io_head.rs"; [ "$2" = 1 ] && cat "$here/io_after.rs"; echo; cat "$here/gen.rs"; } > "$t/crates/io/tests/it/spot_bench.rs"
echo "mod spot_bench;" >> "$t/crates/io/tests/it/main.rs"
{ cat "$here/ui_head.rs"; echo; cat "$here/gen.rs"; } >> "$t/crates/ui-egui/src/canvas/channel_view/tests.rs"
