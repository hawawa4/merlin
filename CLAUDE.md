# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Merlin is a Linux live instrument host (a rack of CLAP/VST3/VST2/LV2/SoundFont/Windows plugins switched from MIDI controllers), written in Rust (edition 2024). It is explicitly not a DAW. [README.md](README.md) covers what it does and how users install it; this file covers working in the code.

## Commands

`just` is the task runner (`just` lists recipes).

```
just check                       # cargo fmt --check, clippy --workspace --all-targets -D warnings, cargo test --workspace
just fmt                         # cargo fmt --all
just dev [--fresh] [--rig F]     # release build + run the app against test-assets/
just setup                       # optional: downloads Surge XT and a SoundFont into test-assets/ (gitignored)
cargo test -p merlin-engine --test looper some_test_name    # one integration test
cargo test -p merlin-dsp comp::                             # unit tests by path filter
```

- CI runs only `cargo clippy/test -p 'merlin-*'` (the library crates); the `app` crate's tests need X11, Vulkan and sometimes Wine. `just check` runs the whole workspace, so it is the stricter local gate.
- Several integration tests need `test-assets/` (Surge XT, a SoundFont) and print `skipped: ...` and pass when it is missing, so a green run without `just setup` proves less. `just test-wine` runs the Wine-backed ones.
- `vendor/` is excluded from the workspace (`exclude = ["vendor"]`) except `oxisynth`, which is pulled in through `[patch.crates-io]`. yabridge is C++ built with meson inside Docker (`just yabridge-build`, `just wine-build`); don't expect `cargo` to touch it.
- libjack is `dlopen`ed at runtime; `.cargo/config.toml` points `PKG_CONFIG_PATH` at a stub `jack.pc` so the build needs no JACK headers. Build needs `libxcb1-dev`.
- Soak/e2e/compat recipes (`just soak`, `soak-song`, `compat`, `e2e-controls`) need a running PipeWire and are long-running; they drive the app through the `MERLIN_SCRIPT` hook (`"secs:verb args; ..."`, slots 1-based, see `parse_script` in `app/src/main.rs`).
- Env switches: `MERLIN_NO_SANDBOX=1` loads plugins in the app process instead of a child (this revives the crash-restart path in `app/src/guard.rs`); `MERLIN_PLUGIN_HOST` overrides the `merlin-plugin-host` path; `MERLIN_RELEASE_VERSION` is baked in at build time by the release workflow, and without it the updater reports `dev` and never updates.

## Architecture

The app is `app/` (egui on a hand-rolled winit loop, **X11 only**, so plugin editors can be embedded as child windows; it runs under XWayland on Wayland). Everything else is a library crate in `crates/`, plus controller drivers in `devices/`. Dependencies point inward: `model` and `engine` know nothing about UI or audio backends.

**Threading is the central design constraint.** The audio thread owns the `Engine` (`crates/engine`) and calls `Engine::process`. The main thread only holds an `EngineHandle`: commands go in over a lock-free queue, retired processors come back over another (so they are dropped off the audio thread), and meters/note activity are read from shared atomics. Nothing in `engine`, `dsp` or the audio-thread half of `hosting` may allocate, lock or block. `crates/audio-io/tests/rt_safety.rs` checks this with `assert_no_alloc` (debug builds only). When adding code on the audio path, the vendored `oxisynth` patches are the precedent: it was patched to remove a per-note heap allocation.

**UI / session split in `app/`.** `ui.rs` and the `*_ui.rs` / view files are pure drawing: they read `Session` and return `Action`s, and `main.rs` applies them. `Session` (`app/src/session.rs` and `session/*.rs`) owns everything that is not drawing: the engine and its JACK backend, loaded plugins, the rig and persistence, the editor dock, MIDI controls, setlists, looper, capture. Background threads and plugins reach the main loop through `UserEvent` on a winit `EventLoopProxy`. New features usually mean a new `Action` variant, handling in `Session`, and drawing in a view.

**Plugin hosting stack.** `hosting` defines `PluginHandle` and the CLAP/VST3/VST2/LV2 hosts, SoundFont playback (`rustysynth`/`oxisynth`) and built-in instruments. In normal operation the app never loads a plugin itself: `sandbox` starts one `merlin-plugin-host` process per plugin (`RemotePlugin` implements `PluginHandle`), with control over a Unix socket (every call has a timeout) and audio through shared memory with futex handoff. A dead plugin process means a silent slot plus a `HostEvent::Crashed`, not a crashed app. Plugin discovery is also out of process (`merlin-scan`, `crates/scanner`), with a cache keyed by path+mtime and a quarantine list. `merlin-scan` and `merlin-plugin-host` are binaries of the `app` package and must sit next to `merlin`, which is why packaging ships all four binaries together.

**Windows plugins.** `crates/wine` manages a downloaded runtime (our patched Wine + pinned yabridge, see `docs/wine.md`), a private Wine prefix, and "bridges" (yabridge chainloaders, `Name.clap` plus a `Name.clap-win` link) that make Windows plugins look like native ones to the scanner. The app sets `WINELOADER` and `PATH` for the managed runtime itself and marks that with `MERLIN_MANAGED_RUNTIME` so a self-restart doesn't mistake it for the user's Wine. The yabridge fork has a socket-failure hook so `guard.rs` can quarantine a crashing plugin and restart from the autosave. `crates/bridge` (`merlin-bridge`) is a separate CLI exposing the same runtime to other DAWs.

**Control surfaces.** `crates/control` is pure logic: MIDI in, a list of outcomes out, which the app maps to the same `Action`s as on-screen buttons. `devices/surface-api` defines the `Driver`/`Surface` traits (bytes in, bytes out, no I/O); `devices/launchkey-mk3` implements them, and the policy for what pads/knobs do lives in `crates/control/src/surface.rs`, driven by the driver's `Description`. New drivers are registered in `app/src/drivers.rs` behind a cargo feature. See [devices/README.md](devices/README.md).

**Persistence.** Rigs (`.mrig`) are serde types in `crates/model`. File format changes must bump `RIG_VERSION` and add a step to `MIGRATIONS` in `crates/model/src/migrate.rs` (the list is empty today: no format change has shipped yet). The app autosaves to `~/.local/state/merlin/autosave.mrig` and restores it after a crash.

**Updater and releases.** `crates/updater` looks for exact release asset names (`merlin_VERSION_amd64.deb`, `merlin-VERSION-linux-amd64.{AppImage,tar.gz}`, `SHA256SUMS.txt`) produced by `.github/workflows/release.yml`; renaming one breaks in-app updates. The version comes from `app/Cargo.toml`, and the tag must be `vX.Y.Z` to match. The Wine runtime is a separate GitHub release tagged `runtime`, published by hand (`just package-runtime`); its URL is hard-coded in `crates/wine/src/layout.rs`.

## Conventions

- UI is deliberately skeuomorphic: controls are built from the hardware look in `app/src/hw.rs` (chassis, key caps, recessed switches, pedals), not stock egui widgets. New UI should use those pieces.
- Many doc comments cite `plan §4.x` / `plan D7`. `docs/plan.md` and `docs/research.md` were deleted; don't go looking for them, and don't add new references.
- Vendored code has a `PATCHES.md` (`vendor/oxisynth`, `vendor/yabridge`) recording every local change and whether it could go upstream; update it when touching vendored sources. Keep Wine patches in `build-support/wine-patches/` and see `docs/wine.md` for the maintenance checklist when moving Wine or yabridge versions.
- Tests that create throwaway Wine prefixes must set `XDG_DATA_HOME` and `XDG_CONFIG_HOME`, or Wine's `winemenubuilder` writes `.desktop` files into the real home.
