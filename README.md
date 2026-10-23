# Merlin

A Linux-first live instrument host for playing in real time: a rack of instruments
(CLAP, VST3, VST2, LV2, SoundFonts, and Windows plugins through a bundled Wine + yabridge),
each with simple built-in effects, switched from MIDI controllers. Not a DAW: no tracks, no
routing matrix, no recording to arm. The one exception is a MIDI capture that is always on: the app keeps
the last 30 minutes of what you played, in memory, and one button saves them as a `.mid` file.

## What it does

- **Rack:** 8 instrument slots with embedded plugin editors, a keyboard map, zones and layers.
  Each slot has an EQ, compressor, delay and reverb; the master has a limiter.
- **Playing live:** snapshots, macros, a Perform view, MIDI mapping of any control on any
  controller, and a Novation Launchkey MK3 driver (DAW mode, pad lights, screen).
- **Songs:** setlists, songs and parts, backing tracks, a click, MIDI clock and timeline markers.
- **Plugins:** a searchable browser, a plugin scanner with a cache, and a performance check
  (realtime scheduling and more, with the exact fix). Rigs are saved as `.mrig` files, with autosave.

Status: Phase 1 (the MVP) is built and the later phases of the plan are being filled in.
[docs/qa-checklist.md](docs/qa-checklist.md) lists what has to be checked by hand.

## Running it

Needs a Rust toolchain, [`just`](https://github.com/casey/just), and PipeWire (through its JACK
API). Rust 1.99 and PipeWire 1.6 on PikaOS are what it is developed on.

```
just setup     # downloads Surge XT and a SoundFont into test-assets/ (optional)
just dev       # builds and runs the app
just check     # format, lint and test; run before committing
just           # lists everything else (soak runs, compatibility pass, Wine build, ...)
```

`just dev` points at the current round of manual testing; its comment says what to try.
Windows plugins need the patched Wine and yabridge, which are built in Docker
(`just wine-build`, `just yabridge-build`) or downloaded by the app from Settings.

## Layout

- [app/](app/): the application (egui on its own winit loop, X11)
- [crates/](crates/): the product, one crate per concern:
  `model` (rig, slots, filters), `hosting` (plugin API, CLAP, VST3, VST2 and LV2 hosts, built-in instruments),
  `dsp` (built-in effects), `engine` (router, note ownership, slot mixer, queues),
  `audio-io` (JACK and offline backends), `embed` (editor dock), `scanner` (plugin discovery and cache),
  `control` (MIDI messages to actions), `backing` (backing tracks streamed from disk),
  `recorder` (the rolling MIDI capture), `wine` (Windows plugin support),
  `bridge` (`merlin-bridge`, Windows plugins for other hosts such as REAPER, Bitwig and Ardour)
- [devices/](devices/): controller drivers that know nothing about the app (`surface-api`, `launchkey-mk3`)
- [vendor/](vendor/): yabridge and oxisynth with our patches, each with a note on what could go upstream
- [build-support/](build-support/), [tools/](tools/): the Wine patches and Dockerfiles, and scripts for building, packaging and testing

## Documents

- [docs/plan.md](docs/plan.md): architecture and roadmap
- [docs/research.md](docs/research.md): existing products and technical research
- [docs/wine.md](docs/wine.md): the Wine + yabridge runtime, our patches, and what could go upstream
- [docs/compat-matrix.md](docs/compat-matrix.md): which Windows plugins work (written by `just compat`)
- [docs/qa-checklist.md](docs/qa-checklist.md): manual checks

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
