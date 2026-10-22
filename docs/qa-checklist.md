# QA checklist

Things only a person with ears and a mouse can check. Run `just dev` unless a step says otherwise.
Tick as you go; anything odd, send me what you saw (and the exact text of any error in the log line under the title).

## 1. Rack and effects
- [ ] Layer 3–4 slots (click one, shift-click others) and play. Each row shows a small `N.N%` next to its meter; Windows plugins add `wine N%`. Hover it for the explanation. Does anything go orange or red while you play normally?
- [ ] Effects tab → *Load effect…* → "Surge XT Effects" as an insert. Untick **on** to bypass it: the sound should change back at once. Open it with **Edit**.
- [ ] Save the rig, press **New**, reopen it. The insert, its settings and its bypass state come back.

## 2. SoundFonts
- [ ] Load MuseScore General and a system SF2. Pick presets from the list, use the search box, move **level**.
- [ ] Expand **Shape the sound**: attack, release, brightness, resonance, tuning, voices. Each should be audible; **Reset** undoes them. Save and reopen: values stay.
- [ ] Pick a drum kit (bank 128, e.g. "Standard") and play across the keyboard.
- [ ] Settings → Plugins → *Sound folders*: add a folder with a `.sf2`; it appears in the browser.
- [ ] Drag a `.sf2` from your file manager onto the window **while a plugin editor is showing** (not just on the empty dock). Banner appears? File loads?

## 3. Windows plugins
- [ ] Windows plugins are in the browser without pressing anything. Load Podolski and TyrellN6 (VST3 and CLAP). Play, use the editor, save and reopen.
- [ ] **Silence check:** load Podolski or TyrellN6 ten times (load, play a chord, unload). Does it ever stay silent? Note whether it was right after loading or after reopening a rig. (Known: about 1 start in 10 under my test runs.)
- [ ] Settings → Windows: the doctor lines are green. Optional: *Download Windows plugin support* needs a published runtime; to try it locally, run `just package-runtime`, then `python3 -m http.server -d dist/runtime`, set `runtime_url = "http://127.0.0.1:8000/runtime-manifest.json"` in `~/.config/merlin/settings.toml`, and start the app **without** `just dev` (it uses its own Wine).
- [ ] Optional, if you have a Windows plugin installer: drop its `.exe`/`.msi` on the window. Does the installer window appear, and do the plugins show up afterwards?

## 4. MIDI (no hardware needed for the first part)
- [ ] In a second terminal run `just fake-keyboard`. Settings → MIDI devices: "Fake Keyboard" appears with a light that flashes. Set it to **Controller**: the notes stop playing instruments. Back to **Keyboard**: they play. Untick **use**: it disconnects.
- [ ] When the fake keyboard exits, the log says the device is gone; run it again and it comes back with the role you set.
- [ ] With a real keyboard (later): what is it called in the list? Does it play? Does unplugging and replugging it behave?

## 5. Controls (needs a controller or a keyboard with pedals/knobs; or use the pretend one)
- [ ] No hardware: `just e2e-controls` prints `ok: ...`.
- [ ] Settings → Controls. Pick "Select a slot", slot 2, press **Learn…**, press a pad/button/pedal on your device. The mapping appears and pressing it switches slots.
- [ ] Learn a foot switch for **Next slot** and another for **Previous slot**: do they walk through the loaded slots and wrap around?
- [ ] Learn a fader or knob for **Volume of a slot** and another for **Master volume**. With "Pick up", does the knob do nothing until it reaches the current value, then follow it?
- [ ] Map **Select the slot given by the number sent** to Program Change, then send program changes from your keyboard (or use its patch buttons).
- [ ] A mapped key or pedal must no longer play the instrument; unmapped ones still do.
- [ ] Save the rig, close, reopen: the mappings are still there.

## 5b. Snapshots (no hardware needed except the last three items)
Snapshots are saved inside the rig file, so one file holds the whole setup.
- [ ] Load 3 slots. Make a sound you like (slot 1 plays, volumes set, one effect on) and press **+ Save snapshot** in the strip under the title bar. Change things (select slot 3, lower a volume, mute a slot, switch an effect off, pick another SoundFont preset), save a second snapshot, then click between them. Each click restores: which slots play, volumes, mute, zones, effects on/off, insert bypass, SoundFont preset, master volume.
- [ ] Right-click a snapshot: rename it, **Update with what is set now**, **Remove**.
- [ ] Recalling a snapshot while holding a key: no stuck note, no pop you wouldn't expect.
- [ ] Save the rig, **New**, reopen it: the snapshots are in the strip and still recall correctly.
- [ ] Open a rig that has *fewer* instruments loaded than the snapshot expects (unload a slot after saving it): recalling works and nothing crashes.
- [ ] Settings → Controls → *Recall a snapshot* → Learn → press a pad/button. Pressing it recalls that snapshot.
- [ ] *Next snapshot* and *Previous snapshot* on a foot switch step through the strip and wrap.
- [ ] *Recall the snapshot given by the number sent* on Program Change: patch 0 is the first snapshot. After a recall, a mapped knob does nothing until it reaches the value (if set to Pick up).

## 5c. Perform view
- [ ] Press **Perform** in the title bar (or F11). The editor disappears and you get snapshot buttons, one tile per loaded instrument, the master fader and PANIC. Click a snapshot: it recalls and lights up.
- [ ] Click a tile's name: only that instrument plays (its border lights). **+** layers it with the others. **MUTE** mutes it. The tile's slider changes its volume and the bar above it moves while you play.
- [ ] **Full screen** toggles the window; F11 or **Leave** (or Escape) goes back to the rack, and the editor of the selected slot returns.
- [ ] **Lock**: then **Leave**, Escape and F11 do nothing, and dropping a file on the window is ignored (the log says so). A quick click on **LOCKED** does nothing; holding it for about a second (a yellow bar fills) unlocks.
- [ ] With a controller: snapshot and slot mappings keep working while locked; the tiles follow.
- [ ] On a touch screen, if you have one: are the buttons big enough, does holding to unlock work?
- [ ] Window start: launching the app from a terminal while typing elsewhere must not steal your typing focus (needs your KDE setup; see the log line "could not ask for a quiet start" if it fails).

## 5d. Macros
- [ ] Load a SoundFont and a CLAP/VST3 synth. Select the SoundFont, open the **Macros** tab. On macro 1 press **+ parameter...**: pick *Brightness* and *Level*. Drag the knob: the sound gets brighter and louder together. The **x** removes a target.
- [ ] On the synth: add its filter cutoff and one more parameter to a macro (use the search box). The knob moves them, and the plugin's editor (Plugin tab) shows the new values.
- [ ] Set a target's *from* and *to* to 100% and 0%: the parameter runs backwards.
- [ ] Rename a macro; it keeps the name after Save, New, reopen. The knob position and its targets are restored.
- [ ] Adding the *first* target to a macro must not change the sound by itself.
- [ ] Save a snapshot with a macro at 0.2, move the macro to 0.9, recall the snapshot: the macro and the sound go back.
- [ ] Settings > Controls: *Macro of a slot (knob or fader)*, pick slot and macro, **Learn...**, turn a knob. The knob moves the macro (Pick up: only after it passes the current position).
- [ ] Perform view: macros that have targets appear as sliders on their instrument's tile.
- [ ] Try a Windows plugin (Surge XT Windows, Six Sines) as a macro target; the app must stay responsive.

## 5e. Knob options and encoders (needs a controller with knobs, ideally an endless encoder)
- [ ] Map a fader to *Master volume*, press **options**: *Invert* flips it; *range* 20% to 60% confines it; *Soft*/*Hard* change how it feels near the ends.
- [ ] Takeover *Scale*: set the master to about 80%, put the knob low, turn it up. The volume rises a little and both reach the top together, with no jump.
- [ ] Endless encoder: map it to *Master volume* or a macro, **options** > kind. Try the three encodings until turning right goes up and left goes down by one step per click. Adjust *step*.
- [ ] *Macro of the slot that plays*: map 8 knobs to macros 1-8. Select a slot, turn a knob: its macro moves. Select another slot: the same knobs now move that slot's macros (with Pick up, only after they reach the macro's position).
- [ ] *Volume of the slot that plays* behaves the same for one fader.
- [ ] Save, reopen: options (range, curve, kind) and takeover are kept.

## 5f. Setlist, songs and parts
- [ ] **Setlist** (title bar) > **+ Add song**. Name it, set a tempo. Set the rig up the way the first part should sound, press **+ Part from the current sound**; change the sound, add another part; add a tempo override on one.
- [ ] **Go** on a part: the sound and the tempo (shown in Perform) change to that part. The snapshot strip shows the part's snapshot lit.
- [ ] Reorder songs and parts with **^ v**; delete a part, and delete a snapshot a part uses (the part just shows "no snapshot").
- [ ] Perform view: **Next part >** and **< Prev part** walk through the song and into the next song; part buttons jump; the song buttons jump to a song. The cue note shows large.
- [ ] Settings > Controls: *Next part* / *Previous part* on foot switches (or pads): the same walk, from the pedals.
- [ ] Save, **New**, reopen: songs, parts, tempos and notes are back and still recall the right sounds.

## 5g. Transport, click and MIDI clock
- [ ] Perform view: press **START**. With count-in 1 bar you hear four clicks (the first higher), the display says "count-in n", then "1 . 1, 1 . 2, ..." as the bars run. **STOP** stops it.
- [ ] **-**/**+** and **TAP** change the tempo (tap four times); the click follows at once. **CLICK** and its slider mute it and set its level. Count-in off starts straight away.
- [ ] Settings > Audio > Click output: with a multi-output interface pick two of its outputs: the click leaves the main mix and comes out there only (headphones). Back to "Main outputs" mixes it in again.
- [ ] Enter a part that has its own tempo (Setlist): the click follows it. Change beats per bar for a song (Setlist) or in Settings > Audio and listen for the accent on beat 1.
- [ ] MIDI clock: connect the `merlin:midi_out` port to a drum machine or a software that shows incoming tempo (`pw-link merlin:midi_out <port>`); it follows the tempo and starts and stops with START and STOP.
- [ ] Settings > Controls: *Start / stop*, *Click on / off*, *Tap tempo* on a foot switch or pad.
- [ ] Close and reopen the rig: tempo, count-in and click settings are back.

## 5h. Backing tracks
- [ ] Setlist > a song > **+ Add track**: type the path of a stem (or drop a file on the window while the Setlist screen is open). It says *loading...* then *ready 3:07*. A file that is not audio says *cannot play* (hover for why).
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

## 6. Stability
- [ ] Kill a Windows plugin's Wine process while the app runs: `pgrep -af yabridge-host`, then `kill -9 <pid>` of one. The app should restart itself from the autosave and name the plugin it quarantined (it comes back with *Retry*).
- [ ] The 2-hour rehearsal: 6–8 instruments including a Windows one, switching and playing for 2 hours. For an unattended version: `just soak 7200 3 --windows "Six Sines"`, then read the `state:` lines (xruns, held keys, RSS, threads). Expect 0 xruns, 0 held keys at the end, memory flat.
- [ ] Song soak: `just soak-song` (100 passes, about 8 minutes, nothing else using the audio): prints `ok: 100 passes...`. With your real stems and a real song, play it 20 times in a row from the Perform view with the foot switches and tell me about any drift, gap or stuck state.
- [ ] Close the app normally a few times: no crash dialog, no leftover `yabridge-host` processes (`pgrep -af yabridge-host`).

## 7. Known, not worth reporting
- Surge XT (VST3) shows a text-cursor mouse pointer everywhere in its editor.
- The built-in effects are basic and need a lot more work.
- Cardinal (Windows) is refused on purpose: it crashes Wine.
