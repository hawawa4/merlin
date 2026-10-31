#!/usr/bin/env bash
# Builds Wine Staging at a pinned version, plus our patches (build-support/wine-patches/),
# in Docker. WoW64 build (i386 + x86_64): 64-bit plugins run natively, and 32-bit programs (most
# plugin installers are 32-bit) start through Wine's WoW64 layer. A 64-bit-only build cannot run
# an installer at all ("failed to load syswow64\ntdll.dll"). 32-bit *plugins* are still not
# bridged (yabridge's bitbridge is off). Output: runtime/wine-<version>-<tag>/ (relocatable:
# bin/wine).
set -euo pipefail

VERSION="${1:-11.20}"
TAG="${2:-wow64}"
IMAGE="merlin-wine-build"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DL="$ROOT/runtime/src/wine-dl"
SRC="$ROOT/runtime/src/wine-$VERSION-$TAG"
OUT="$ROOT/runtime/wine-$VERSION-$TAG"

if [ -x "$OUT/bin/wine" ]; then
    echo "already built: $OUT"
    exit 0
fi

mkdir -p "$DL"
[ -f "$DL/wine-$VERSION.tar.xz" ] || curl -fL -o "$DL/wine-$VERSION.tar.xz" \
    "https://dl.winehq.org/wine/source/${VERSION%%.*}.x/wine-$VERSION.tar.xz"
[ -f "$DL/wine-staging-$VERSION.tar.gz" ] || curl -fL -o "$DL/wine-staging-$VERSION.tar.gz" \
    "https://github.com/wine-staging/wine-staging/archive/refs/tags/v$VERSION.tar.gz"

# Fresh source tree: Wine, then Staging's patchset, then ours.
rm -rf "$SRC" && mkdir -p "$SRC" "$DL/staging-$VERSION"
tar -xf "$DL/wine-$VERSION.tar.xz" -C "$SRC" --strip-components=1
tar -xzf "$DL/wine-staging-$VERSION.tar.gz" -C "$DL/staging-$VERSION" --strip-components=1
python3 "$DL/staging-$VERSION/staging/patchinstall.py" --destdir="$SRC" --all
for p in "$ROOT"/build-support/wine-patches/*.patch; do
    echo "applying $(basename "$p")"
    patch -d "$SRC" -p1 < "$p"
done

docker build -q -t "$IMAGE" "$ROOT/build-support/wine"
rm -rf "$OUT" && mkdir -p "$OUT"   # make it ourselves, or Docker creates it root-owned
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$SRC:/src" -v "$OUT:/out" -w /src "$IMAGE" bash -c '
    set -e
    mkdir -p build && cd build
    ../configure --prefix=/out --enable-archs=i386,x86_64 --disable-tests
    make -j"$(nproc)"
    make install'
echo "$VERSION-$TAG" > "$OUT/VERSION"
echo "built: $OUT"
"$OUT/bin/wine" --version
