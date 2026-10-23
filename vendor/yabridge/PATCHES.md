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
