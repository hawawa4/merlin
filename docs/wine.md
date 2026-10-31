# Wine and Windows plugins: where we stand

*Written 2026-10-23. Plan context: [plan.md](plan.md) §4.8 (Windows plugins) and D6; research: [research.md](research.md) §3.2.*

## In one paragraph

Windows plugins run in **our own patched Wine 11.20 (Staging)**, bridged by a **pinned, lightly patched yabridge** (master commit `b580a9f7`, not a release). Both are built by us in Docker, packed into one archive and downloaded by the app on demand; the user's system Wine is never used or touched. The Wine patches are what make most commercial plugins usable (JUCE editors freeze without them). The yabridge patch only keeps the app alive when a Windows plugin crashes. Nothing here depends on a yabridge *release* or on the user having a particular Wine.

## What we ship

| Piece | Version | Where it comes from | Our changes |
|---|---|---|---|
| Wine | 11.20 + Wine Staging 11.20, WoW64 build (`--enable-archs=i386,x86_64`: 64-bit plugins, and 32-bit programs for installers) | `tools/build-wine.sh` (Docker, Ubuntu 24.04, `build-support/wine/Dockerfile`) → `runtime/wine-11.20-wow64/` | 3 patches, [build-support/wine-patches/](../build-support/wine-patches/) |
| yabridge | master `b580a9f7` (2026-08-02), vendored in [vendor/yabridge/](../vendor/yabridge/), `-Dbitbridge=false` | `tools/build-yabridge.sh` (Docker, Ubuntu 24.04, `build-support/yabridge/Dockerfile`, compiled with our own Wine's `winegcc`) → `runtime/yabridge-b580a9f7-wine-11.20-wow64/` | 1 patch, listed in [vendor/yabridge/PATCHES.md](../vendor/yabridge/PATCHES.md) |
| Prefix | created by `wineboot -u`, `WINEDLLOVERRIDES=mscoree,mshtml=d` (no Mono/Gecko dialogs) | `crates/wine` | none |
| Package | `wine-11.20-wow64_yabridge-b580a9f7.tar.zst`, about 70 MB, with a SHA-256 manifest | `tools/package-runtime.sh`, fetched by `crates/wine/src/runtime.rs` | none |

How the app uses it: `crates/wine` writes yabridge *chainloaders* (`Name.clap` + a `Name.clap-win` link to the real DLL) and runs the installer and `wineboot` in our prefix. The app sets `WINELOADER` and puts the yabridge folder first on `PATH` when it starts, so only our process uses this Wine. We do not use `yabridgectl` at all; `crates/wine` reimplements its sync.

## Installers and manager apps

Settings → Windows plugins runs a plugin installer (`.exe` or `.msi`) in the app's own prefix, or any program installed there, and when it ends makes bridges for the plugins that appeared (`Session::run_windows_program`). Two things about this that are easy to get wrong:

- **Installers are 32-bit.** Inno Setup and NSIS stubs are `PE32`, whatever they install. That is why the Wine build is WoW64 (`i386,x86_64`); the plugins themselves still have to be 64-bit. A prefix made by the old 64-bit-only Wine is upgraded by the new Wine on its first start (checked: `syswow64` goes from empty to about 850 files, installers then run), so nothing has to be recreated. A user with an old *download* is shown "This version cannot run most plugin installers" and an Update button (`Installed::runs_32_bit`).
- **Many vendors ship a manager app instead of plugins** (Native Access, Arturia Software Center, ...). Settings lists the programs in the prefix that have a Start Menu or Desktop shortcut after "Installed:" and opens one on a click; the plugins it installs are bridged when it is closed. They are found by reading the `.lnk` files (`crates/wine/src/apps.rs`; Wine writes the full target path into each). Uninstallers, documents, Wine's own programs and linked folders (a prefix's `Desktop` is a link into the user's home) are skipped. A program with no shortcut is not listed ("Open Wine prefix" still reaches it).

Updating the runtime restarts the app, and a restart keeps the environment: the app marks the `WINELOADER` and `PATH` it set for the downloaded runtime (`MERLIN_MANAGED_RUNTIME`) and takes them back at startup, otherwise the old runtime passes for the user's own Wine and the app restarts into it forever.

Wine's `winemenubuilder` is still active in the prefix: an installer that makes a Start Menu or Desktop shortcut also writes `.desktop` files and menu entries into the user's `~/.local/share/applications` and `~/.config/menus`, and the prefix's first start registers file associations (`.reg`, `.chm`, ...) with `WINEPREFIX=<our prefix>`. Tests that make throwaway prefixes must set `XDG_DATA_HOME` and `XDG_CONFIG_HOME` (the compat and soak scripts do).

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

## `merlin-bridge`: Windows plugins for other plugin hosts

[crates/bridge](../crates/bridge/) is a small command line tool that makes our Wine and yabridge usable from REAPER, Bitwig, Ardour and any other host, without the app. It is `yabridgectl` with the runtime included (`just bridge --help`):

- `runtime install` downloads the same Wine + yabridge archive the app uses, into the same folder (`~/.local/share/merlin/runtime`), so installing it from either place serves both.
- `sync [FOLDER...]` makes a bridge for every 64-bit Windows CLAP, VST3 and VST2 plugin in the Wine prefix the app uses and in each folder named (folders are remembered in `~/.config/merlin/bridge.json`). Bridges go to `~/.clap/merlin-bridges`, `~/.vst3/merlin-bridges` and `~/.vst/merlin-bridges`, one subfolder per source. Plugin files are never touched.
- `install FILE.exe|.msi` runs an installer in the prefix, then syncs. `forget FOLDER` and `clean` remove bridges. `status` shows what is installed and bridged, and warns when `yabridgectl` has bridged the same plugins (a host would list them twice).

Bridges here are **symlinks** (`Placement::Link`) to `<runtime>/current/yabridge/libyabridge-chainloader-*.so`. `current` is a link to the active runtime that the runtime install keeps up to date (the app's install too), so a runtime update needs no new `sync`; `sync` is for installing or removing plugins. The app keeps making its own copied bridges in `~/.local/share/merlin/bridges` and its scan skips `merlin-bridges`, so it does not list plugins twice.

Not covered: nothing watches the folders, so a new plugin needs a `sync`; the tool does not stop a plugin that is known to crash Wine (Cardinal) from being bridged; a folder inside someone's own Wine prefix runs in that prefix with our Wine (`sync` says so).

## Known limits

- **64-bit plugins only.** The bitbridge is off, and Wine 11's default WoW64 mode is incompatible with it. Wine itself is a WoW64 build, though, so 32-bit *programs* run: installers and vendors' manager apps are mostly 32-bit. (Until 2026-10-31 it was built 64-bit only, and every 32-bit installer failed with `wine: failed to load \??\C:\windows\syswow64\ntdll.dll error c0000135`. The app tells a user with such a download to update: `Installed::runs_32_bit`.)
- **Cardinal (Windows build) is refused**: it overflows the stack when it starts and takes Wine down (`app/src/quirks.rs`).
- **Six Sines** is occasionally silent after save and reopen (matrix: intermittent).
- DPI scaling and cursor offsets on yabridge master were reported by others; we have not tested other DPI settings.
- Copy protection (iLok and similar) may fail in Wine.
- Not tested: Wine stable 11.0.x as the base instead of the 11.20 development release.

## Why Staging, and why a bundled runtime

We build on Wine Staging (the build script applies Staging's patchset, then ours). I have not recorded a reason for choosing Staging over vanilla Wine; our three patches apply to vanilla 11.20 too, so it is worth deciding whether Staging is still wanted. A bundled runtime is needed because (a) system Wine versions vary and yabridge releases lag, and (b) our patches must be in the Wine the plugins run in. If 0001 lands upstream, users with a recent system Wine would no longer *need* our build for JUCE plugins, but would still need yabridge master.

## Maintenance checklist

- Moving to a new Wine version: change the default in `tools/build-wine.sh`, run `just wine-build`, check the three patches still apply (`patch --dry-run`), then run `just compat` (the compatibility pass).
- Moving yabridge: replace the contents of `vendor/yabridge` with the new upstream commit in one commit, re-apply the patch in a second, update `vendor/yabridge/UPSTREAM_COMMIT`, then `just yabridge-build`. yabridge is compiled against the Wine we ship (`winegcc` from `runtime/wine-*/`), so rebuild it whenever Wine changes. Rerun the same checks.
- Publishing: `just package-runtime`, then upload the archive and `runtime-manifest.json` to the release the app downloads from (`DEFAULT_RUNTIME_URL` in `crates/wine/src/layout.rs`; a repo-specific URL that must change when the project moves). The release is tagged `runtime`; `gh release create runtime dist/runtime/* --title "Windows plugin support" --latest=false` the first time, `gh release upload runtime dist/runtime/* --clobber` after that. It is not marked as the latest release, so `releases/latest` stays the app (see `.github/workflows/release.yml`).
