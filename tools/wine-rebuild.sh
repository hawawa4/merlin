#!/usr/bin/env bash
# Incremental rebuild of our patched Wine after editing its source tree (development helper).
# Usage: tools/wine-rebuild.sh [version] [tag]   (defaults match tools/build-wine.sh)
set -euo pipefail
VERSION="${1:-11.20}"
TAG="${2:-vblank}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/runtime/src/wine-$VERSION-$TAG"
OUT="$ROOT/runtime/wine-$VERSION-$TAG"
# Makefiles are regenerated when a Makefile.in changed (needs a configure re-run via make).
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$SRC:/src" -v "$OUT:/out" -w /src/build \
    merlin-wine-build bash -c 'set -e; [ ../dlls/dcomp/Makefile.in -nt Makefile ] && (cd .. && tools/make_makefiles >/dev/null 2>&1 || true); ./config.status >/dev/null 2>&1 || true; make -j"$(nproc)"; make install >/dev/null'
