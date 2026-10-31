# Merlin

[![CI](https://img.shields.io/github/actions/workflow/status/hawawa4/merlin/ci.yml?branch=main&label=CI)](https://github.com/hawawa4/merlin/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hawawa4/merlin)](https://github.com/hawawa4/merlin/releases)

A live instrument host for Linux. You load up to eight instruments into a rack, give each a few
simple effects, and switch between them from a MIDI keyboard or controller while you play.

It plays CLAP, VST3, VST2 and LV2 plugins, SoundFonts (`.sf2`, `.sf3`) and, through a Wine and
yabridge runtime it downloads on request, 64-bit Windows plugins. The aim is that a Windows
plugin works in the rack without you setting up Wine or yabridge yourself.

Merlin is **not a DAW**: no tracks, no routing matrix, no recording to arm. The one
recording feature is a MIDI capture that is always on. It keeps the last 30 minutes you played,
in memory, and one button saves them as a `.mid` file.

Screenshots and downloads: [merlin.hawawa.org](https://merlin.hawawa.org).

## Install

Download a build from the [releases page](https://github.com/hawawa4/merlin/releases):

| File | Use |
|---|---|
| `merlin_VERSION_amd64.deb` | Debian, Ubuntu, PikaOS and derivatives: `sudo apt install ./merlin_*.deb` |
| `merlin-VERSION-linux-amd64.AppImage` | anywhere else: `chmod +x` it and run it |
| `merlin-VERSION-linux-amd64.tar.gz` | the four binaries, unpacked wherever you like |

You need:

- Linux on x86_64. Nothing else is built.
- PipeWire with its JACK API (`pipewire-jack`). Merlin talks to audio through JACK.
- Vulkan (`libvulkan1` and a driver) for the UI.
- An X11 session. The window is an X11 one, so on Wayland it runs through XWayland. This is
  deliberate: plugin editors are embedded as child windows, which only works on X11.

Installed copies update themselves from Settings > Updates. Local builds report `dev` and
never update.

## Using it

On first start Merlin scans for plugins in the standard places: `~/.clap`, `~/.vst3`, `~/.vst`,
`~/.lxvst`, `~/.lv2` (plus `$LV2_PATH`) and the matching folders under `/usr/lib`,
`/usr/lib64` and `/usr/local/lib`. SoundFonts are looked for in `~/.local/share/sounds/sf2`,
`~/.local/share/soundfonts`, `/usr/share/sounds/sf2` and `/usr/share/soundfonts`. Add other
folders in Settings. The scan runs in a separate process and caches its result, so a plugin
that crashes the scanner is set aside instead of taking the app down.

**Windows plugins.** Settings > Windows plugins downloads the Wine + yabridge runtime (about
70 MB) and can run a plugin installer, or a vendor's manager app (Native Access and the like),
inside Merlin's own Wine prefix. Plugins that show up are bridged automatically. Your system
Wine is never used or modified.

**What you get:**

- **Rack:** 8 slots, each with the plugin's own editor embedded in the main window, an EQ,
  compressor, delay and reverb, and a limiter on the master. A keyboard view lets you paint
  which keys and MIDI channel each instrument plays (splits, layers).
- **Playing live:** scenes (what is playing, volumes, mutes, zones, effects, presets, macros),
  macros, a lockable Perform view with big scene buttons and a PANIC button, and MIDI mapping:
  press MIDI MAP, click any control, move a knob or press a pad. There is a driver for the
  Novation Launchkey MK3 (DAW mode, pad lights, screen).
- **Songs:** setlists made of songs and parts. A part recalls a scene and can change the
  tempo. Backing tracks, a click, MIDI clock and timeline markers.
- **Looper:** records MIDI per instrument over 1 to 8 bars, with A/B parts, overdub and undo.
- **Crash isolation:** every plugin runs in its own process. If one crashes or hangs, its slot
  goes silent (with a *Retry* button) and nothing else is affected.
- **Performance check:** reports whether realtime scheduling and similar are set up, with the
  fix for each problem it finds.

**Where files go** (XDG locations):

| What | Where |
|---|---|
| Settings | `~/.config/merlin/settings.toml` |
| Plugin cache | `~/.cache/merlin/` |
| Rigs (`.mrig`) | `~/.local/share/merlin/rigs/` |
| Autosave, used for crash recovery | `~/.local/state/merlin/autosave.mrig` |
| Windows runtime and Wine prefix | `~/.local/share/merlin/` |
| MIDI captures | `~/Music/Merlin/` (changeable in Settings) |

**Command line:** `merlin [--plugin-dir DIR]... [--rig FILE] [--fresh]`. `--plugin-dir` adds a
plugin folder, `--rig` opens a rig, `--fresh` starts empty instead of restoring the autosave.

### Windows plugins in other hosts

`merlin-bridge` makes the same Wine runtime available to REAPER, Bitwig, Ardour and other Linux
plugin hosts, like `yabridgectl` with the runtime included: `merlin-bridge runtime install`,
then `merlin-bridge sync`. See `merlin-bridge --help` and [docs/wine.md](docs/wine.md).

## Limitations

- Developed on PikaOS with a handful of plugins and one controller (a Launchkey MK3 49). Other
  setups are less tested, so expect rough edges.
- Windows plugins are 64-bit only. Copy protection (iLok and the like) may not work. Some
  plugins occasionally start silent under Wine, and Cardinal is refused because it crashes
  Wine on start.
- LV2 plugins are not told the tempo.
- Linux x86_64 only.

## Building from source

You need a recent stable Rust toolchain (edition 2024; developed on 1.99),
[`just`](https://github.com/casey/just), and the libxcb development headers (`libxcb1-dev` on
Debian and Ubuntu). JACK headers are not needed: libjack is loaded at runtime, and
`build-support/pkgconfig/jack.pc` stubs it for the build.

```
just setup     # optional: downloads Surge XT and a SoundFont into test-assets/
just dev       # builds a release binary and runs it against test-assets/
just check     # fmt, clippy (warnings are errors) and tests; run before committing
just           # lists everything else
```

You can also run `cargo run --release -p merlin` directly. Plain `just dev` has no Windows
plugin support until the patched Wine exists under `runtime/`: build it with
`just wine-build` and `just yabridge-build` (both run in Docker and take a while), or install
the runtime from Settings in the app.

Other recipes worth knowing:

- `just soak` and `just soak-song`: long random-input and song-playback runs that watch for
  xruns, stuck notes and memory growth. They need PipeWire.
- `just compat`: loads every plugin it finds, one process each, and writes a compatibility
  table to `docs/compat-matrix.md` (not committed). About a minute per Windows plugin.
- `just e2e-controls`, `just fake-keyboard`, `just launchkey-probe`: MIDI mapping and
  controller checks, with or without hardware.
- `just rt-status`: is realtime scheduling available on this machine?

Tests: `cargo test --workspace` runs everything. CI runs only the `merlin-*` library crates,
because the `app` tests need X11, Vulkan and, for some, Wine.

`MERLIN_SCRIPT="3:load 1 Surge XT; 5:on 60; 8:quit" merlin` drives the app from a script of
`seconds:action` steps (slots are 1-based); the soak and compat tools use it.

### Releasing

Bump `version` in `app/Cargo.toml`, merge to main, then run the *release* workflow, or push a
tag `vX.Y.Z` that matches the version. CI must pass first. The workflow publishes the `.deb`,
AppImage, tarball and `SHA256SUMS.txt`. Keep those file names as they are: the in-app updater
looks for exactly them. The Wine runtime is a separate release tagged `runtime`, published by
hand with `just package-runtime` (see [docs/wine.md](docs/wine.md)).

## Repository layout

- [app/](app/): the application. egui on its own winit loop, X11 only. Also builds
  `merlin-scan` (loads one plugin file and reports what is in it) and `merlin-plugin-host`
  (runs one plugin in its own process). The app looks for both next to its own binary.
- [crates/](crates/): the product, one crate per concern.
  - `model`: rig, slots, scenes, mappings and their file formats
  - `hosting`: the plugin API, the CLAP, VST3, VST2 and LV2 hosts, SoundFont and built-in instruments
  - `dsp`: the built-in effects
  - `engine`: router, note ownership, slot mixer, looper, transport and the realtime queues
  - `audio-io`: JACK and offline backends
  - `embed`: embeds plugin editor windows in the main window
  - `scanner`: plugin discovery and the cache
  - `sandbox`: the out-of-process plugin host: shared-memory audio, crash detection
  - `control`: turns MIDI messages into actions
  - `backing`: backing tracks streamed from disk
  - `recorder`: the rolling MIDI capture
  - `wine`: Windows plugin support: runtime download, prefix, bridges
  - `bridge`: `merlin-bridge`, the command line tool above
  - `updater`: the in-app updater
- [devices/](devices/): controller drivers that know nothing about the app
  ([devices/README.md](devices/README.md) explains how to write one).
- [vendor/](vendor/): yabridge and oxisynth with our patches. Each has a `PATCHES.md` that
  says what we changed and what could go upstream.
- [build-support/](build-support/) and [tools/](tools/): the Wine patches and Dockerfiles, and
  the scripts behind the `just` recipes.
- [site/](site/): the landing page (Astro), published as a Docker image by the release workflow.
- [packaging/](packaging/): desktop file and icons for the `.deb` and AppImage.

Realtime rule for anyone touching `engine`, `dsp` or `hosting`: the audio callback must not
allocate, lock or block. The `rt_safety` test in `crates/audio-io/tests/` runs the audio path
under `assert_no_alloc` (active in debug builds, which is what `cargo test` uses).

## Documents

- [docs/wine.md](docs/wine.md): the Wine + yabridge runtime, our patches, and what could go upstream

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
