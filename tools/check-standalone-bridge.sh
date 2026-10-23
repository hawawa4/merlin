#!/usr/bin/env bash
# Checks that yabridge bridges work the way another DAW would use them: a symlinked chainloader, a
# clean environment (no PATH entry, WINELOADER or WINEPREFIX), and a throwaway HOME so Wine cannot
# touch a real ~/.wine. Loads one Windows CLAP through target/release/merlin-scan and
# prints PASS/FAIL for each case.
#
#   tools/check-standalone-bridge.sh [plugin.clap]     (default: Surge XT in test-assets' prefix)
#
# Needs: just build (cargo build --release -p merlin), `just wine-build`, `just yabridge-build`,
# and a prefix with the plugin installed (the default is test-assets/wine/prefix-vblank).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WINE="${WINE_DIR:-$ROOT/runtime/wine-11.20-vblank}"
YAB="${YABRIDGE_DIR:-$ROOT/runtime/yabridge-b580a9f7-wine-11.20-vblank}"
PREFIX="${PREFIX:-$ROOT/test-assets/wine/prefix-vblank}"
DLL="${1:-$PREFIX/drive_c/Program Files/Common Files/CLAP/Surge XT.clap}"
SCAN="$ROOT/target/release/merlin-scan"
[ -x "$SCAN" ] || { echo "build the scanner first: cargo build --release -p merlin" >&2; exit 1; }
[ -f "$DLL" ] || { echo "no plugin at $DLL" >&2; exit 1; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/runtime" "$T/bridges" "$T/home" "$T/outside"
# The runtime layout the app downloads: yabridge/ and wine/ side by side.
cp -r "$YAB" "$T/runtime/yabridge"
ln -s "$WINE" "$T/runtime/wine"

bridge() { # bridge <dir> <name> <dll>
    mkdir -p "$1"
    ln -s "$T/runtime/yabridge/libyabridge-chainloader-clap.so" "$1/$2.clap"
    ln -s "$3" "$1/$2.clap-win"
}
scan() { # scan <bridge .clap>
    env -i HOME="$T/home" DISPLAY="${DISPLAY:-:0}" XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
        PATH=/usr/bin:/bin timeout 120 "$SCAN" "$1" --windows 2>&1 || true
}
fail=0
check() { # check <label> <output> <expect-no-~/.wine: 0|1>
    if grep -q "MERLIN-SCAN-RESULT \[{" <<<"$2"; then
        if [ "$3" = 1 ] && [ -e "$T/home/.wine" ]; then echo "FAIL  $1 (it created ~/.wine)"; fail=1
        else echo "PASS  $1"; fi
    else
        echo "FAIL  $1"; tail -5 <<<"$2" | cut -c1-200; fail=1
    fi
}

# 1. The plugin is inside a prefix: yabridge finds the prefix from the plugin's own path.
bridge "$T/bridges" plugin "$DLL"
check "bridge in a clean environment (prefix found from the plugin's path)" "$(scan "$T/bridges/plugin.clap")" 0

# 2. The plugin is outside every prefix: the runtime's default-wineprefix names the prefix.
cp "$DLL" "$T/outside/plugin.clap"
bridge "$T/bridges2" plugin "$T/outside/plugin.clap"
echo "$PREFIX" > "$T/runtime/yabridge/default-wineprefix"
rm -rf "$T/home/.wine"
check "plugin outside any prefix, default-wineprefix set" "$(scan "$T/bridges2/plugin.clap")" 1
exit $fail
