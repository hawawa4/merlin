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

## 6. Stability
- [ ] Kill a Windows plugin's Wine process while the app runs: `pgrep -af yabridge-host`, then `kill -9 <pid>` of one. The app should restart itself from the autosave and name the plugin it quarantined (it comes back with *Retry*).
- [ ] The 2-hour rehearsal: 6–8 instruments including a Windows one, switching and playing for 2 hours. For an unattended version: `just soak 7200 3 --windows "Six Sines"`, then read the `state:` lines (xruns, held keys, RSS, threads). Expect 0 xruns, 0 held keys at the end, memory flat.
- [ ] Close the app normally a few times: no crash dialog, no leftover `yabridge-host` processes (`pgrep -af yabridge-host`).

## 7. Known, not worth reporting
- Surge XT (VST3) shows a text-cursor mouse pointer everywhere in its editor.
- The built-in effects are basic and need a lot more work.
- Cardinal (Windows) is refused on purpose: it crashes Wine.
