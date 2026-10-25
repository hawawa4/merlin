# QA checklist

Things only a person with ears and a mouse can check. Run `just dev` unless a step says otherwise.

## 5c. Perform view
- [ ] Click a tile: only that instrument plays (its border lights). This works anywhere on the tile that is not a button or a slider: the empty area of the frame, the number, the name, the level bar. **+** layers it with the others. **MUTE** mutes it. The tile's slider changes its volume and the bar above it moves while you play.
- [ ] **LOCK** (in the Stage strip): then the view switch and MIDI MAP are greyed, Escape and F11 do nothing, and dropping a file on the window is ignored (the strip under the header says so). A quick click on **LOCKED · HOLD TO UNLOCK** does nothing; holding it for about a second (a white bar fills) unlocks. (It used to give up at about 0.8 s, before the bar was full, so it never unlocked.)

## 5d. Macros
- [ ] Built-in effects as macro targets: on a slot's Macros tab press **+ parameter...**. The built-in effects come first (EQ, Compressor, Delay, Reverb, one entry per knob; the search box finds them, try *reverb mix*). Turn the Reverb and the EQ on in the Effects drawer (a macro moves their knobs but does not switch an effect on), then put *Reverb mix* and *EQ high* on one macro: dragging the macro moves both pedals' knobs in the drawer and you hear both change. The rows say *(effect)*; *from*/*to* and **x** work like for any target.
- [ ] Save, **New**, reopen: the effect targets and the macro's position are back. Store a scene with the macro at 0.2, move the macro to 0.9, recall the scene: the macro, the pedal knobs and the sound go back.
- [ ] Perform view: a macro that drives an effect knob shows up as a slider on its instrument's tile. Map a controller knob to the macro (Settings > Controls > *Macro of a slot*): the reverb and EQ follow it.

## 5g. Transport, click and MIDI clock
- [ ] Settings > Audio > Click output: with a multi-output interface pick two of its outputs: the click leaves the main mix and comes out there only (headphones). Back to "Main outputs" mixes it in again.
- [ ] MIDI clock: connect the `merlin:midi_out` port to a drum machine or a software that shows incoming tempo (`pw-link merlin:midi_out <port>`); it follows the tempo and starts and stops with START and STOP.
- [ ] Settings > Controls: *Start / stop*, *Click on / off*, *Tap tempo* on a foot switch or pad.
- [ ] Close and reopen the rig: tempo, count-in and click settings are back.

## 5h. Backing tracks
- [ ] Setlist > a song > **+ Add tracks…**, the library on the right, or a file dropped on the window while the Setlist screen is open. It says *loading...* then *ready 3:07*. A file that is not audio says *cannot play* (hover for why).
- [ ] **Go** on a part of the song, Perform view, **START**: after the count-in the tracks play together, in time with the click. The clock shows 0:00 / length. **STOP** stops them (no click or pop), and **START** again restarts from the top.
- [ ] Try your real stems: a long stereo WAV, a 44.1 kHz file (resampled), an MP3 and a mono file. Does anything drift against the click over a whole song? Any gaps or crackles? (The audio-engine counter *underruns* in the log lines would grow if the disk could not keep up.)
- [ ] 8+ stems at once from a slow disk or a USB stick.
- [ ] Per-track **gain**, **mute** (also in Perform) and **cue**: a cue track is heard only on the click output (headphones), not in the main mix (Settings > Audio > Click output).
- [ ] Switch to another song and back (parts, foot switch, Setlist): the tracks of the entered song are the ones that play.
- [ ] Save, reopen: the tracks are still listed; delete or move a file and open the rig: that track says *cannot play* and the rest still work.
- [ ] Part tempo overrides change the click but not the recorded stems: expected.

## 5i. Timeline markers
- [ ] Setlist > a song > Timeline > **+ Marker**: bar 2 beat 1 **Go to part** 2; bar 3 **Cue** "Chorus"; bar 5 **Stop**. Go to the song, Perform view, START: at bar 2 the sound changes to part 2, the cue appears large at bar 3, the transport stops at bar 5. Try it with backing tracks playing too: do the part changes land on the beat?
- [ ] **Program Change** and **Controller** markers: with a synth or a MIDI monitor connected to the `merlin:midi_out` port (`pw-link merlin:midi_out <port>`), it receives them on the right channel at the right bar, together with the MIDI clock.
- [ ] Change the song's tempo or beats per bar: the markers keep their bars and beats (they move in time).
- [ ] Edit a marker while the transport is stopped, press START: the new marker list is used. (Editing while it runs takes effect after the next stop.)
- [ ] **Next part** marker plus a foot switch mapped to *Next part*: both advance without skipping a part.

## 5j. VST2 plugins
- [ ] Kontakt/Reaktor-type plugins that are huge: how long do they take to load, and does the app stay responsive?

## 5k. LV2 plugins
- [ ] LV2 plugins in `~/.lv2` or `/usr/lib/lv2` appear in the browser as `(LV2)` without any setting (the first scan reads their description files, quick). Load a few synths and effects: do they play, show their editor, and come back from a saved rig? Tell me which do not (and the message in the log line, e.g. "needs <feature>").
- [ ] A plugin with a GTK or Qt editor (not X11): it will play but its editor will not open (expected for now).
- [ ] Insert an LV2 effect on a slot (Effects drawer > + Load effect).
- [ ] The Macros tab > *+ parameter...* on an LV2 plugin lists its control ports (plugins that use only patch parameters list none).

## 5l. MIDI map mode: any control on any controller
Works with any device that sends CC, notes or Program Change (this is the "every device is supported" route; the Novation-specific one comes later). No hardware: run `just fake-keyboard` or the pretend controller to get something to learn from.
- [ ] Macros tab: each macro slider can be mapped (same as before, now by clicking it).

## 5m. Launchkey MK3 (your keyboard)
Plug it in before starting `just dev` (or while it runs: the log says `controller: Launchkey MK3 49 found`). The keys, wheels and pedal work as a keyboard as before; the DAW port is taken as a controller, put into DAW mode, and given a built-in layout. When the app quits the device goes back to normal. No setup needed.
- [ ] **Pad modes** (the Shift menu on the device): *Session* (the default) is what is described above. *Drum*: the 16 pads play notes on channel 10 of the MIDI port into the slot that plays (try a SoundFont drum kit), and now glow in that slot's colour instead of staying dark. *Device Select* (hold the Device Select button): the pads become two rows, top = select slot 1-8 (lit like the Session row), bottom = add/remove the slot as a layer (bright = it plays). Release it and the Session pads are back. *Scale chords / User chords / Custom*: the device does these itself and the app leaves the pad lights alone. Tell me if a mode leaves wrong lights behind when you switch. **Known problem: Drum mode plays nothing.** To find out why, run `just launchkey-probe` (the app can be running), switch the device to Drum mode, press a few drum pads and send me the lines: it now names the drum pads and shows whether they arrive on the MIDI or the DAW port (on the DAW port the app treats them as controls and never plays them).

## 5n. MIDI capture (the last 30 minutes, saved by a button)
The app always listens to your keyboards and the on-screen keys. Nothing is written until you press **Capture MIDI** (header, or **CAPTURE** in the Perform view). Files go to `~/Music/Merlin`.
- [ ] A long session: leave it running for an hour of normal playing and check memory (Settings > Performance, or `ps`): the capture should stay a few MB at most.

## 6. Stability
- [ ] Kill a Windows plugin's Wine process while the app runs: `pgrep -af yabridge-host`, then `kill -9 <pid>` of one. The app should restart itself from the autosave and name the plugin it quarantined (it comes back with *Retry*).
- [ ] The 2-hour rehearsal: 6–8 instruments including a Windows one, switching and playing for 2 hours. For an unattended version: `just soak 7200 3 --windows "Six Sines"`, then read the `state:` lines (xruns, held keys, RSS, threads). Expect 0 xruns, 0 held keys at the end, memory flat.
- [ ] Song soak: `just soak-song` (100 passes, about 8 minutes, nothing else using the audio): prints `ok: 100 passes...`. With your real stems and a real song, play it 20 times in a row from the Perform view with the foot switches and tell me about any drift, gap or stuck state.
- [ ] Close the app normally a few times: no crash dialog, no leftover `yabridge-host` processes (`pgrep -af yabridge-host`).

## 7. Known, not worth reporting
- Surge XT (VST3) shows a text-cursor mouse pointer everywhere in its editor.
- The built-in effects are basic and need a lot more work.
- Cardinal (Windows) is refused on purpose: it crashes Wine.
