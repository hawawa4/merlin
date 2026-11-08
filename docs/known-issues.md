# Known issues

Things seen in use that are not fixed yet. Remove an entry when it is.

## Pianoteq costs more CPU hosted than standalone

On the same passages Merlin shows about 20 points more load than standalone Pianoteq, and
Pianoteq's own overload detection can silence it. Findings so far (see the 0.12.1 commit):

- The sandbox hop and the engine are not the main cause: the engine block is about 0.1% of the
  block, and `MERLIN_NO_SANDBOX=1` is only slightly lower.
- `MERLIN_MIDI_AT_BLOCK_START` and `MERLIN_MAX_BLOCK` made no difference.
- Pianoteq's "multicore rendering: max" took Merlin from 60-80% to about 35% and ended the
  overload drops (standalone with the same setting: about 20%). Suspect: the plugin's helper
  threads run as ordinary threads and are woken late. Ideas: promote them next to the audio
  thread's priority; offer CLAP's thread-pool extension (not registered today).
- VST3 builds use somewhat more CPU than LV2 for the same plugin; not looked into.
