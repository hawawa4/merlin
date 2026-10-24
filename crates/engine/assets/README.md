# Metronome clicks

`metronome_click.wav` and `metronome_accent.wav` (mono, 16-bit, 24 kHz, 70 ms) are the two clicks
of the transport's click and the header's metronome. They are the same files Lyrica uses
(`../lyrica/scripts/extract_metronome.py` cuts them out of a freesound recording of a metronome).
The engine decodes and resamples them once, when the transport is made.
