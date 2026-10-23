# Wine and Windows plugins: where we stand

*Written 2026-10-23. Plan context: [plan.md](plan.md) §4.8 (Windows plugins) and D6; research: [research.md](research.md) §3.2.*

## In one paragraph

Windows plugins run in **our own patched Wine 11.20 (Staging)**, bridged by a **pinned, lightly patched yabridge** (master commit `b580a9f7`, not a release). Both are built by us in Docker, packed into one archive and downloaded by the app on demand; the user's system Wine is never used or touched. The Wine patches are what make most commercial plugins usable (JUCE editors freeze without them). The yabridge patch only keeps the app alive when a Windows plugin crashes. Nothing here depends on a yabridge *release* or on the user having a particular Wine.

## What we ship

| Piece | Version | Where it comes from | Our changes |
|---|---|---|---|
| Wine | 11.20 + Wine Staging 11.20, 64-bit only (`--enable-archs=x86_64`) | `tools/build-wine.sh` (Docker, Ubuntu 24.04, `build-support/wine/Dockerfile`) → `runtime/wine-11.20-vblank/` | 3 patches, [build-support/wine-patches/](../build-support/wine-patches/) |
| yabridge | master `b580a9f7` (2026-08-02), vendored in [vendor/yabridge/](../vendor/yabridge/), `-Dbitbridge=false` | `tools/build-yabridge.sh` (Docker, Ubuntu 24.04, `build-support/yabridge/Dockerfile`, compiled with our own Wine's `winegcc`) → `runtime/yabridge-b580a9f7-wine-11.20-vblank/` | 1 patch, listed in [vendor/yabridge/PATCHES.md](../vendor/yabridge/PATCHES.md) |
| Prefix | created by `wineboot -u`, `WINEDLLOVERRIDES=mscoree,mshtml=d` (no Mono/Gecko dialogs) | `crates/wine` | none |
| Package | `wine-11.20-vblank_yabridge-b580a9f7.tar.zst`, about 40 MB, with a SHA-256 manifest | `tools/package-runtime.sh`, fetched by `crates/wine/src/runtime.rs` | none |

How the app uses it: `crates/wine` writes yabridge *chainloaders* (`Name.clap` + a `Name.clap-win` link to the real DLL) and runs the installer and `wineboot` in our prefix. The app sets `WINELOADER` and puts the yabridge folder first on `PATH` when it starts, so only our process uses this Wine. We do not use `yabridgectl` at all; `crates/wine` reimplements its sync.

## Why yabridge is pinned to master

The last tagged release (5.1.1, late 2024) only works up to Wine 9.21. Wine 9.22 and later changed window handling in a way that breaks it. Master has the fix (a new editor-embedding path by CodeWeavers' rbernon and Asahi Lina's work, merged to master in 2026 and never released). We run master against Wine 11.20 and the compatibility matrix ([compat-matrix.md](compat-matrix.md)) passes for every Windows plugin we tried except Cardinal and an intermittent Six Sines.

Upstream has had no commits since 2026-08-02 and no release since 5.1.1. We treat our copy as a fork we maintain.

## The Wine problem we solved

JUCE (the most common plugin framework) repaints only when `IDXGIOutput::WaitForVBlank()` returns `S_OK`. Wine's version returned `E_NOTIMPL`, so a JUCE editor draws its first frame and never again. Input still works, which makes the editor look dead. Surge XT, Six Sines and OB-Xf all hit this. u-he plugins do not (not JUCE).

DXVK in the prefix is a workaround, not a fix: it repaints Surge and Six Sines but turns OB-Xf's editor white. So we patch Wine instead. JUCE 8's Direct2D renderer then needed two more patches (below).

## The patches, and what upstream could take

All three apply cleanly to a **vanilla Wine 11.20** tree (checked 2026-10-23, `patch --dry-run` on `dlls/dxgi` and `dlls/dcomp` from the release tarball), not only on top of Staging. They are in `diff -ruN` form and would need turning into `git format-patch` commits with tests before submission. Nothing has been submitted yet.

| # | Patch | Size | Upstream chance | What is missing before submitting |
|---|---|---|---|---|
| 0001 | `dxgi`: `IDXGIOutput::WaitForVBlank` sleeps to the next multiple of the refresh period (QPC + the output's refresh rate, 60 if unknown) and returns `S_OK` | about 30 lines, one file (`dlls/dxgi/output.c`) | **High.** It is a "semi-stub", and the same file's neighbour `wined3d_output_get_raster_status()` already fakes vblank the same way. DXVK's `WaitForVBlank` also estimates. | A test in `dlls/dxgi/tests/dxgi.c` (returns `S_OK`, returns within about two refresh periods, repeatedly). Upstream may prefer the layered version: implement `NtGdiDdDDIWaitForVerticalBlankEvent` in `win32u` and call it through `D3DKMTWaitForVerticalBlankEvent`; our patch is the smallest useful step. |
| 0002 | `dxgi`: `IDXGIFactory2::CreateSwapChainForComposition` for D3D11 devices. The swapchain lives on a hidden window, presents to the real HWND once `dcomp` attaches it, and keeps a shadow texture because JUCE repaints only dirty rectangles and expects the compositor to keep the rest | about 157 lines, 3 files | **Medium.** Real functionality, but the shadow-texture composition is a design a Wine maintainer may want done differently, and it is tied to 0003. | Tests, and a design discussion on wine-devel. Probably submitted together with 0003. |
| 0003 | `dcomp`: a minimal DirectComposition device, target and visual tree for HWND targets. `Commit()` attaches each swapchain used as visual content to its target's window. No transforms, clips, effects or surfaces | about 434 lines, `dlls/dcomp/device.c` | **Medium to low as written.** It is deliberately minimal and Wine's dcomp is a stub today, so any real implementation would be welcome, but reviewers may ask for more of the API or a different structure. | Tests in `dlls/dcomp/tests/`, and agreement on how much of the API to implement. |

Suggested upstream order: 0001 alone first (small, with a test, and it unfreezes every JUCE editor on its own), then 0002 and 0003 as one series for JUCE 8's Direct2D renderer (OB-Xf and others on current JUCE). Repro for the description: any JUCE 7.0.6 or later Windows plugin or app (Surge XT 1.3.4) shows a frozen UI under stock Wine.

## The yabridge patch

Already applied in `vendor/yabridge` (about 45 lines, `src/common/communication/common.h`; the first git commit of that folder is upstream unmodified, so `git log -p vendor/yabridge` shows it): when a socket to the Wine host fails, call a hook the host application can register (`yabridge_socket_failed_hook`) before the C++ exception propagates. Without it, a crashing Windows plugin makes the next call across yabridge either block forever or throw a C++ exception through our JACK callback, which aborts a Rust process. The app registers the hook, quarantines the plugin, kills the other Wine hosts and restarts from the autosave (`app/src/guard.rs`). Probably not acceptable upstream as is (it is a global hook for one kind of host), but a general "error callback" might be.

## Bridges that work without the app

The yabridge fork finds its own runtime, so a bridge also works when a DAW loads it with no environment set up (`vendor/yabridge/PATCHES.md`, items 2 and 3). A runtime folder holds `yabridge/` and `wine/` side by side. A bridge that is a **symlink** to `yabridge/libyabridge-chainloader-*.so` finds the libraries, `yabridge-host.exe` and `wine/bin/wine` from there; a *copied* chainloader (what the app writes today) still relies on `PATH` and `WINELOADER`, as before. A plugin inside a Wine prefix uses that prefix; one outside every prefix uses the prefix named on the first line of `yabridge/default-wineprefix`, if that file exists, and otherwise `~/.wine` like stock yabridge. `tools/check-standalone-bridge.sh` checks both cases in a clean environment (no `PATH` entry, no `WINELOADER`, a throwaway `HOME`).

## Known limits

- **64-bit plugins only.** The bitbridge is off, and Wine 11's default WoW64 mode is incompatible with it.
- **Cardinal (Windows build) is refused**: it overflows the stack when it starts and takes Wine down (`app/src/quirks.rs`).
- **Six Sines** is occasionally silent after save and reopen (matrix: intermittent).
- DPI scaling and cursor offsets on yabridge master were reported by others; we have not tested other DPI settings.
- Copy protection (iLok and similar) may fail in Wine.
- Not tested: Wine stable 11.0.x as the base instead of the 11.20 development release.

## Why Staging, and why a bundled runtime

We build on Wine Staging (the build script applies Staging's patchset, then ours). I have not recorded a reason for choosing Staging over vanilla Wine; our three patches apply to vanilla 11.20 too, so it is worth deciding whether Staging is still wanted. A bundled runtime is needed because (a) system Wine versions vary and yabridge releases lag, and (b) our patches must be in the Wine the plugins run in. If 0001 lands upstream, users with a recent system Wine would no longer *need* our build for JUCE plugins, but would still need yabridge master.

## Maintenance checklist

- Moving to a new Wine version: change the default in `tools/build-wine.sh`, run `just wine-build`, check the three patches still apply (`patch --dry-run`), then run `just compat` and `just s4-matrix-vblank`.
- Moving yabridge: replace the contents of `vendor/yabridge` with the new upstream commit in one commit, re-apply the patch in a second, update `vendor/yabridge/UPSTREAM_COMMIT`, then `just yabridge-build`. yabridge is compiled against the Wine we ship (`winegcc` from `runtime/wine-*/`), so rebuild it whenever Wine changes. Rerun the same checks.
- Publishing: `just package-runtime`, then upload the archive and `runtime-manifest.json` to the release the app downloads from (`DEFAULT_RUNTIME_URL` in `app/src/settings.rs`; a repo-specific URL that must change when the project moves).
