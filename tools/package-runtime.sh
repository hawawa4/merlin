#!/usr/bin/env bash
# Packs the Wine + yabridge pair into the archive the app downloads ("Windows plugin support").
#
#   tools/package-runtime.sh [WINE_DIR] [YABRIDGE_DIR]
#
# Defaults are the builds of `just build-wine` / `just build-yabridge`. Output, in dist/runtime/:
#   <id>.tar.zst           wine/ (stripped) + yabridge/
#   runtime-manifest.json  id, file, sha256, size, unpacked_size
# Publish both files side by side (a GitHub release, any web server); the app is given the URL
# of the manifest and finds the archive next to it. Nothing here uploads anything.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WINE="${1:-$ROOT/runtime/wine-11.20-wow64}"
YAB="${2:-$ROOT/runtime/yabridge-b580a9f7-wine-11.20-wow64}"
OUT="$ROOT/dist/runtime"

[ -x "$WINE/bin/wine" ] || { echo "no Wine at $WINE (run: just build-wine)" >&2; exit 1; }
[ -f "$YAB/libyabridge-chainloader-clap.so" ] || { echo "no yabridge at $YAB (run: just build-yabridge)" >&2; exit 1; }

WINE_VERSION="$(cat "$WINE/VERSION" 2>/dev/null || basename "$WINE" | sed 's/^wine-//')"
YAB_COMMIT="$(cut -c1-8 "$YAB/COMMIT" 2>/dev/null || basename "$YAB")"
ID="wine-${WINE_VERSION}_yabridge-${YAB_COMMIT}"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE" "$OUT"

echo "copying $ID ..."
cp -a "$WINE" "$STAGE/wine"
cp -a "$YAB" "$STAGE/yabridge"

# Debug information is more than half of a Wine build. The Windows-side DLLs keep Wine's
# "builtin" marker when stripped; two of them (rsaenh, bcrypt) cannot be, and stay as they are.
echo "stripping ..."
find "$STAGE/wine/lib/wine" -type f \( -name '*.dll' -o -name '*.exe' -o -name '*.sys' -o -name '*.drv' \
    -o -name '*.ocx' -o -name '*.acm' -o -name '*.cpl' -o -name '*.ax' -o -name '*.tlb' -o -name '*.mui' \) \
    -print0 | xargs -0 -n 50 -P "$(nproc)" llvm-strip --strip-debug 2>/dev/null || true
find "$STAGE/wine/lib" "$STAGE/wine/bin" "$STAGE/yabridge" -type f -print0 \
    | xargs -0 -n 50 -P "$(nproc)" strip --strip-unneeded 2>/dev/null || true
rm -f "$STAGE/yabridge/COMMIT.orig"
# Debug builds of the build tools and headers are not needed to run plugins.
rm -rf "$STAGE/wine/include" "$STAGE/wine/share/man"

UNPACKED=$(du -sb "$STAGE" | cut -f1)
ARCHIVE="$OUT/$ID.tar.zst"
echo "compressing (this takes a minute) ..."
tar -C "$STAGE" -cf - wine yabridge | zstd -19 -T0 -q -f -o "$ARCHIVE"

SHA=$(sha256sum "$ARCHIVE" | cut -d' ' -f1)
SIZE=$(stat -c %s "$ARCHIVE")
python3 - "$OUT/runtime-manifest.json" "$ID" "$(basename "$ARCHIVE")" "$SHA" "$SIZE" "$UNPACKED" <<'PY'
import json, sys
path, id_, file, sha, size, unpacked = sys.argv[1:]
json.dump({"id": id_, "file": file, "sha256": sha, "size": int(size), "unpacked_size": int(unpacked)},
          open(path, "w"), indent=2)
open(path, "a").write("\n")
PY
echo "done: $ARCHIVE ($((SIZE / 1048576)) MB, $((UNPACKED / 1048576)) MB unpacked)"
echo "publish $OUT/runtime-manifest.json and $(basename "$ARCHIVE") together"
