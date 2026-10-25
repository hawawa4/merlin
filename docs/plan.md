# Merlin: Product and Technical Plan

*Draft v2, 2026-10-18. Research: [research.md](research.md). "Merlin" is a working name.*

**Changes since v1:**
- Licence fixed as GPLv3.
- Plugin editors embedded in the main window, never in separate windows.
- Built-in SoundFont (SF2/SF3) instrument added.
- VST2/LV2 moved to the end.
- Novation integration moved before setlists and planned tracks.
- x86_64 only.

## 1. Vision

**A Linux-first plugin host for playing live.** Plug instruments into a rack, pick one (or layer or split several) from your keyboard, and play. A little more than a plugin host, a lot less than a DAW.

> Cantabile's live model, MainStage's polish, **without the routing and without the tracks.**

### Design principles

1. **No routing UI.**
   - Signal flow is fixed and invisible: *MIDI in → active instrument(s) → that instrument's effects → master out*.
   - The only routing choices are a slot's MIDI input filter (default: every input) and, later, an output for click and backing tracks.
2. **No tracks, mixer, buses or sends.** The rack *is* the project.
3. **Plugins and sounds just work.**
   - VST3 and CLAP, Windows plugins through a bundled, tested Wine + yabridge, and SoundFonts.
   - VST2 and LV2 come later.
   - Users never type `yabridgectl` or `winecfg`.
4. **One window.** Plugin editors are embedded inside the app and never open as separate windows.
5. **Built for the stage.**
   - Switching instruments never drops audio or leaves notes stuck.
   - Everything is preloaded.
   - Panic always works.
   - It recovers from crashes.
6. **Polish matters.** Rack units look like gear. Settings fit on one screen per topic.
7. **Every user intent is an `Action`.** UI clicks, shortcuts, MIDI mappings, Launchpad pads and timeline markers all dispatch the same actions, so later features are add-ons, not rewrites.

## 2. Scope

| | In |
|---|---|
| **v1 (MVP)** | Rack of instrument slots.<br>Per-slot built-in effects (EQ, compressor, delay, reverb), plus up to 4 plugin inserts.<br>Per-slot MIDI filter (input, channel, key zone, transpose).<br>Exclusive or layered selection.<br>Held notes and tails carry over a switch.<br>Panic.<br>CLAP + VST3 hosting with **embedded editors**.<br>Scanner and browser.<br>Settings: plugin folders, audio, MIDI devices.<br>Windows plugins (basic yabridge integration).<br>Save/load rig, autosave. |
| **Next** | Bundled Wine runtime with "Install Windows plugin…".<br>**SoundFont instrument (SF2/SF3).**<br>System check for realtime readiness.<br>Crash safe mode. |
| **Then** | MIDI learn and mapping: switch instrument by CC or Program Change.<br>Slot macros, snapshots, Perform view.<br>Novation Launchpad and Launchkey. |
| **Later** | Setlists, songs and parts.<br>"Plan a track" timeline with automatic instrument changes.<br>Backing tracks with a separate click output.<br>MIDI clock / Link. |
| **End** | VST2, LV2 (optionally SFZ via sfizz). |
| **Optional** | Out-of-process plugin sandbox.<br>Windows/macOS ports.<br>Headless stage-PC mode. |
| **Never** | Recording as a feature you arm and edit: timeline editing, piano roll, audio tracks. (The one exception is the always-on MIDI capture, §4.12: no transport, no editing, a button that saves a file.)<br>Free-form routing, sends, sidechain, buses.<br>Automation lanes.<br>Separate plugin windows.<br>ARM. |

## 3. Key decisions

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 | Language | **Rust** (2024 edition, stable) | Memory safety around heavy FFI and good real-time tooling. CLAP hosting via `clack`, MIT VST3 bindings via `vst3`. Go is ruled out: garbage-collection pauses and cgo callbacks on the audio thread. |
| D2 | Licence | **GPL-3.0-or-later** ✅ decided | Lets us use the VeSTige/FST headers for VST2 later, bundle and patch yabridge (GPL-3), and use oxisynth (LGPL-2.1) and Ableton Link (GPLv2+). |
| D3 | Audio backend | **JACK API** via the `jack` crate, preferring PipeWire's libjack automatically. `cpal` as a fallback later. | Standard pro-audio API that runs unchanged on PipeWire and JACK2. Sample-accurate JACK MIDI. |
| D4 | MIDI | **JACK MIDI** for playing. **ALSA sequencer** for device listing, hotplug and controller SysEx. | PipeWire exposes hardware MIDI as JACK ports. SysEx and Device Inquiry are easier over ALSA seq. |
| D5 | Plugin formats | **CLAP + VST3** in v1, **built-in SoundFont** in Phase 2, **VST2 + LV2** at the end | Covers almost every modern plugin plus Windows plugins (as VST3/CLAP via yabridge) early. |
| D6 | Windows plugins | **We bundle a tested Wine + yabridge pair** ✅ decided | yabridge's last release doesn't support Wine 9.22 or later, so we pin a tested `master` commit together with a portable Wine build. The user's system Wine is never touched. |
| D7 | UI toolkit | **egui** on an **event loop we own** (`egui-winit` + `egui-wgpu`, not `eframe`). **The app is always an X11 client** (XWayland on Wayland desktops). | Best custom drawing for rack gear. Owning the event loop lets plugin file descriptors and timers be serviced without redrawing our UI. Being an X11 client is what makes embedding work (D8). |
| D8 | Plugin editors | **Embedded in the main window** ✅ decided: an X11 child window placed in our layout, into which the plugin embeds (XEmbed). Never a separate window. | Every Linux plugin GUI API (CLAP, VST3, LV2, yabridge/Wine) embeds into an X11 parent window. Inside our own X11 app, no compositor tricks are needed. See §4.6. |
| D9 | Process model | **v1: one process.** Native plugins in-process; Windows plugins already run in Wine processes. Plugin interface designed so an out-of-process sandbox can be added. Scanning always in a separate process. | Lowest latency and complexity, as in Gig Performer, MainStage and Cantabile. |
| D10 | Platforms | **Linux x86_64 only** ✅ decided. Platform code behind traits, so Windows/macOS stay possible. | — |

## 4. Architecture

### 4.1 Processes and threads

```
┌──────────────────────────── merlin (single process, X11 client) ────────────────────┐
│                                                                                              │
│  JACK RT thread (PipeWire)            Main thread (our winit loop)                           │
│  ┌──────────────────────────┐  cmds   ┌────────────────────────────────────────────┐         │
│  │ Engine::process()        │◄────────│ egui UI · Action dispatcher                │         │
│  │  MIDI router             │  rtrb   │ plugin main-thread calls (create,          │         │
│  │  → slots (instr + FX)    │────────►│   activate, params, state, GUI)            │         │
│  │  → master → outs         │ meters, │ editor dock: X11 child window(s) embedded  │         │
│  └──────────────────────────┘ events  │   in the main window                       │         │
│        ▲ JACK MIDI in                 │ VST3 IRunLoop / CLAP fd+timer servicing    │         │
│                                       └───────────────▲────────────────────────────┘         │
│  Worker threads: deferred freeing,     fd-poller thread (epoll on plugin fds + X11) wakes    │
│  SoundFont loading, disk streaming     the loop with a user event                            │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
  Child processes: merlin-scan (one per plugin under test, with timeout)
                   wine + yabridge host processes (Windows plugins)
```

**Threading rules:**
- Plugin "main thread" calls run on our main thread.
- Plugins are created on the main thread, with a "Loading…" overlay. This happens during setup, not mid-song.
- The fd-poller thread wakes the winit loop. Plugin callbacks then run **without** triggering an egui repaint.

### 4.2 Cargo workspace

```
merlin/
├─ crates/
│  ├─ model/        serde types: Settings, Rig, Slot, PluginRef, (later) Mapping, Snapshot, Setlist
│  ├─ engine/       RT core: router, note ownership, slot chain, master, command/event queues. No UI deps.
│  ├─ hosting/      plugin abstraction + formats: clap (clack-host), vst3 (vst3 crate), builtin; later vst2, lv2
│  ├─ dsp/          built-in effects: EQ, compressor, delay, reverb, master limiter
│  ├─ soundfont/    built-in SF2/SF3 instrument (Phase 2)
│  ├─ audio-io/     backends: jack (primary), cpal (fallback), dummy (tests/offline render)
│  ├─ embed/        X11 editor embedding: child windows, clipping/scroll, XEmbed, focus, run loop
│  ├─ wine/         bundled Wine/yabridge runtime, prefix management, yabridgectl driver
│  ├─ scanner/      `merlin-scan` binary + plugin/sound database
│  ├─ recorder/     the rolling MIDI capture: lock-free tap, in-memory window, Standard MIDI File writer
│  └─ control/      (Phase 3+) actions, MIDI learn, mappings, controller profiles, Novation drivers
└─ app/             main binary: winit loop, egui UI, wiring
```

### 4.3 Engine

**Signal flow per audio block:**

```
JACK MIDI in ─► Router ─► per-slot events ─► Slot[i]: Instrument ─► EQ ─► Comp ─► [inserts] ─► Delay ─► Reverb ─► Gain/Pan/Mute ─┐
                                                                                                                         ├─► Master (limiter, vol) ─► Main L/R
                                                                                                         … Slot[n] ──────┘    (later: Cue/Click outs)
```

**Data model:**
- **Slot:** name, colour, instrument (`PluginRef` + state), built-in FX settings, up to 4 inserts, volume, pan, mute, `MidiFilter`, armed flag.
- **MidiFilter:** input device, channel, key zone, velocity zone, transpose/octave.

**Real-time rules** (checked by `assert_no_alloc` in debug builds and by `rtsan-standalone` in CI):
- No allocation, locks, syscalls or logging on the audio thread.
- UI → engine commands go through an `rtrb` queue. Engine → UI events (meters, note activity, CPU, xruns) go through a queue plus atomics.
- Structural changes are built off-thread and swapped in. The old state comes back on a return queue and is freed on a worker thread.
- Flush-to-zero on the audio thread.
- `mlockall` where permitted.

**Performance:**
- **Idle slots sleep.** CLAP `CLAP_PROCESS_SLEEP`; for VST3, processing stops once the tail has run out and the output is silent. Many preloaded instruments therefore stay cheap.
- **No latency compensation.** Plugin latency is reported, with a warning above a few milliseconds.
- **Multi-core:** parallel slot processing on a real-time thread pool later (Phase 2). v1 uses one audio thread.

### 4.4 MIDI routing and note ownership (what makes switching seamless)

**Note ownership table**, keyed by (input, channel, played note) → (slot, transposed note):
- Recorded on note-on.
- Note-off and poly aftertouch **always go to that slot**, whatever has been switched or re-zoned since.

**Pedal state per slot** (CC64, CC66, CC67):
- After a switch, the old slot keeps getting pedal events until the pedal comes up.
- Whether the new slot inherits a pedal that is already down is a setting (default: no).

**Switch modes:**
- **Exclusive** (default): selecting a slot makes it the only keyboard target.
- **Layer:** a toggle adds or removes slots. Zones give splits.
- **Old slot:** keeps processing so tails ring out, then sleeps. "Instant silence" and "defer until notes released" become options later.

**Panic:**
1. Synthesized note-offs from the ownership table.
2. CC64=0, CC120 and CC123 to every slot.
3. A brief master fade out and back in.

**Other controllers:** pitch bend and mod wheel follow the current target(s), and are reset on the slot that loses focus.

**Testing:** pure logic, so exhaustive unit and property tests (every note-on eventually receives its note-off).

### 4.5 Hosting layer

Each format implements one interface, split by thread the way CLAP and VST3 are:

```rust
/// Lives on the main thread: lifecycle, parameters, state, GUI.
pub trait PluginHandle {
    fn info(&self) -> &PluginInfo;                       // name, vendor, format, ports, latency
    fn activate(&mut self, sr: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>>;
    fn deactivate(&mut self, proc: Box<dyn PluginProcessor>);
    fn params(&self) -> &[ParamInfo];
    fn save_state(&mut self) -> Result<Vec<u8>>;
    fn load_state(&mut self, data: &[u8]) -> Result<()>;
    fn editor(&mut self) -> Option<&mut dyn PluginEditor>; // attach(x11_parent), size, scale, resize
    fn on_main_thread(&mut self);                          // plugin-requested callbacks
}

/// Sent to the audio thread; must be real-time-safe.
pub trait PluginProcessor: Send {
    fn process(&mut self, io: &mut AudioIo, events_in: &EventList,
               events_out: &mut EventList, transport: &Transport) -> ProcessStatus;
}
```

**Implementations:**
- `ClapPlugin` (clack-host)
- `Vst3Plugin`: our own host layer on the `vst3` bindings (IHostApplication, IComponentHandler, IPlugFrame, IRunLoop, IConnectionPoint, parameter and event queues)
- `BuiltinPlugin`: our effects and the SoundFont instrument. These have a native egui panel instead of an X11 editor.
- Later: `Vst2Plugin`, `Lv2Plugin`, and a `RemotePlugin` for the out-of-process sandbox.

**Bridged plugins** are detected by the `yabridge_version` symbol, get a "Windows" badge, and have per-plugin quirks applied.

### 4.6 Embedded plugin editors

Every Linux plugin GUI API embeds into an **X11 parent window**:
- CLAP: `gui.set_parent` with `CLAP_WINDOW_API_X11`
- VST3: `attached(kPlatformTypeX11EmbedWindowID)`
- yabridge/Wine: reparents the Windows editor into the window we pass
- LV2 (later): `ui:parent` via suil

Because the whole app is an X11 client, we can create **child windows inside our own main window** and hand them to plugins. XWayland composites the result as one window on your Wayland desktop.

**How it works:**
1. **The editor dock** is an area of the egui layout (see §5). In that area we create an X11 **child window** of the main window with x11rb and keep its position and size matched to the dock rectangle whenever the layout changes.
2. **Clipping and scrolling:** a two-level structure.
   - An outer "viewport" child window clips.
   - An inner "canvas" window is what the plugin embeds into.
   - Scrolling moves the canvas, so oversized editors scroll; smaller ones are centred on a styled background.
3. **Sizing:** the plugin's preferred size drives the canvas. Resizable plugins get the dock size, both ways (CLAP `request_resize`/`set_size`, VST3 `IPlugFrame::resizeView`/`onSize`). HiDPI scale goes to the plugin via CLAP `set_scale` and VST3 `IPlugViewContentScaleSupport`.
4. **Switching editors:**
   - One editor is visible at a time: the selected slot's.
   - Other editors stay alive but hidden (unmapped / CLAP `hide`) for instant switching.
   - Least-recently-used ones are destroyed beyond a memory cap (default about 8).
5. **Keyboard focus:** XEmbed focus handshake. Clicking into the editor gives it focus (for text fields in plugins); clicking elsewhere returns it. App-level shortcuts (panic, instrument switching) are delivered either way via the X11 key events we receive, plus MIDI.
6. **Overlap rule:** native child windows always draw *above* egui content, so egui popups, menus and modals must never overlap the dock.
   - Menus and popups open away from the dock.
   - Full-screen panels (Settings, Perform view) unmap the editor while they are shown.
7. **Fallback for floating-only plugins** (rare: CLAP plugins that can't embed, or misbehaving GUIs):
   - Find the plugin's top-level X11 window (via `_NET_WM_PID` / creation tracking) and `XReparentWindow` it into our canvas. This is Carla-style "window capture".
   - It still looks embedded.
   - Per-plugin quirk flags (`quirks.toml`) select this path.
8. **Wine/yabridge editors:** test these specifically in spike S4. yabridge master's new embedding (for Wine 10/11) and the known cursor/DPI offset issues are the main risk.

**Native Wayland (future, not needed now):** if we ever drop XWayland, we would run an embedded nested compositor (e.g. Smithay) that hosts plugin Wayland subsurfaces (VST3 3.8 preview / CLAP draft) and an Xwayland for X11 plugins. Bitwig reportedly does this. It's a large project, deferred until plugins actually ship Wayland GUIs.

**Rules learned in spikes S2/S4** (bridged Windows editors get no input without them):
- Hand the plugin a **canvas** window inside a **clip** window. Scroll by moving the
  canvas, never the plugin's own window.
- After `set_parent`, and whenever our layout moves the dock, make the canvas emit a real
  `ConfigureNotify` (resize it, or nudge it by a pixel and back). yabridge only tells Wine
  its screen position on those events; without them Wine maps every click elsewhere.
- Set `WM_WINDOW_ROLE` on our top-level window: yabridge picks the topmost ancestor with a
  role as the window whose moves it tracks.
- Accept `request_resize` without calling `set_size` with an unadjusted size (JUCE flags it).
- One canvas per slot, kept for the slot's life; show = map, hide = `gui.hide()` + unmap.
- Destroy surplus bridged editors from the main loop a few hundred ms after a switch, never in
  the same turn as showing another editor (Wine aborts the plugin host with `BadWindow`).
- While an egui popup is open, unmap the dock (§4.6 rule 6, verified in S5).
- **A crashing bridged plugin must not take the app down** (found in Phase 1 QA). When a Wine host
  dies, the next call across yabridge either blocks forever or throws a C++ exception out of the
  JACK callback, which aborts a Rust process. Our patched yabridge calls a host hook before the
  exception propagates; the app's guard quarantines the plugin, kills the other hosts and
  restarts itself from the autosave (`app/src/guard.rs`). Known-bad plugins are refused up front
  (`app/src/quirks.rs`).

### 4.7 Scanning and the sound database

**`merlin-scan`** loads one plugin in a child process with a timeout. A crash or hang puts it in **quarantine**, with a retry button.

**Cheap metadata first:**
- CLAP: factory descriptors.
- VST3: `moduleinfo.json` when present.
- SoundFonts: header and preset list only, never loading samples.
- Bridged plugins start Wine (seconds each), so they scan in the background with progress shown.

**Cache:** `~/.cache/merlin/plugins.json`, keyed by path + mtime + size. Only changed files are rescanned. inotify folder watching optional.

**Browser:**
- One list of "Instruments" and "Effects" across formats and SoundFonts.
- Search, favourites, badges (CLAP / VST3 / SF / WIN).
- One entry per plugin, even when installed in several formats; preferred format is CLAP > VST3, native > Windows.

### 4.8 Windows plugins

**v1 (basic):**
- Detect Wine and yabridge.
- "Windows plugin folders" in Settings, with standard prefix locations detected automatically.
- The app runs `yabridgectl add` and `yabridgectl sync` on the user's behalf, then scans `~/.vst3/yabridge` and `~/.clap/yabridge`. It works alongside an existing yabridgectl setup, and the user's other DAWs see the same bridges.
- Compatibility status is shown inline.

**Phase 2 (bundled runtime):**
- **"Windows plugin support"** is a one-click download into `~/.local/share/merlin/runtime/` of the **pinned portable Wine build + pinned yabridge build** (compiled by us from a tested commit), with checksums.
  - Our process sets `WINELOADER`, so only our app uses this Wine.
  - Optionally it can be bundled in the release package directly.
- **Managed prefix:** `~/.local/share/merlin/wineprefix`, configured automatically (fonts, DPI, NTSync where available).
- **"Install Windows plugin…"** lets the user pick an `.exe` or `.msi`. The app then:
  1. runs it in the managed prefix,
  2. adds the folders,
  3. syncs,
  4. scans,
  5. shows the new plugins in the browser.
- **Dropping files on the window** ✅: an installer (`.exe`/`.msi`) is installed, a `.sf2`/`.sf3` loads into a free slot, a `.mrig` opens; a banner says what the drop will do.
- **Wine Doctor panel:** versions, prefix health, per-plugin load test, yabridge options (groups, editor), logs.
- **Limits stated up front:** 64-bit plugins only; some copy protection/iLok may fail; native Linux versions preferred when available.
- **Upstream risk:** expect to contribute fixes to yabridge or keep a small patch set.

### 4.9 SoundFont instrument (SF2/SF3)

**No "SF to CLAP" conversion needed.** A built-in instrument (`BuiltinPlugin`) plays SoundFonts directly, so the UX is the same as any plugin.

**User experience:**
- Settings → Sounds: add "Sound folders" (defaults include `~/.local/share/sounds/sf2`, `/usr/share/sounds/sf2`, `/usr/share/soundfonts`).
- Every `.sf2`/`.sf3` found appears in the browser **as an instrument** (badge "SF"), with its presets listed beneath it.
  - Dropping a file adds a slot.
  - Dropping a preset adds a slot with that preset selected.
  - Dragging a `.sf2` file onto the rack from a file manager also works.
- **The rack unit shows a preset picker** (bank/program) and a small native panel: volume envelope offsets, filter cutoff/resonance, reverb/chorus send to the slot's own FX, polyphony, tuning. MIDI Program Change on the slot's input switches presets.
- **Loading:** samples load on a worker thread (with a progress indicator) and are shared when several slots use the same file. Memory use is shown.

**Engine: oxisynth** ✅ decided (spike S6; the spike code is gone from the tree, see Phase 0 below).

[oxisynth](https://github.com/PolyMeilex/oxisynth) is a pure-Rust port of FluidSynth
(LGPL-2.1, fine under our GPLv3). It sounds like FluidSynth, handles SF2 modulators and SF3
(Ogg Vorbis via `lewton`), renders at ~340× realtime, and needs no C dependency. It is
already proven on MuseScore_General.sf3 in a sibling project (musepractice). Notes for
the implementation:
- **Patch one allocation:** `inner_noteon` collects a 64-entry `Vec` on every note-on.
  Replace it with a stack array and upstream the fix; carry it via `[patch.crates-io]`
  until released. Keep an RTSan test in CI.
- **SF3 load** decodes all samples up front (~2.4 s and ~230 MiB for MuseScore General):
  load on a worker thread, share decoded fonts between slots, consider caching decoded PCM.
- **No public preset list:** add an accessor in the same upstream patch (the spike probes
  program changes instead).
- **Gain** defaults to FluidSynth's 0.2; set per slot and level-match against plugins.
- Rejected: FluidSynth itself (mutexes on every call, OpenMP in distro builds, C
  dependency) and rustysynth/rustysynth-ext (discard SF2 modulators, so many SoundFonts
  sound wrong; upstream rustysynth cannot read SF3).

**Later:** SFZ through the same mechanism with `sfizz` (BSD-2), since SFZ libraries are common for free pianos and orchestral sounds.

### 4.10 Actions (the extensibility hook)

```rust
enum Action {
    SelectInstrument(SlotId), ToggleLayer(SlotId), NextInstrument, PrevInstrument,
    SetSlotParam { slot: SlotId, target: ParamTarget, value: f32 }, SetMute(SlotId, bool),
    SelectPreset(SlotId, Preset), Panic, RecallSnapshot(SnapshotId), NextPart, PrevPart, /* … */
}
```

- **Sources:**
  - v1: UI and shortcuts.
  - Phase 3: MIDI mappings.
  - Phase 4: Launchpad and Launchkey.
  - Phase 5: timeline markers.
- **One dispatcher** turns actions into engine commands plus model updates. Feedback (LEDs, displays) subscribes to model changes.

### 4.11 Persistence

| File | Location | Format |
|---|---|---|
| Settings | `~/.config/merlin/settings.toml` | TOML |
| Rig | user-chosen `*.mrig` | JSON, versioned schema with migrations. Plugin state base64; SoundFont slots store file path + preset + panel values. |
| Autosave and crash marker | `~/.local/state/merlin/` | JSON, plus a marker recording "plugin X was being created or called", for safe mode |
| Plugin/sound cache, quarantine | `~/.cache/merlin/` | JSON |

**`PluginRef`** holds the format, a stable ID (CLAP id / VST3 class UID / SoundFont path + hash), name, vendor and a path hint. A missing plugin becomes a "missing" placeholder and is never silently dropped.

### 4.12 MIDI capture

The app always listens and keeps the last N minutes (default 30) of what was played; **Capture MIDI** saves them as a `.mid` file. This is not recording in the DAW sense: nothing is armed, nothing is edited, and nothing touches the disk until the button is pressed. The use is the one every musician knows: the best take was the one you were not recording.

- **What is kept**: every channel message that reaches the instruments from a keyboard-role input or the on-screen keyboard (notes, pedals, wheels, aftertouch, controllers, Program Change), as it arrived and before the router changes anything (zones, transposition and layers apply to what the *instruments* get, not to what you *played*). Not kept: controller-role devices, and any control that is mapped to an action (those are the app's, not music), and SysEx and clock.
- **How**: the JACK callback has a `Tap` (`crates/recorder`) and pushes `(frame, input, 3 bytes)` into a lock-free queue (wait-free, no allocation, no lock). A thread wakes four times a second, moves them into a `VecDeque` and drops what is older than the window, so the newest moment replaces the oldest. Time is the audio frame count, so a dropout in the sound card does not stretch the notes. The buffer is capped at 1 million events (16 MB) whatever the window.
- **The file**: Standard MIDI File format 1, 960 ticks per beat, a first track with the name, tempo and time signature (the transport's, so a performance played to the click lands on the bar lines of a sequencer; the notes keep their real times whatever the tempo), then one track per MIDI input that played, named after the device. The file starts at the first message, not 30 minutes before. A note-off whose note-on fell before the window is left out, and notes (and a sustain pedal) still down at the end are released at the moment of capture, so every file is clean to open.
- **Saving**: `~/Music/Merlin/Merlin capture <date> <time>.mid` (a setting). A capture never overwrites another and the file appears whole or not at all. Pressing again later includes the same playing and what came after: the buffer is not emptied.
- **Where**: a button in the header and in the Perform view, a mappable action (*Save the last minutes of MIDI*, so a foot switch or a pad does it), Settings > MIDI for the length (0 turns it off) and the folder.
- **Not done**: the audio is not captured (use your DAW's own recorder or `pw-record`), the on-screen keyboard's notes are always velocity 100, and a device that was swapped during the window is named by what is connected at the time of the capture.

## 5. UI

One window. The rack sits on top and the **editor dock** below shows the selected instrument.

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ MERLIN   Rig: Sunday gig ▾         CPU ▮▮▯▯▯ 23%  xruns 0  128 @ 48k   [⚙] [PANIC]│
├───────────────┬──────────────────────────────────────────────────────────────────────────┤
│ BROWSER       │ ┃1 GRAND PIANO  Pianoteq·CLAP  (●)ACTIVE  VOL◔ [M] EQ● CMP○ DLY○ REV● ┃   │
│ [search…   ]  │ ┃2 WARM PAD     Vital·CLAP     ( )       VOL◔ [M] EQ○ CMP○ DLY● REV● ┃   │
│ ★ Favourites  │ ┃3 STRINGS      LABS·VST3·WIN  (●)LAYER  VOL◔ [M] EQ● CMP● DLY○ REV● ┃   │
│ Instruments   │ ┃4 ORGAN        GM.sf3·SF ▾"Drawbar Organ" ( ) VOL◔ [M] EQ○ CMP○ ...   ┃   │
│   Surge XT    │ ┃                    [ + drag an instrument here ]                    ┃   │
│   Vital       ├──────────────────────────────────────────────────────────────────────────┤
│   FluidR3 SF  │ EDITOR · 1 GRAND PIANO        [Plugin] [Effects] [Zone & MIDI]           │
│   Serum 2 WIN │ ┌──────────────────────────────────────────────────────────────────────┐ │
│ Effects       │ │                                                                      │ │
│   …           │ │        (plugin's own GUI, embedded here as an X11 child window)      │ │
│               │ │                                                                      │ │
│               │ └──────────────────────────────────────────────────────────────────────┘ │
├───────────────┴──────────────────────────────────────────────────────────────────────────┤
│ KEYBOARD  ▕▒▒▒▒ STRINGS ▒▒▒▒▏                                                            │
│           ▕██████████████████ GRAND PIANO █████████████████████████▏    • • (live)      │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

**Rack units:**
- Compact 1U units with coloured faceplates.
- Click selects (exclusive). Shift-click layers.
- Selecting a unit also shows it in the editor dock.

**Editor dock tabs:**
- **Plugin:** the embedded GUI, or the native panel for built-ins and SoundFonts.
- **Effects:** the four FX "pedals" with LEDs and knobs, plus inserts.
- **Zone & MIDI:** zone editor ("learn split": press two keys), transpose, input/channel filter.

The dock can be collapsed, or **maximised** (rack shrinks to a strip) for large plugin GUIs.

**Keyboard map:** zones coloured by slot, with live note dots.

**Settings** (one full-screen panel, tabbed):
- **Plugins:** folders, rescan, quarantine.
- **Sounds:** SoundFont folders.
- **Windows plugins:** runtime, prefix, Install…, Wine Doctor.
- **Audio:** backend, rate, buffer size with latency read-out.
- **MIDI devices:** role, enable, activity LED.
- **Performance check.**

**Later:** Perform view and lock mode (Phase 3); Novation status (Phase 4); setlist sidebar (Phase 5).

**Visual style:** dark stage-friendly theme, high-contrast LEDs, large hit targets, works on 1366×768 and touchscreens.

## 6. Roadmap

*Durations are rough estimates for one experienced full-time developer.*

### Phase 0: technical spikes (3 weeks)

*The spike code and its notes were removed once Phase 1 had taken what it needed; they are in git history (the last commit that has `spikes/` is the parent of the one that removed it).*

| Spike | What | Passes when |
|---|---|---|
| S1 audio | `jack` crate on PipeWire: MIDI keyboard → sine → out | Round-trip measured with `jack_iodelay` at 64/128 frames. Realtime priority via rtkit. PipeWire's libjack loads without `pw-jack`. |
| S2 CLAP + embed | clack-host loads Surge XT (CLAP) and plays. Editor **embedded in an X11 child window inside an egui/wgpu main window** under XWayland. | Sound, embedded editor with mouse and keyboard input, resize, scroll/clip, state that restores. |
| S3 VST3 | Minimal Rust VST3 host on the `vst3` crate: Surge XT (VST3), embedded the same way | Process + embedded editor (IRunLoop) + state. **Timeboxed to 1 week.** If it bogs down, Plan B is a C++ shim over the official SDK hosting classes (MIT). |
| S4 Wine | yabridge from a pinned `master` commit, with Wine 11.20 Staging and a portable Wine build. A Windows VST3 freebie, embedded. | Editor embeds with correct mouse coordinates (no cursor offset) and HiDPI. Load time measured. First compatibility matrix. |
| S5 UI | egui on our own winit loop, forced X11: rack with 8 units + editor dock + keyboard map, fd-poller wakeups | Idle CPU under ~2% with an embedded editor open. Dock overlap rules work. |
| S6 SoundFont ✅ | FluidSynth FFI vs rustysynth (+ oxisynth): render GeneralUser GS (SF2) and MuseScore_General.sf3 (SF3) | Correct playback of both. Allocation-free render path. Build or bundle story clear. |

**Exit:** go/no-go on Rust-only VST3, embedding approach confirmed for CLAP, VST3 and Wine, Wine + yabridge pair chosen, SoundFont engine chosen.

**Phase 0 result: GO** (all six spikes done; remaining items are manual input checks, not open questions):

| Question | Answer |
|---|---|
| Rust-only VST3? | **Yes.** About 1,200 lines on the `vst3` crate hosted Surge XT with editor, run loop and state (S3). No C++ shim. Known gaps listed in the spike (IMessage, multi-bus, units/programs). |
| Embedding for CLAP, VST3, Wine? | **Confirmed.** One two-level scheme (clip window + one canvas per slot) serves all three. Rules in §4.6 (ConfigureNotify announce, `WM_WINDOW_ROLE`, no destroy-in-the-same-turn for bridged editors). Six editors alive, switching in microseconds (S5). |
| Wine + yabridge pair? | **yabridge `b580a9f7` + Wine Staging 11.20 built by us with a one-file `WaitForVBlank` patch** (S4). Surge XT, Six Sines and u-he plugins work. Known gaps: JUCE 8 Direct2D plugins (OB-Xf: Wine lacks `CreateSwapChainForComposition`) and DPF/OpenGL (Cardinal: stack overflow). Bridged plugins cost real CPU: about 2–7% of a core each at idle. |
| Audio backend? | **JACK API on PipeWire** with the libjack preload (S1). 128 frames, 0 xruns. This machine has no realtime scheduling, so the performance check is a v1 feature. |
| UI? | **egui on our own winit loop, X11.** Rack + dock + keyboard map idle at about 0.5% UI thread; the dock-hides-for-popups rule works (S5). |
| SoundFont engine? | **oxisynth** (S6). |

**Carried into Phase 1:** no code from the spikes is reused as is. Take from them: the dock and
canvas logic (S2/S5), `FdPoller` (S2), the VST3 host objects (S3), the JACK port setup and
libjack preload (S1), `build-wine.sh` and the patch set (S4).

### Phase 1: MVP, a playable rack (9–12 weeks)

1. ✅ Workspace, `model`, `engine` with the dummy backend and offline render tests.
2. ✅ JACK backend, JACK MIDI, automatic hardware connection, stats. (Meters in the UI; MIDI hotplug through JACK port notifications; device names, roles and activity lights below. The ALSA sequencer is only needed for controller SysEx and moves to the controller work in Phase 3.)
3. ✅ Router and note ownership, exclusive/layer, zones, transpose, panic (property-tested).
4. ✅ CLAP hosting (instrument) and state; plugin inserts (up to 4 effect plugins per slot, after EQ and compressor, before delay and reverb, with bypass, state in the rig, editor tabs in the dock).
5. ✅ VST3 hosting and state (`hosting::vst3`, ported from S3 onto the `PluginHandle` traits, with `IMessage`/`IAttributeList`; first audio bus only, no programs/units yet). Scanner reads `.vst3` bundles too.
6. ✅ **`embed` crate:** editor dock, clipping/scroll, editor cache, overlap rules. (Keyboard focus handshake: manual test pending.)
7. ✅ Built-in FX in the `dsp` crate: EQ, compressor, delay, reverb per slot (an Effects drawer over the rack), master limiter. Tails keep ringing after an instrument sleeps. Plugin inserts are in (item 4).
8. ✅ Scanner: cache, background scan with one child process per file and a timeout (`merlin-scan`), failed files with Retry, quarantine list; searchable browser with favourites and drag-and-drop. VST3-specific `moduleinfo.json` and SoundFont scanning later.
9. ✅ Rack UI, keyboard map, Settings panel (plugin folders, audio buffer size, MIDI inputs, Windows plugin paths) and the performance check with the exact fix for each problem. MIDI devices tab ✅: every device with an activity light, a role (Keyboard plays the instruments; Controller never does and is kept for the control mapping) and a use switch, remembered by name; `just fake-keyboard` stands in for hardware. Tested with a fake JACK keyboard only, not yet with real devices.
10. ◐ Windows plugins work through a manually configured Wine (`just dev` wires our patched build); the managed runtime and "Install Windows plugin…" are Phase 2.
11. ◐ Save/open rig, autosave with crash recovery, missing-plugin placeholders ✅; crash marker ✅ (a plugin that kills the app while loading is quarantined on the next start and the rest of the rig comes back); rig migrations ✅ (a step chain with a backup of the old file; none needed yet, format 1 is current).

**Exit:**
- A 2-hour rehearsal with 6–8 instruments, including at least one Windows plugin, all editors embedded.
- No dropouts or stuck notes when switching.
- 0 xruns at 128 frames on the dev machine.

### Phase 2: seamless plugins and sounds (5–7 weeks)

- Wine + yabridge runtime ✅ as a download (`just package-runtime` makes a 40 MB `tar.zst` with a manifest; Settings → Windows → "Download Windows plugin support" fetches it with `curl`, checks the SHA-256, unpacks it into `~/.local/share/merlin/runtime` and restarts into it with the same rig; default URL is a release tagged `runtime` on the repository, not published yet), managed prefix ✅ (`~/.local/share/merlin/wineprefix`, made on the first install), "Install Windows plugin…" ✅ (Settings → Windows: runs an `.exe`/`.msi` in the prefix on a worker thread, then makes the bridges and scans; the `wine` crate does what `yabridgectl sync` would, for CLAP and VST3), Wine Doctor ◐ (Wine, chainloader and prefix checks; per-plugin load test and logs to do). Windows VST3 bridging ✅ (merged bundles in `~/.local/share/merlin/bridges/vst3`; u-he Podolski and TyrellN6 load, embed, play, and save and restore their state; 32-bit plugins are skipped with a reason). VST2 bridging ☐.
- **SoundFont instrument** (§4.9) ✅: oxisynth (vendored with four small patches in `vendor/oxisynth`) plays `.sf2`/`.sf3` as instruments. They come from the standard sound folders plus the user's own (Settings → Plugins → Sound folders, with counts) and by dropping a file on the window; they are listed in the browser, a slot shows a searchable preset list, a level control and a "Shape the sound" panel (attack, release, brightness, resonance, tuning, voices; all saved in the rig), MIDI Program Change switches presets, drum kits (bank 128) play on their own channel, decoded files are shared between slots and decode on worker threads, and the audio path allocates nothing (tested, including while the controls move). Reverb and chorus come from the slot's own effects. Later: SFZ through sfizz.
- Scanner hardening: quarantine, parallel scans, metadata-only fast paths.
- Window-capture fallback for floating-only plugins, and the quirks database.
- **Compatibility pass** ◐: `just compat` (tools/compat.py) loads every instrument and effect the app finds in its own process, plays, saves and reopens, retries failures to separate flaky from broken, and writes [compat-matrix.md](compat-matrix.md). Current test assets: 17 plugins, 14 pass, 1 known unsupported (Cardinal), 2 with caveats (an add-on sound-effects SoundFont, and the intermittent silence of u-he plugins under Wine below). It found: the app froze when Surge XT's Windows build was saved right after loading (now the autosave waits 2 s, and a watchdog restarts the app and quarantines a plugin that stays stuck in a call for more than 15 s); a crash at exit about one run in fifteen (yabridge's static destructors; the app now exits without running them); and u-he plugins (Podolski, TyrellN6, CLAP and VST3) go silent about one start in ten under Wine, cause not found. About 25 reference plugins are still needed.
- Sleeping idle slots ✅, per-slot CPU ✅ (the engine times each slot per block and the rack row shows its share of the block budget, orange above 50% and red above 80% for the worst recent block; Windows plugins also show their Wine host's CPU), optional multi-threaded slot processing ☐.
- Performance check panel.
- Crash marker → safe mode.

**Exit:**
- A compatibility suite of about 25 reference plugins and SoundFonts passes (§7).
- First sound in under 5 minutes on a fresh distro, including a Windows plugin installed from its `.exe`.

### Phase 3: control and snapshots (4–6 weeks)

- MIDI learn ✅ (Settings → Controls: pick an action, press Learn, move a control; the mapping is saved in the rig).
- Controller profiles (device → logical controls).
- Mappings as source → glue → target:
  - Glue: absolute/relative with auto-detected encoding, toggle/trigger, range/curve, takeover modes (Jump, Pickup, Pickup-tolerant, Scale).
  - Scopes: global → rig → slot.
- **Switch instruments by CC, Program Change or note**, with next/prev on a foot switch ✅ (select, layer, mute, next, previous, slot-by-number, slot volume and master volume faders with jump or pick-up, panic; mapped controls no longer play an instrument; tested end to end with a pretend controller, `just e2e-controls`). Glue ✅ (the *options* button of a knob mapping): invert, range, soft/hard curve, endless encoders in three encodings with a step size, and the *Scale* takeover. Per-slot scope ✅ as *Macro / Volume of the slot that plays*, which follows the selection. Still to do: toggle-style glue for buttons, pick-up with tolerance, auto-detecting an encoder's encoding, controller profiles (they come with Novation in Phase 4). A global scope (mappings outside the rig) is left out on purpose: one file holds the whole setup.
- 8 macros per slot ✅: dock tab *Macros* (next to Plugin and Effects). Each macro has a name, a knob and any number of targets: a parameter of the instrument or one of its inserts (searchable picker), or, for a SoundFont, level / attack / release / brightness / resonance / tuning. Each target maps the knob's ends to a part of the parameter's range (it can run backwards). Moved by the tab, by a mapped controller knob (*Macro of a slot*, jump or pick-up), or by recalling a snapshot; macros with targets also show in the Perform view. Adding a macro's first target does not change the sound (the knob starts at the parameter's default or current position). Not done: naming macros from the plugin's own parameter names in the Perform view when unnamed, curves, macros for the built-in effects.
- **Snapshots** ✅ (partly): active slots, zones, transpose, volumes, pan, mute, FX on/off, insert bypass, SoundFont presets and master volume. They live inside the rig file (one file holds the whole setup), shown as a strip under the header (click to recall, right-click to rename, update or remove). Recalled by button, Program Change (`recall snapshot by number`) or next/previous on a foot switch; `just e2e-controls` checks all three. Macro positions are part of them.
- **Perform view** and lock mode ✅: header button *Perform* (or F11; Escape leaves). A stage screen with big snapshot buttons, one tile per instrument (play, layer, mute, level meter, volume), the master fader, panic and a DSP/xrun read-out; the editor dock is unmapped while it shows. *Lock* stops the view being left and ignores dropped files until *Unlock* is held for about a second; *Full screen* toggles the window. Not done: touch-screen testing, a per-rig choice of which controls to show.

**Exit:** a whole set is playable without touching the computer.

### Phase 4: Novation integration (3–4 weeks)

**Generic MIDI mapping first** ✅ (first slice, works with any controller): *MIDI map mode* as in Ableton: the header button (Ctrl+M) tints every mappable control on screen; click one, move a control on the device, and the two are linked (shown as a green badge such as `CC 14`; right-click removes it; learning a control again replaces its source). Mappable on screen: slot select, mute, volume, master, PANIC, snapshots, macros, built-in effect switches, insert on/off, and in the Perform view the transport, tap, click, tempo, click level and part buttons. The *Parameters* dock tab maps any parameter of the instrument or an insert (searchable, virtualised so big lists scroll). New actions: pan, effect on/off, insert bypass, click level, tempo, plugin parameter. Buttons can be read per press (pads, momentary foot switches) or by level (*Latching switch*: 64 and up is on). Tested: control-crate unit tests (including pick-up of a parameter and both switch modes) and `just e2e-controls` with a Surge XT parameter. Not done: parameters are not read back from the plugin (pick-up and scale use the last value this app set), pitch bend and aftertouch as sources, NRPN/14-bit CC, mapping a control to several targets with different ranges from the screen (Settings > Controls has the glue options), controller profiles and the device-specific Novation parts below.

**Launchkey MK3 first** ✅ (first slice; the one on the desk, any size: 25, 37, 49, 61, 88): written from Novation's own *Launchkey [MK3] Programmer's Reference Guide* (public PDF), not from other people's scripts. The DAW port is a controller by default (the MIDI port stays a keyboard); on connect the app switches the device to DAW mode, on leaving it switches it back. A built-in layout (a *controller profile*) sits behind the user's mappings: top pads select slots, bottom pads recall snapshots, Track ◄ ► = previous/next slot, Scene ▲ ▼ = previous/next part, knobs = macros of the slot that plays, faders = slot volumes and master (49/61/88), the buttons under the faders mute slots, Play / Click / Quantise = transport / click / tap tempo. Feedback: pads (slot colour, pulsing for the slot that plays, snapshot blue/green), button LEDs, the 16x2 screen (slot, snapshot or part and tempo), and the names and values shown while a knob or fader is moved. All of it is plain bytes in `crates/control/src/launchkey.rs` (unit-tested), sent through a new MIDI output on the JACK backend. Tested with a pretend device (`just e2e-controls`, which checks the bytes the app sends) and a device inquiry plus DAW-mode messages on the real one; what the lights and the screen look like is the user's to check (QA 5m). Second round: the knobs have defaults that need no setup (*Knob 1-8 of the slot that plays*: its macro if it has one, else pan, reverb, delay, bass, mid, treble, delay time, SoundFont brightness; reverb, delay and EQ switch on and off with the knob) and the pad modes are followed: the device reports its pad mode, so Session shows slots and snapshots, Device Select (held) shows select and layer rows, Drum lights the drum pads in the playing slot's colour, and the chord and custom modes are left to the device. Not done: the Launchkey Mini / MK4 (different, SysEx-based protocol: the MK4 Python script that was shared is a decompiled copy of Ableton's script with no licence, so it is only a reference for facts and none of it is copied), pad modes other than Session (drum, chords, custom), the arpeggiator and scale controls, the OLED of the MK4, a per-device layout editor.

**Device support is modular** ✅ (structure; downloads not built): everything that knows one controller lives in `devices/`, in crates that do not depend on the app: `surface-api` (the vocabulary: a `Description` of what a device has with stable control ids, pages of pads, well-known button roles; a `View` of lights, text and labels; the `Driver` and `Surface` traits; bytes in, bytes out, serde-able) and one crate per device family (`launchkey-mk3`). The app's decisions about what the controls are for are in `crates/control/src/surface.rs` and work from the description, so a Launchpad or any other device with pads and knobs gets the same layout from its driver alone. Drivers are optional cargo features of the app (`app/src/drivers.rs`). Meant to be reusable in other apps and DAW plugins, and forkable: `devices/README.md` says how to write one. Next steps if wanted: run drivers out of process (JSON lines on stdin/stdout, found in a drivers folder and downloaded on demand) or as WebAssembly modules, so the app does not carry code for controllers nobody has; the crates' licence (now the repository's GPL-3.0-or-later) should be chosen before releasing them separately, since a permissive one is what lets other DAW plugins use them.

**Device handling:**
- Detection by Device Inquiry + port index, port-name regexes as a fallback.
- Hotplug.
- DAW mode on connect, left on exit, state re-sent on reconnect.

**Launchpad X / Mini MK3 / Pro MK3:**
- Columns are slots (paged in banks of 8).
- Rows are snapshots.
- The right column is next/prev and Panic (song parts take it over in Phase 5).
- LED states: static = loaded, pulse = active, flash = queued, clock-synced.
- A mixer overlay gives volume and mute.

**Launchkey MK4 / Mini MK4:**
- Track ◄ ► selects the instrument and routes the keys to it (soft-arm), with its name on the OLED.
- Encoders control slot macros, with names and values on screen.
- Pads are slots or snapshots.
- Transport buttons are reserved for Phase 5.

**Exit:** a Launchpad and a Launchkey are fully usable without the computer.

### Phase 5: songs, setlists, backing tracks, i.e. "plan a track" (6–8 weeks)

- **Setlist → Song → Parts** ✅ (first slice). The rig holds the songs in playing order; a song has a tempo, beats per bar, a note and parts; a part is a snapshot plus an optional tempo and a cue note. *Setlist* screen (header button) edits them; *Add part from the current sound* saves a snapshot and the part in one go. *Next part / Previous part* (then on to the next song) and *Next / Previous song* are mappable actions and buttons in the Perform view, which also shows the song, part, tempo and cue. Slots are shared by all songs because they are the rig's slots, so per-song preloading is not needed yet. Not done: other part overrides (macros, volumes), and a setlist file shared between rigs.
- **Transport** ✅ (first slice): a sample-accurate tempo clock on the audio thread with a count-in (0 to 4 bars), a click, bar and beat counting, and MIDI clock out (24 ppqn with Start and Stop). The click is mixed into the main outputs or sent to two chosen outputs of the interface (Settings > Audio); the clock goes out of a JACK port named `midi_out`. Songs and parts set the tempo and the beats per bar; the Perform view has Start/Stop, tempo -/+ and Tap, click level and count-in; mappable: start/stop, click, tap. Verified: offline tests of the clock, `just e2e-controls`, and `pw-mididump` on the live clock port (24 ticks a beat). Not done: a tempo map inside a song (tempo changes at bars), time signatures other than beats per bar over a quarter note, sending clock to a chosen device automatically, Ableton Link.
- **Backing tracks** ✅ (first slice): a song lists audio files (WAV, FLAC, MP3, OGG, M4A; symphonia decodes, rubato resamples to the engine's rate). The tracks of the song you enter are opened and decoded ahead on background threads (a 3-second ring each, so the audio thread only pops samples), and start sample-accurately where the transport's run starts, after the count-in; stopping fades them out and opens them again for the next start. Each has gain, mute and an output: the main mix or the *cue* output (where the click goes) for guide tracks. Add them in the Setlist screen (path box or drop a file on the window); the Perform view shows the song clock and a mute per track. Tested offline (stem timing, gain, mute, cue routing) and live (`just e2e-controls`: two stems, one resampled, play, stop and restart from the top with no underruns). Not done: starting mid-song, per-stem outputs beyond main and cue, a tempo-synced stem (stretching), memory-budgeted preloading of the next song while this one plays.
- **Timeline markers with actions** ✅ (first slice): markers at a bar and beat of the song's run (Setlist screen > Timeline). Actions: go to a part, next part, show a cue (big amber text in the Perform view), stop, send a Program Change or a Controller on the `midi_out` port. The audio thread fires them on their clock tick and sends the MIDI itself, in order with the MIDI clock (verified live with `pw-mididump`: Start, Program Change, Controller one bar later, Stop); the ones that change the app come back through the control queue (`just e2e-controls` checks part, cue and stop on their bars). Manual advance from a foot switch is *Next part*. Not done: jump (needs seeking the stems), markers placed by time or by tapping during playback, other tempo/meter changes inside a song.
- **Novation hooks:** Launchpad right column = song parts; Launchkey transport and screen = song/part.
- **Optional:** Ableton Link.

**Exit:** a song with stems and automatic instrument changes plays correctly 100 times in a row in a soak test. ✅ `just soak-song`: 100 passes of count-in, two stems (one resampled), a part change and a stop on their bars: all passes correct, no underruns, memory flat (127 to 129 MB); one xrun in 8 minutes on this machine, which has no realtime scheduling.

### Phase 6: remaining formats (3–5 weeks)

- **VST2** ✅: written against the shared binary ABI (`crates/hosting/src/vst2`, no Steinberg header), for native Linux `.so` plugins (`~/.vst`, `~/.lxvst`, `/usr/lib/vst` and friends) and Windows `.dll` plugins through yabridge's VST2 chainloader: bridges are made in the app's own folder (a chainloader `.so` with the `.dll` linked beside it), from the Windows prefix's VST2 folders and from any folder you add in Settings > Plugins (one list for every kind of plugin, Windows or Linux), only for 64-bit DLLs that export `VSTPluginMain`. MIDI in, parameters (applied on the audio thread), chunk state, embedded X11 editors, shell plugins, host tempo and play state for plugins that ask. Tested against a small C plugin built during the tests, and live: CollaB3 (a Windows VST2 organ) plays, shows its editor and comes back from a saved rig. Known gaps: no MIDI out from plugins, no programs list, editors cannot be resized by the host, Pianoteq 6 Stage (Windows) loads and shows its editor but asks for activation in a fresh Windows environment (its own licence check), so I could not hear it.
- **LV2** ✅ (first slice): done in Rust with no lilv or suil, so nothing has to be installed beside the app: the Turtle files are parsed with `oxttl` (ports, features, UI, state), the plugin is run through its C interface (`crates/hosting/src/lv2`). Audio, control and atom ports (MIDI in), control ports as parameters (so macros can drive them), state through `state:interface` plus the control values, the worker extension (a thread of the host, answers delivered into the next `run`), the usual features (urid, options, buf-size, log, state paths), and X11 editors (idle, resize, instance access, parent), embedded in the dock. Bundles are found in `LV2_PATH`, `~/.lv2`, `/usr/lib/lv2` and friends and in any plugin folder. Tested against a small C plugin built during the tests (MIDI, gain, worker round trip, state) and live with Surge XT's LV2 (plays, editor embeds, comes back from a saved rig). Not done: patch parameters (a plugin that exposes parameters only through `patch:writable`, like Surge XT, lists none for macros), CV ports (connected to silence), time/transport atoms, MIDI out, GTK and Qt editors (they need suil), presets from the bundle.
- Optional: SFZ via sfizz.

### Phase 7: optional

- Out-of-process sandbox for native plugins, with hot reload after a crash.
- Windows/macOS builds.
- Headless kiosk mode.
- MIDI 2.0.
- Native Wayland embedding through a nested compositor, only once plugins actually ship Wayland GUIs.

**Total to the end of Phase 5:** about 8–11 months for one developer. The biggest swing factors are VST3 hosting (S3) and embedded Wine editors (S4).

## 7. Testing and reliability

**Automated tests:**
- **Unit and property tests:** router, note ownership, zones, takeover, and later the timeline scheduler.
- **Offline engine tests** with the dummy backend: scripted MIDI in, rendered audio out, note-on/note-off balance per slot, golden output for the built-in DSP and the SoundFont instrument.
- **Real-time safety:** `assert_no_alloc` in debug builds; a CI job running the engine under `rtsan-standalone`.

**Compatibility suite:** load → process → state round-trip → embed editor → resize → close.
- **Linux-native:** Surge XT, Vital, Dexed, OB-Xd, Odin 2, Airwindows Consolidated, LSP, Dragonfly Reverb, Pianoteq (trial).
- **Windows via yabridge:** a handful of freebies.
- **SoundFonts:** GeneralUser GS, FluidR3, MuseScore_General (SF3).
- Results feed the published Wine/yabridge compatibility matrix.

**Soak tests** (`just soak SECS SEED [--windows NAME]`, a seeded random script against the real app and plugins, reporting xruns, held keys, memory and threads): hours of random switching, MIDI floods and editor switching; track xruns, stuck notes and memory growth.

**Hardware:** the dev machine plus one low-end x86 laptop, at least 2 USB audio interfaces and 2 MIDI keyboards.

## 8. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| VST3 hosting in Rust is DIY, with a long tail of plugin quirks | High | Spike S3 timeboxed. Study Maolan and vst3-host. C++ shim over the MIT SDK as Plan B. Compatibility suite and quirks database. |
| Embedded editor edge cases: focus, popups and menus inside plugin GUIs, HiDPI, Wine cursor offsets | High | S2/S4 test these first. Two-level clipping windows. Window-capture fallback. Per-plugin quirks. Maximisable dock for big GUIs. |
| yabridge/Wine fragility: no release since 2024, maintainer short on time | High | Bundle a tested pair. Publish a compatibility matrix. Isolate it in the `wine` crate. Upstream fixes or a patch set. |
| A plugin crash on stage kills the show | High | Windows plugins already out of process. Safe mode. Quarantine. Interface ready for an out-of-process `RemotePlugin`. |
| egui overlays can't draw over native child windows | Medium | Overlap rule (§4.6). Unmap the editor during full-screen panels. |
| Pre-1.0 crate churn (clack, egui, winit) | Medium | Pin versions, thin wrappers, fork if blocked. |
| Scope creep (what stalled Meadowlark) | High | Hard "Never" list. Exit criteria per phase. |
| Third-party plugins ignore real-time rules | Medium | Per-slot CPU and xrun attribution. Sleeping idle slots. Warnings. |

## 9. Decisions

| Decision | Status |
|---|---|
| Licence | GPL-3.0-or-later ✅ |
| Windows runtime | Bundled, tested Wine + yabridge ✅ |
| Plugin editors | Embedded, never separate windows ✅ |
| Phase order | Novation before setlists/planned tracks ✅ |
| Platform | x86_64 only ✅ |
| VST2 + LV2 | At the end ✅ |
| SoundFont engine | **oxisynth** ✅ decided (S6) |
| Phase 0 spikes | ✅ GO (S1–S6) |
| Name "Merlin" | Open |

## 10. Immediate next steps

Phase 0 is done (see §6). Phase 1 starts with its item 1: the workspace skeleton, `model`,
`engine` with the dummy backend, and offline render tests. Then JACK, router, CLAP, VST3, `embed`,
in the order of §6 Phase 1.
