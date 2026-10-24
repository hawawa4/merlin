# QA checklist

Things only a person with ears and a mouse can check. Run `just dev` unless a step says otherwise.
Sections 0 to 2 (frame, rack and effects, SoundFonts, SoundFont folders) and 5 (controls) are checked and gone. Tick as you go; anything odd, send me what you saw (and the exact text of any error in the log line under the title).

## 0. Re-check: fixes from your last report
- [ ] **Text size and contrast.** Open **Settings** (every tab) and **Setlist** (add a song, a part, a track): all text is readable at a normal distance, the small grey help lines included. Anything still too small or too dim? Say which tab.
- [ ] **The header never changes.** Click RACK, SETLIST, PERFORM, SETTINGS: the rig plate, the view switch, the three icon buttons, master and PANIC stay exactly where they are. In Perform, New / Open… / Save and the keyboard button are there but greyed out; **LOCK** and **FULL SCREEN** are now in the *Stage* strip right under the notice line. With the lock on, the view switch and MIDI MAP are greyed (not gone).
- [ ] **Folder dialogs.** Settings > Plugins: **Add folder…** opens your desktop's folder chooser (KDE's or GTK's); choosing a folder adds it (and a rescan finds its plugins); cancelling adds nothing. Same for the sound folders, Settings > MIDI > MIDI capture > **Choose…** (and **Default**), Settings > Windows plugins > **Browse…** for the Wine program, the yabridge folder and the installer, and **Add folder…** for Windows VST2 folders. No box asks you to type a path to add (the Wine and yabridge boxes can still be typed in).
- [ ] **Tempo and metronome in the header** (left of the level meter): a metronome icon and the beats per minute. Drag the number, scroll the wheel over it, or click it and type: the tempo changes everywhere (Perform's tempo, the click, MIDI clock). It is the same number in every view. Click the metronome icon: it lights and clicks at that tempo, with a higher click on beat 1 of the bar (beats per bar: Settings > Audio, or the song's); click again to stop. START in Perform takes the click over without doubling it; the metronome keeps going when the song stops. The two click sounds are the ones from Lyrica; do they feel right? *Metronome on / off* can be mapped to a pedal (Settings > Controls).
- [ ] **Plugins follow the tempo.** Load a synth or effect that syncs to the host (an arpeggiator or a tempo-synced delay or LFO: Surge XT as CLAP and as VST3, a Windows plugin). Change the header tempo: the plugin's rate follows. (VST2 already did; CLAP and VST3 used to be told 120 whatever it was, and LV2 is still not told: tell me if you have an LV2 that needs it.)
- [ ] **Level meter** in the header: two rows of lamps (left, right), green to amber to red, moving while you play and falling back smoothly; a lamp stays at the peak for a moment; the round lamp at the end lights red for two seconds if the output reaches full scale (turn master up with a loud patch). Hover for the explanation. Does it cost noticeable CPU when idle (it should stop redrawing when silent)?
- [ ] **The header fits.** At your usual window size everything is on one line; make the window narrower and the fader and meter shrink, then the levels and tempo drop to a second line. Nothing overlaps at any width.
- [ ] **Setlist** is only about choosing: no scene strip there and no way to make a scene (they are made in the Rack view); a part has a scene picker (with a hint when there are none yet) and **+ Add part**. The **^ v x** buttons of each song sit in one column at the right of its row, never over the name; a long name is cut short with …. Songs, parts, tracks and markers wrap instead of running off the edge when the window is narrow.
- [ ] **Backing tracks in the Setlist.** **+ Add tracks…** opens a file chooser (several files at once, audio only) and adds them to the song being edited. Settings > Audio > **Backing tracks folder** > Choose…: that folder appears in the Setlist view's right-hand **Backing tracks library**: click a folder to go in, **Up**, **Refresh**, **Change…**, the filter box narrows the audio files by name, double-click a file to add it as a track to the selected song (hover shows its path and size). Only folders and audio files are listed, hidden ones are not. Without a folder chosen the panel offers a button to choose one. **Clear** in Settings empties the choice.
- [ ] **Scenes** (see 5b below): rename by double-click, overwrite, and Save also saves the lit scene.
- [ ] **Browser** (see 5o below): a small picture beside each family, instrument and plugin; right-click works every time; double-click fills the first empty slot.
- [ ] **Mouse wheel on a plugin's scroll bar** (Podolski, or any editor bigger than the dock): turn the wheel over the empty room or the bar beside the plugin. It should now glide, one notch moving the picture the same way every time, as smooth as dragging the thumb. If it still skips, tell me which plugin and whether the wheel was over the bar or the empty room.

## 3. Windows plugins
- [ ] Windows plugins are in the browser without pressing anything. Load Podolski and TyrellN6 (VST3 and CLAP). Play, use the editor, save and reopen.
- [ ] **Silence check:** load Podolski or TyrellN6 ten times (load, play a chord, unload). Does it ever stay silent? Note whether it was right after loading or after reopening a rig. (Known: about 1 start in 10 under my test runs.)
- [ ] Settings → Windows: the doctor lines are green. Optional: *Download Windows plugin support* needs a published runtime; to try it locally, run `just package-runtime`, then `python3 -m http.server -d dist/runtime`, set `runtime_url = "http://127.0.0.1:8000/runtime-manifest.json"` in `~/.config/merlin/settings.toml`, and start the app **without** `just dev` (it uses its own Wine).
- [ ] Optional, if you have a Windows plugin installer: drop its `.exe`/`.msi` on the window. Does the installer window appear, and do the plugins show up afterwards?

## 4. MIDI (no hardware needed for the first part)
- [ ] In a second terminal run `just fake-keyboard`. Settings → MIDI devices: "Fake Keyboard" appears with a light that flashes. Set it to **Controller**: the notes stop playing instruments. Back to **Keyboard**: they play. Untick **use**: it disconnects.
- [ ] When the fake keyboard exits, the log says the device is gone; run it again and it comes back with the role you set.
- [ ] With a real keyboard (later): what is it called in the list? Does it play? Does unplugging and replugging it behave?

## 5b. Scenes (no hardware needed except the last three items)
Scenes are saved inside the rig file, so one file holds the whole setup.
- [ ] Load 3 slots. Make a sound you like (slot 1 plays, volumes set, one effect on) and press **+ Store scene** in the strip under the title bar. The new scene's name opens for typing at once: type one and press Enter (Escape keeps *Scene N*). Change things (select slot 3, lower a volume, mute a slot, switch an effect off, pick another SoundFont preset), store a second scene, then click between them. Each click restores: which slots play, volumes, mute, zones, effects on/off, insert bypass, SoundFont preset, master volume.
- [ ] **Double-click** a scene: its name becomes a text field with the old name selected. Enter or a click elsewhere keeps the new name, Escape drops it. A single click still just recalls it.
- [ ] **Pre-create, then tune:** press **+ Store scene** four times, naming each (Verse, Chorus...). Click *Chorus*, change a few things, press the rig's **Save**: *Chorus* now holds those changes (click *Verse*, then *Chorus* again to see) and the other scenes are untouched.
- [ ] Right-click a scene: **Rename** (same text field in the strip), **Overwrite with what is set now**, **Remove**. The menu has no text field in it any more, so it stays open until you pick something.
- [ ] Recalling a scene while holding a key: no stuck note, no pop you wouldn't expect.
- [ ] Save the rig, **New**, reopen it: the scenes are in the strip and still recall correctly.
- [ ] Open a rig that has *fewer* instruments loaded than the scene expects (unload a slot after saving it): recalling works and nothing crashes.
- [ ] Settings → Controls → *Recall a scene* → Learn → press a pad/button. Pressing it recalls that scene.
- [ ] *Next scene* and *Previous scene* on a foot switch step through the strip and wrap.
- [ ] *Recall the scene given by the number sent* on Program Change: patch 0 is the first scene. After a recall, a mapped knob does nothing until it reaches the value (if set to Pick up).


## 5c. Perform view
- [ ] Press **PERFORM** in the header (or F11). The header stays exactly as in the other views (New / Open… / Save and the keyboard button greyed out); LOCK and FULL SCREEN sit in the *Stage* strip below it. The editor disappears and you get scene buttons, one tile per loaded instrument, the master fader and PANIC. Click a scene: it recalls and lights up.
- [ ] Click a tile's name: only that instrument plays (its border lights). **+** layers it with the others. **MUTE** mutes it. The tile's slider changes its volume and the bar above it moves while you play.
- [ ] **Full screen** toggles the window; F11, Escape or **RACK** in the switch goes back to the rack, and the editor of the selected slot returns.
- [ ] **LOCK** (in the Stage strip): then the view switch and MIDI MAP are greyed, Escape and F11 do nothing, and dropping a file on the window is ignored (the strip under the header says so). A quick click on **LOCKED · HOLD TO UNLOCK** does nothing; holding it for about a second (a white bar fills) unlocks.
- [ ] With a controller: scene and slot mappings keep working while locked; the tiles follow.
- [ ] On a touch screen, if you have one: are the buttons big enough, does holding to unlock work?
- [ ] Window start: launching the app from a terminal while typing elsewhere must not steal your typing focus (needs your KDE setup; see the log line "could not ask for a quiet start" if it fails).

## 5d. Macros
- [ ] Load a SoundFont and a CLAP/VST3 synth. Select the SoundFont, open the **Macros** tab. On macro 1 press **+ parameter...**: pick *Brightness* and *Level*. Drag the knob: the sound gets brighter and louder together. The **x** removes a target.
- [ ] On the synth: add its filter cutoff and one more parameter to a macro (use the search box). The knob moves them, and the plugin's editor (Plugin tab) shows the new values.
- [ ] Set a target's *from* and *to* to 100% and 0%: the parameter runs backwards.
- [ ] Rename a macro; it keeps the name after Save, New, reopen. The knob position and its targets are restored.
- [ ] Adding the *first* target to a macro must not change the sound by itself.
- [ ] Save a scene with a macro at 0.2, move the macro to 0.9, recall the scene: the macro and the sound go back.
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
- [ ] **Setlist** (title bar) > **+ Add song**. Name it, set a tempo. Make two scenes in the Rack view first (*+ Store scene*). In the Setlist add parts with **+ Add part** and pick a scene for each; add a tempo override on one.
- [ ] **Go** on a part: the sound and the tempo (shown in Perform) change to that part. The scene strip shows the part's scene lit.
- [ ] Reorder songs and parts with **^ v**; delete a part, and delete a scene a part uses (the part just shows "no scene").
- [ ] Perform view: **Next part >** and **< Prev part** walk through the song and into the next song; part buttons jump; the song buttons jump to a song. The cue note shows large.
- [ ] Settings > Controls: *Next part* / *Previous part* on foot switches (or pads): the same walk, from the pedals.
- [ ] Save, **New**, reopen: songs, parts, tempos and notes are back and still recall the right sounds.

## 5g. Transport, click and MIDI clock
- [ ] Perform view: press **START**. With count-in 1 bar you hear four clicks (the first higher), the display says "count-in n", then "1 . 1, 1 . 2, ..." as the bars run. **STOP** stops it.
- [ ] **-**/**+** and **TAP** change the tempo (tap four times); the click follows at once. **CLICK** and its slider mute it and set its level. Count-in off starts straight away.
- [ ] Settings > Audio > Click output: with a multi-output interface pick two of its outputs: the click leaves the main mix and comes out there only (headphones). Back to "Main outputs" mixes it in again.
- [ ] Enter a part that has its own tempo (Setlist): the click follows it. Change beats per bar for a song (Setlist) or in Settings > Audio and listen for the accent on beat 1. The header's metronome does the same without a song running (see 0).
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
- [ ] Settings > Windows plugins > *Folders with Windows VST2 plugins*: add your VstPlugins folder. The 64-bit DLLs appear in the browser as `(VST2) (Windows)` after a minute or two (each one is started once to read its name). Load a few: do they play, show their editor, and come back after Save / New / reopen?
- [ ] Which of your VST2 plugins do NOT work (no sound, no editor, crash)? Send me the names. (Pianoteq 6 Stage shows its Activation window in the app's own Windows environment, which is its licence check, not a bug: activate it there once.)
- [ ] A native Linux `.so` VST2 plugin in `~/.vst` or `~/.lxvst`, if you have one: it should appear without any setting.
- [ ] Kontakt/Reaktor-type plugins that are huge: how long do they take to load, and does the app stay responsive?
- [ ] An effect (reverb, EQ) as an insert on a slot.

## 5k. LV2 plugins
- [ ] LV2 plugins in `~/.lv2` or `/usr/lib/lv2` appear in the browser as `(LV2)` without any setting (the first scan reads their description files, quick). Load a few synths and effects: do they play, show their editor, and come back from a saved rig? Tell me which do not (and the message in the log line, e.g. "needs <feature>").
- [ ] A plugin with a GTK or Qt editor (not X11): it will play but its editor will not open (expected for now).
- [ ] Insert an LV2 effect on a slot (Effects drawer > + Load effect).
- [ ] The Macros tab > *+ parameter...* on an LV2 plugin lists its control ports (plugins that use only patch parameters list none).

## 5l. MIDI map mode: any control on any controller
Works with any device that sends CC, notes or Program Change (this is the "every device is supported" route; the Novation-specific one comes later). No hardware: run `just fake-keyboard` or the pretend controller to get something to learn from.
- [ ] Click **MIDI map** in the header (or Ctrl+M). Every mappable control gets a blue tint: the slot names, **M**, **vol**, the master slider, PANIC and the scene buttons. Clicking one turns it amber ("move a control...") instead of using it. Move a knob or press a button on your device: the tint turns green and shows the source (`CC 14`).
- [ ] Right-click a green control: *Remove the mapping*. Click a green control and move another knob: the new one replaces the old one. Escape (or the button) leaves map mode; then the controls work normally again.
- [ ] Map the slot volume of slot 1 to a fader, then leave map mode and move the fader: the **vol** number follows. Do the same with a button on **M** (mute), the master slider and a scene button.
- [ ] Effects drawer (map mode on): the four pedal footswitches and the inserts' **ON** buttons can be mapped. A pad flips the effect each press. In Settings > Controls, set a switch mapping to **Latching switch** and use a foot switch that stays down: down = on, up = off, whatever it was before.
- [ ] Macros tab: each macro slider can be mapped (same as before, now by clicking it).
- [ ] **MIDI LEARN** (next to the tabs, while a plugin's editor is shown): click it, then turn any knob in the *plugin's own window*. The button stays lit while it waits; the strip under the header says *Turn a knob in the plugin's own window…*, then *Now move a control on your device to map “<name>”* once the app has found the knob. Move a knob on your controller: it is mapped and the plugin's knob follows it. Try it on a CLAP, a VST3, an LV2 and a Windows plugin, and on an insert (its tab). Tell me which plugins it does not find the right knob for (nothing moves, or a different one is picked). Learning a parameter that already has a control replaces that control. It gives up after 30 seconds, and clicking the button again cancels.
- [ ] **Parameter list** tab (next to Macros; it was called Parameters): every parameter of the instrument (and of an insert, via the drop-down) with a **Map** button. Press it, move a knob: the plugin parameter follows the knob. Try a cutoff or a volume you can hear. Does the search box find what you look for? Do very long lists (Kontakt, Reaktor) scroll smoothly?
- [ ] Not read back: if you change a mapped parameter in the plugin's own window, a knob set to *Pick up* or *Scale* does not know; use *Jump* for those (or tell me if this bothers you). (MIDI LEARN only *reads* the plugin's values while it looks for the knob you touch.)
- [ ] Perform view: the **MIDI map** button is there too (not while locked). Map START/STOP, TAP, CLICK, the tempo number (click the "120 bpm"), the click-level slider, the part buttons.
- [ ] Settings > Controls still lists everything, with the parameter's name next to its mapping. New there: *Pan of a slot*, *Click level*, *Tempo*.
- [ ] Save the rig, open it again: the mappings (including parameter ones) are still there and still work.
- [ ] No hardware: `just e2e-controls` now also checks pan, effect switches (flip and latching), click level, tempo, a Surge XT parameter, a reverb knob and a SoundFont knob, and learning a parameter by touch.

## 5m. Launchkey MK3 (your keyboard)
Plug it in before starting `just dev` (or while it runs: the log says `controller: Launchkey MK3 49 found`). The keys, wheels and pedal work as a keyboard as before; the DAW port is taken as a controller, put into DAW mode, and given a built-in layout. When the app quits the device goes back to normal. No setup needed.
- [ ] **Lights, first**: load 3-4 instruments. The top pad row lights one pad per loaded slot in that slot's colour; the one that plays pulses. The pads under the faders' buttons (the round buttons below the faders) glow white for loaded slots. Save two scenes (+ Store scene): the bottom pad row lights blue for each, green for the one recalled last. Tell me what you actually see: wrong colours, dark pads, anything that stays lit after the app closes.
- [ ] **Screen**: top line shows the slot that plays (`1 Piano`), bottom line the scene or song part and tempo (`Snap 2 120bpm`). Turn a knob: the screen shows the macro's name and percentage (for the slot that plays). Move a fader: `Volume 1` and the value. Which line is which row, is any text cut off or wrong?
- [ ] **Pads**: top row selects slot 1-8; bottom row recalls scene 1-8. Do the lights follow at once?
- [ ] **Track < / >** = previous / next slot: `<` moves up the rack, `>` down (on the real device they send the opposite CCs from the guide's diagram; checked with `just launchkey-probe`). Scene up/down (previous/next part) follow the guide and are not verified on the device yet. **Scene up/down** = previous/next song part (needs a setlist: Setlist screen). **Play** = start/stop the transport, **Click** = click on/off, **Quantise** = tap tempo.
- [ ] **Knobs 1-8** work without any setup on the slot that plays: 1 Pan, 2 Reverb, 3 Delay, 4 Bass, 5 Mid, 6 Treble (EQ, flat in the middle), 7 Delay time, 8 Brightness (SoundFonts only). Reverb, Delay and EQ switch themselves on when you turn the knob up and off when it is back at the bottom or flat (see the Effects drawer follow). The screen shows the name and value (`Reverb 35%`, `Bass +3dB`, `C`/`L40`/`R25`). If you put something in macro 1-8 of the slot (Macros tab) that macro takes the knob instead. All knobs pick up: after changing slot a knob does nothing until it reaches the current value. Do these feel right, are the ranges useful (is the reverb too much at the top, the delay time wrong)? Which other things should be on knobs?
- [ ] **Pad modes** (the Shift menu on the device): *Session* (the default) is what is described above. *Drum*: the 16 pads play notes on channel 10 of the MIDI port into the slot that plays (try a SoundFont drum kit), and now glow in that slot's colour instead of staying dark. *Device Select* (hold the Device Select button): the pads become two rows, top = select slot 1-8 (lit like the Session row), bottom = add/remove the slot as a layer (bright = it plays). Release it and the Session pads are back. *Scale chords / User chords / Custom*: the device does these itself and the app leaves the pad lights alone. Tell me if a mode leaves wrong lights behind when you switch.
- [ ] Start the app with the Launchkey already plugged in (the case that once did not connect): the log should say `controller: Launchkey MK3 49 found` and the pads light within a second. (Found and fixed one cause: the first message, which switches on DAW mode, could be sent before the connection to the device existed and was lost; it now waits 0.3 s.)
- [ ] Switching slot with the controller used to feel late because the new slot's editor opens first. The lights and screen should now follow at once, before the editor appears; does it?
- [ ] **Faders 1-8** = volume of slots 1-8, **fader 9** = master (49/61/88-key models), also with pick-up. The round buttons under them mute slots 1-8 (the light dims), the ninth one is Panic.
- [ ] Settings > Controls > *Built-in layout of connected controllers* lists all of it. Map one of the same controls yourself (MIDI map mode works with them too): yours wins and the built-in row is crossed out.
- [ ] Unplug the USB cable and plug it in again: it gets its lights back by itself. Quit the app: pads dark and the keyboard works as before.
- [ ] `just launchkey-probe`: press and turn every control; the names it prints should match the controls (it also shows the CC numbers). Send me any line that says `CC <n>` without a name, or a name that is wrong.
- [ ] No hardware: `just e2e-controls` plays a pretend Launchkey MK3 and checks the layout and the lights.

## 5n. MIDI capture (the last 30 minutes, saved by a button)
The app always listens to your keyboards and the on-screen keys. Nothing is written until you press **Capture MIDI** (header, or **CAPTURE** in the Perform view). Files go to `~/Music/Merlin`.
- [ ] No hardware: `just e2e-controls` includes a capture check: a pretend keyboard plays notes, a pedal and a note that is still held, a mapped control presses the capture button, and the `.mid` file must have the right messages and timing.
- [ ] Play something on a keyboard (or `just fake-keyboard`) for a minute, press **Capture MIDI**: the strip under the header says `saved 1:00 of MIDI to ...` in green for a few seconds. Open the file in a sequencer (Ardour, Reaper, Bitwig, MuseScore): is there a track named after your keyboard, are the notes where you played them, does the first note sit at the start, is the tempo the app's tempo?
- [ ] Set the tempo to 90 in the app and play along with the click for a few bars: in the sequencer the notes should fall near the bar lines at 90 bpm.
- [ ] Hold a chord and press the button with the other hand (or the mapped foot switch): the chord is in the file and ends where you pressed. A sustain pedal held down is released at the end.
- [ ] Press it right after starting the app: it says nothing was played. Press it twice in a row: two files, the second with ` (2)` if it was in the same second. Neither empties the buffer.
- [ ] Settings > MIDI > MIDI capture: change *Keep the last* to 1 min, play, wait two minutes, play a few notes, capture: only the last minute is in the file. 0 turns it off (the buttons grey out). Change *Save to* (Enter to apply) and capture: it goes there. *Default* brings back `~/Music/Merlin`.
- [ ] MIDI map mode: click **Capture MIDI**, press a pad or a foot switch: it is mapped. A controller-role device (the Launchkey's pads) and anything mapped never appears in the file.
- [ ] A long session: leave it running for an hour of normal playing and check memory (Settings > Performance, or `ps`): the capture should stay a few MB at most.

## 5o. Browser: icons, double-click, favourites and tags
- [ ] Every family branch has a little picture (Keys a keyboard, Strings a violin, Guitars a guitar, Woodwinds a clarinet, Brass a trumpet, Voices a microphone, Percussion a drum, Synth a waveform, World a globe, Multi-instrument a rack, Other a sparkle), instruments with their own look have one too (Organ pipes, Harp, Saxophone, French Horn), and each plugin row shows the picture of its first tag (a plain circle with *?* under *Untagged*). Do they read at a glance? Are any wrong or ugly?
- [ ] **Double-click** a plugin: it loads into the first empty slot (not the slot you are looking at), and double-clicking several in a row fills slots 1, 2, 3... Dragging onto a slot still works, with the name following the pointer.
- [ ] **Right-click** a plugin name: the menu opens every time, also if you hold the button a moment or wiggle the mouse a little. Dragging with the left button still starts a drag, and a right-drag does not.
- [ ] Click the star beside a plugin: it fills yellow where it is (the list does not move). **Favourites** (above the list) shows only starred plugins. Quit and reopen: still favourites.
- [ ] Right-click a plugin: *Favourite*, then *Tag as* with a submenu per family (Keys, Brass, Woodwinds...) and the instruments inside (Piano, Trumpet, Flute...). Tick *Keys > Piano*: the menu stays open; also tick *Synth > Pad*. Click outside. The plugin shows `Keys › Piano, Synth › Pad` under its name, and the family's name is gold in the menu.
- [ ] Under the search box, choose the family *Keys*: only plugins tagged Keys remain, and the second list (*Any instrument*) becomes usable; pick *Piano*. *Untagged* lists what you have not tagged yet. It combines with the search box and **Favourites**.
- [ ] Typing `piano` in the search box also finds plugins you tagged Piano.
- [ ] Automatic tags: without tagging anything, plugins the app knows (Surge XT, u-he synths, SoundFonts like MuseScore General) already show a tag in *italics* under their name (hover: "Tagged automatically"), and the family filter finds them. Plugins nobody knows stay untagged: *Untagged* lists them.
- [ ] Right-click an automatically tagged plugin and tick another tag: the automatic ones are ticked in the menu already, and after your click the line is no longer italic (it is yours now). Rescan (Settings > Plugins) and *Use automatic tags* in the menu: your tags are not touched by the rescan, and the menu entry brings the italics back.
- [ ] *Clear tags* on an automatically tagged plugin leaves it with none, even after a rescan or restart (it's your choice, not a gap to fill).
- [ ] Right-click > *Clear tags* removes them. Quit and reopen: tags are still there (they are in `~/.config/merlin/settings.toml` under `[tags]`).

## 6. Stability
- [ ] Kill a Windows plugin's Wine process while the app runs: `pgrep -af yabridge-host`, then `kill -9 <pid>` of one. The app should restart itself from the autosave and name the plugin it quarantined (it comes back with *Retry*).
- [ ] The 2-hour rehearsal: 6–8 instruments including a Windows one, switching and playing for 2 hours. For an unattended version: `just soak 7200 3 --windows "Six Sines"`, then read the `state:` lines (xruns, held keys, RSS, threads). Expect 0 xruns, 0 held keys at the end, memory flat.
- [ ] Song soak: `just soak-song` (100 passes, about 8 minutes, nothing else using the audio): prints `ok: 100 passes...`. With your real stems and a real song, play it 20 times in a row from the Perform view with the foot switches and tell me about any drift, gap or stuck state.
- [ ] Close the app normally a few times: no crash dialog, no leftover `yabridge-host` processes (`pgrep -af yabridge-host`).

## 7. Known, not worth reporting
- Surge XT (VST3) shows a text-cursor mouse pointer everywhere in its editor.
- The built-in effects are basic and need a lot more work.
- Cardinal (Windows) is refused on purpose: it crashes Wine.
