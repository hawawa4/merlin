# Merlin patches to oxisynth 0.1.0 (LGPL-2.1)

Upstream: https://github.com/PolyMeilex/oxisynth. To be sent upstream; drop this copy once released.

1. `core/midi.rs` `inner_noteon`: the 64-entry modulator list was built on the heap with a
   `Vec` on every note-on. It is a stack array now.
2. `core/voice_pool/mod.rs`: `Vec::with_capacity(polyphony)`, so the first notes do not grow
   the voice vector on the audio thread.
3. `core/soundfont/mod.rs`: `SoundFont` is `Clone` (samples are shared) and has `presets()`,
   so the preset list is readable and one decoded file can serve several synths.
4. `api/settings.rs`: `Synth::active_voice_count()`, so a host can skip a silent synth.
5. `api/font.rs`: `Synth::program_select()`, to pick a preset of one particular SoundFont when
   several are loaded (a program change takes the first font that has the bank and program).
