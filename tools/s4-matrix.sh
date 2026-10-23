#!/usr/bin/env bash
# S4 compatibility matrix: load each bridged Windows plugin in the S2 host and check
#  - load + embed time, and whether it crashed;
#  - whether its editor *redraws*: screenshot the plugin window, set a parameter from the
#    host and play a chord, screenshot again, compare pixels. A live editor changes.
# Usage: tools/s4-matrix.sh [bridge.clap ...]   (default: every bridge in test-assets/bridges/clap)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="${S4_OUT:-$ROOT/target/s4-matrix}"
mkdir -p "$OUT"
YAB="${YABRIDGE_DIR:-$ROOT/runtime/yabridge-b580a9f7-wine-11.20-vblank}"
WINE="${WINELOADER:-/opt/wine-staging/bin/wine}"
BIN="$ROOT/target/release/s2-clap-embed"
cargo build -q -p s2-clap-embed --release

plugins=("$@")
[ ${#plugins[@]} -gt 0 ] || mapfile -t plugins < <(ls "$ROOT"/test-assets/bridges/clap/*.clap)

printf "%-22s %-8s %-9s %-9s %-10s %s\n" plugin result load embed redraw notes | tee "$OUT/summary.txt"
for plugin in "${plugins[@]}"; do
    name=$(basename "$plugin" .clap)
    tag=${name// /_}
    log="$OUT/$tag.log"
    S2_SCRIPT="2:param 0 0.1; 9:param 0 0.9; 9:on 48; 9:on 55; 9:on 60; 9:on 64; 14:off 48; 14:off 55; 14:off 60; 14:off 64; 15:quit" \
    YABRIDGE_DEBUG_LEVEL="1+editor" PATH="$YAB:$PATH" WINELOADER="$WINE" \
        timeout 90 "$BIN" "$plugin" > "$log" 2>&1 &
    pid=$!
    for _ in $(seq 1 120); do
        grep -qE "editor embedded|startup failed|error:" "$log" && break
        kill -0 $pid 2>/dev/null || break
        sleep 0.5
    done
    sleep 5   # params at 2 s settle; editor idle
    ww=$(grep -o "wine_window: [0-9]*" "$log" | head -1 | awk '{print $2}')
    [ -n "$ww" ] || ww=$(grep -o "plugin_window: [0-9]*" "$log" | head -1 | awk '{print $2}')
    shot() { [ -n "$ww" ] && timeout 10 import -window "$ww" "$OUT/$tag-$1.png" 2>/dev/null; }
    shot a
    sleep 6   # param change + chord at 9 s
    shot b
    wait $pid; code=$?
    load=$(grep -o "loaded .* in [0-9.]*[a-zµ]*" "$log" | awk '{print $NF}')
    embed=$(grep -o "editor embedded in [0-9.]*[a-zµ]*" "$log" | head -1 | awk '{print $NF}')
    redraw="-"
    if [ -f "$OUT/$tag-a.png" ] && [ -f "$OUT/$tag-b.png" ]; then
        diff=$(compare -metric AE "$OUT/$tag-a.png" "$OUT/$tag-b.png" null: 2>&1 | awk '{print int($1)}')
        if [ "${diff:-0}" -gt 50 ]; then redraw="yes($diff)"; else redraw="NO($diff)"; fi
    fi
    result=ok; [ $code -ne 0 ] && result="exit$code"
    notes=$(grep -oE "error: [^\n]*|startup failed: .*|no params extension|no such parameter" "$log" | head -1)
    printf "%-22s %-8s %-9s %-9s %-10s %s\n" "$name" "$result" "${load:--}" "${embed:--}" "$redraw" "$notes" | tee -a "$OUT/summary.txt"
done
echo "screenshots and logs: $OUT"
