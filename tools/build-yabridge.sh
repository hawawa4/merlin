#!/usr/bin/env bash
# Builds the vendored yabridge (vendor/yabridge, with our patches already applied) against our own
# patched Wine (tools/build-wine.sh), in Docker. 64-bit only, no bitbridge.
# Usage: tools/build-yabridge.sh [wine-dir-name]    (default: wine-11.20-vblank, under runtime/)
# Output: runtime/yabridge-<upstream commit>-<wine-dir-name>/ with the plugin libraries and the
# Wine host binaries. The meson subprojects (asio, bitsery, clap, ...) are downloaded into
# vendor/yabridge/subprojects/ (git-ignored there); the first build needs network.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WINE_NAME="${1:-wine-11.20-vblank}"
WINE="$ROOT/runtime/$WINE_NAME"
SRC="$ROOT/vendor/yabridge"
COMMIT="$(cat "$SRC/UPSTREAM_COMMIT")"
IMAGE="merlin-yabridge-build"
OUT="$ROOT/runtime/yabridge-$COMMIT-$WINE_NAME"
BUILD="$ROOT/runtime/src/yabridge-build-$WINE_NAME"

[ -x "$WINE/bin/winegcc" ] || { echo "no Wine at $WINE (run: just wine-build)" >&2; exit 1; }
docker build -q -t "$IMAGE" "$ROOT/build-support/yabridge" >/dev/null
mkdir -p "$BUILD"   # kept between runs: ninja only rebuilds what changed

# Wine was configured with --prefix=/out, so it is mounted at /out.
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -e PATH="/out/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -e WINEPREFIX=/tmp/prefix -e WINEDEBUG=-all \
    -v "$SRC:/src" -v "$BUILD:/build" -v "$WINE:/out:ro" -w /src "$IMAGE" bash -c '
        set -e
        wine --version
        [ -f /build/build.ninja ] || meson setup /build --buildtype=release --cross-file=cross-wine.conf \
            --unity=on --unity-size=10000 -Dbitbridge=false
        ninja -C /build
        strip /build/libyabridge{,-chainloader}-{clap,vst2,vst3}.so /build/yabridge-host.exe.so'

mkdir -p "$OUT"
cp "$BUILD"/libyabridge{,-chainloader}-{clap,vst2,vst3}.so \
   "$BUILD"/yabridge-host.exe{,.so} "$OUT/"
echo "$COMMIT" > "$OUT/COMMIT"
cat "$WINE/VERSION" > "$OUT/WINE"
echo "built: $OUT"
ls -la "$OUT"
