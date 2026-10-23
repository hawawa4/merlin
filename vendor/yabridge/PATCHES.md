# Merlin patches to yabridge

Upstream: https://github.com/robbert-vdh/yabridge, master commit `b580a9f7` (2026-08-02; the last
release, 5.1.1, does not work with Wine 9.22 or later). GPL-3.0, like this project. `git log -p
vendor/yabridge` shows exactly what we changed: the first commit is upstream unmodified.

Built by `tools/build-yabridge.sh` against our own patched Wine (see docs/wine.md). The `subprojects/*.wrap`
files download asio, bitsery, clap, function2, ghc_filesystem, tomlplusplus and the VST3 SDK (tag
`v3.7.7_build_19-patched`) from GitHub at build time; the build needs network.

1. `src/common/communication/common.h`: `yabridge_socket_failed_hook`. When a socket to the Wine host
   fails, a function the host application registered is called before the C++ exception propagates.
   Without it, a crashing Windows plugin throws a C++ exception through the host's audio callback, which
   aborts a Rust process. Nothing changes for hosts that do not register a hook.

A bridge that is a symlink into a self-contained runtime works without `PATH`, `WINELOADER` or `WINEPREFIX`,
which is how a DAW started from a desktop launcher looks. The runtime is a folder with `yabridge/`
(the libraries, `yabridge-host.exe`) and `wine/` side by side. A copied chainloader behaves as before.

2. `src/chainloader/utils.cpp`: the chainloader looks for `libyabridge-*.so` next to its own real path
   (symlinks followed) before searching `PATH`.
3. `src/plugin/utils.cpp`:
   - `WINELOADER`: if it is unset or not executable, `<runtime>/../wine/bin/wine` is used, for the host
     process and for the `wine --version` log line. A `WINELOADER` that works still wins.
   - Wine prefix: after `WINEPREFIX` and the `dosdevices` folder above the plugin, the first line of
     `<runtime>/default-wineprefix` (when that file exists) names the prefix, instead of `~/.wine`.
   - The "runtime" is the folder that holds the bridge's real path and `yabridge-host.exe`.

`tools/check-standalone-bridge.sh` tests all of this against a clean environment.
