# Merlin

A Linux-first live instrument host for playing in real time: a rack of instruments
(CLAP, VST3, Windows plugins via a bundled Wine + yabridge, SoundFonts), each with
simple built-in effects, switched from MIDI controllers. Not a DAW: no tracks, no
routing matrix, no recording.

Status: **Phase 1 (MVP) in progress.** The spikes are done (GO). `just dev` runs the app: a rack of
8 CLAP instruments (Linux, or Windows through Wine + yabridge) with embedded editors, a keyboard
map, zones, layers, save/open and autosave. VST3, built-in effects and the settings panel are next.

- [docs/research.md](docs/research.md): existing products and technical research
- [docs/plan.md](docs/plan.md): architecture and roadmap
- [spikes/](spikes/): throwaway experiments that answered the Phase 0 questions
- [crates/](crates/): the product, built up in the order of the plan's Phase 1:
  `model` (rig, slots, filters), `hosting` (plugin API, CLAP host, built-in instruments),
  `engine` (router, note ownership, slot mixer, queues), `audio-io` (JACK and offline backends),
  `embed` (editor dock), `scanner` (plugin discovery and cache)
- [app/](app/): the application

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
