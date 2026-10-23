# Merlin

A Linux-first live instrument host for playing in real time: a rack of instruments
(CLAP, VST3, Windows plugins via a bundled Wine + yabridge, SoundFonts), each with
simple built-in effects, switched from MIDI controllers. Not a DAW: no tracks, no
routing matrix, no recording to arm. The one exception is a MIDI capture that is always on: the app keeps
the last 30 minutes of what you played, in memory, and one button saves them as a `.mid` file.

Status: **Phase 1 (MVP) in progress.** The spikes are done (GO). `just dev` runs the app: a rack of
8 CLAP or VST3 instruments (Linux, or Windows CLAP through Wine + yabridge) with embedded editors, a keyboard
map, zones, layers, save/open and autosave. Each slot has built-in effects (EQ, compressor, delay, reverb) and the master has a limiter. A searchable browser, a settings panel and a performance check (realtime scheduling and more, with the exact fix) are in.

- [docs/research.md](docs/research.md): existing products and technical research
- [docs/plan.md](docs/plan.md): architecture and roadmap
- [docs/wine.md](docs/wine.md): the Wine + yabridge runtime, our patches, and what could go upstream
- [crates/bridge](crates/bridge/): `merlin-bridge`, Windows plugins for other plugin hosts (REAPER, Bitwig, Ardour)
- [spikes/](spikes/): throwaway experiments that answered the Phase 0 questions
- [crates/](crates/): the product, built up in the order of the plan's Phase 1:
  `model` (rig, slots, filters), `hosting` (plugin API, CLAP host, built-in instruments),
  `dsp` (built-in effects), `engine` (router, note ownership, slot mixer, queues), `audio-io` (JACK and offline backends),
  `embed` (editor dock), `scanner` (plugin discovery and cache), `recorder` (the rolling MIDI capture)
- [app/](app/): the application

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
