# Merlin: Product and Technical Plan

*Draft v1, 2026-10-18. Research behind this plan: [research.md](research.md). "Merlin" is a working name.*

## 1. Vision

**A Linux-first plugin host for playing live.** You plug instruments into a rack, pick one (or layer or split several) from your keyboard, and play. It is a little more than a plugin host and a lot less than a DAW.

> Cantabile's live model, MainStage's polish, **without the routing and without the tracks.**

### Design principles

1. **No routing UI.**
   - Signal flow is fixed and invisible: *MIDI in → active instrument(s) → that instrument's effects → master out*.
   - The only "routing" choices are a slot's MIDI input filter (default: every input) and, later, an output for click and backing tracks.
2. **No tracks, mixer, buses or sends.** The rack *is* the project: a vertical list of instrument units.
3. **Plugins just work.** VST3, CLAP, VST2 and LV2, plus Windows plugins through a managed, version-pinned Wine runtime. Users never type `yabridgectl` or `winecfg`.
4. **Built for the stage.**
   - Switching instruments never drops audio or leaves notes stuck.
   - Everything is preloaded.
   - Panic always works.
   - It recovers from crashes.
5. **Polish matters.** Rack units should look like gear. Settings fit on one screen per topic.
6. **Every user intent is an `Action`.** UI clicks, shortcuts, MIDI mappings, timeline markers and Launchpad pads all dispatch the same actions. This keeps the planned features cheap to add later.

## 2. Scope

| | In |
|---|---|
| **v1 (MVP)** | Rack of instrument slots.<br>Per-slot built-in effects (EQ, compressor, delay, reverb), plus up to 4 optional plugin inserts.<br>Per-slot MIDI filter (input device and channel, key zone, transpose).<br>Exclusive or layered instrument selection.<br>Held notes and tails carry over a switch.<br>Panic.<br>CLAP + VST3 hosting with plugin editors.<br>Plugin scanner and browser.<br>Settings: plugin folders, audio, MIDI devices.<br>Windows plugins through yabridge.<br>Save/load rig, autosave. |
| **Next** | VST2, LV2.<br>Managed Wine runtime with an "Install Windows plugin…" button.<br>System check for realtime readiness.<br>Crash safe mode.<br>MIDI learn and mapping: switch instrument by CC or Program Change.<br>Slot macros, snapshots, Perform view. |
| **Later** | Setlists, songs and parts.<br>"Plan a track" timeline with automatic instrument changes.<br>Backing tracks with a click on a separate output.<br>MIDI clock / Link.<br>Novation Launchpad and Launchkey integration. |
| **Optional** | Out-of-process plugin sandbox.<br>Windows/macOS ports.<br>Flatpak.<br>Headless stage-box mode. |
| **Never** | Timeline recording/editing, piano roll, audio tracks.<br>Free-form routing or patchbay, sends, sidechain, buses.<br>Automation lanes. |

## 3. Key decisions

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 | Language | **Rust** (2024 edition, stable) | Memory safety around heavy FFI and good real-time tooling. Usable CLAP hosting (`clack`) and MIT VST3 bindings (`vst3`). Go is ruled out: garbage-collection pauses and cgo callbacks on the audio thread. C++/JUCE would mean AGPL or a paid licence and no CLAP hosting. |
| D2 | Licence | **GPL-3.0-or-later** (recommended, **needs your decision**) | Needed for VST2 hosting, because the only clean-room headers are GPL and Windows VST2 plugins stay VST2 under yabridge. Also lets us bundle or patch yabridge (GPL-3) and use Ableton Link. A proprietary licence would drop VST2, keep yabridge at arm's length, and need Ableton's licence for Link. |
| D3 | Audio backend | **JACK API** via the `jack` crate. **Prefer PipeWire's libjack automatically.** `cpal` (native PipeWire/ALSA) as a fallback later. | The JACK API is the pro-audio standard and works unchanged on PipeWire and JACK2. JACK MIDI gives sample-accurate note timing. |
| D4 | MIDI | **JACK MIDI ports** for playing. **ALSA sequencer** for device listing, hotplug and controller SysEx (later). | PipeWire exposes hardware MIDI as JACK ports. Device Inquiry and SysEx are easier over ALSA seq. |
| D5 | Plugin formats | **CLAP → VST3** in v1, then **VST2 → LV2** | CLAP is fastest to get right with `clack` and validates the engine. VST3 is the format most plugins ship and is essential for Windows plugins. |
| D6 | Windows plugins | **yabridge**, **pinned and tested together with a specific Wine build**, managed by the app | yabridge is the only viable bridge. Its last release doesn't support Wine 9.22 or later, so we pin a tested `master` commit together with a portable Wine build. Users' own setups are respected. |
| D7 | UI toolkit | **egui** running on an **event loop we control** (`egui-winit` + `egui-wgpu`, not `eframe`), with the app forced to run as an X11 client | Best custom drawing for rack gear, fast to build with. Owning the event loop lets us service plugin timers and file descriptors without redrawing our own UI. iced is the fallback and gets confirmed in spike S5. |
| D8 | Plugin editors | **Separate top-level X11 windows** that we create (winit x11; x11rb if needed) | Plugin GUIs on Linux are X11-only, Wine editors included. Separate windows keep the toolkit choice independent. |
| D9 | Process model | **v1: one process**; native plugins in-process (Windows plugins already run in Wine processes). Plugin interface designed so an out-of-process "sandbox" version can be added later. Scanning always runs in a **separate process**. | Lowest latency and complexity. This is what Gig Performer, MainStage and Cantabile do. Crash risk is reduced by safe mode and quarantine. |
| D10 | Platforms | **Linux x86_64 first.** Platform-specific code behind traits (audio backend, editor window, plugin formats, Wine). | Windows/macOS stay possible: cpal covers WASAPI, ASIO and CoreAudio; the ASIO SDK is now GPLv3. |

## 4. Architecture

### 4.1 Processes and threads

```
┌──────────────────────────── merlin (single process) ────────────────────────────┐
│                                                                                          │
│  JACK RT thread (PipeWire)            Main thread (our winit loop)                       │
│  ┌──────────────────────────┐  cmds   ┌────────────────────────────────────────┐         │
│  │ Engine::process()        │◄────────│ egui UI · Action dispatcher            │         │
│  │  MIDI router             │  rtrb   │ plugin main-thread calls (create,      │         │
│  │  → slots (instr + FX)    │────────►│   activate, params, state, GUI)        │         │
│  │  → master → outs         │ meters, │ plugin editor windows (X11)            │         │
│  └──────────────────────────┘ events  │ VST3 IRunLoop / CLAP fd+timer servicing│         │
│        ▲ JACK MIDI in                 └───────────────▲────────────────────────┘         │
│        │                                              │ wakeups                          │
│  Worker threads: deferred freeing,     fd-poller thread (epoll on plugin fds + X11)      │
│  disk streaming (later), rig loading prep                                                │
└──────────────────────────────────────────────────────────────────────────────────────────┘
  Child processes: merlin-scan (one per plugin under test, with timeout)
                   wine + yabridge host processes (Windows plugins, managed by yabridge)
```

**Threading rules:**
- Plugin "main thread" calls happen on our main thread. Most plugins assume that one thread runs both their GUI and their main-thread callbacks.
- Plugins are created on the main thread. A "Loading…" overlay covers the UI meanwhile. That is acceptable because loading happens during setup, not mid-song.
- The fd-poller thread epolls the plugins' registered fds and wakes the winit loop with a user event. Callbacks then run on the main thread **without** triggering an egui repaint.

### 4.2 Cargo workspace

```
merlin/
├─ crates/
│  ├─ model/        serde types: Settings, Rig, Slot, PluginRef, (later) Mapping, Snapshot, Setlist
│  ├─ engine/       RT core: router, note ownership, slot chain, master, command/event queues. No UI deps.
│  ├─ hosting/      plugin abstraction + formats: clap (clack-host), vst3 (vst3 crate), vst2, lv2, builtin
│  ├─ dsp/          built-in effects: EQ, compressor, delay, reverb, master limiter
│  ├─ audio-io/     backends: jack (primary), cpal (fallback), dummy (tests/offline render)
│  ├─ editor-win/   X11 editor windows, fd/timer run loop, XEmbed/resize handling
│  ├─ wine/         Wine/yabridge detection, runtime install, yabridgectl driver, prefix management
│  ├─ scanner/      `merlin-scan` binary + plugin database (cache)
│  └─ control/      (phase 3+) actions, MIDI learn, mappings, controller profiles, Novation drivers
└─ app/             main binary: winit loop, egui UI, wiring
```

### 4.3 Engine

**Signal flow per audio block:**

```
JACK MIDI in ─► Router ─► per-slot event lists ─► Slot[i]: Instrument ─► FX: EQ ─► Comp ─► [inserts] ─► Delay ─► Reverb ─► Gain/Pan/Mute ─┐
                                                                                                                                 ├─► Master (limiter, volume) ─► Main L/R
                                                                                                                  ... Slot[n] ───┘     (later: Cue/Click outs)
```

**Data model:**
- **Slot:** name, colour, instrument (`PluginRef` + state), built-in FX settings, up to 4 inserts, volume, pan, mute, `MidiFilter`, armed flag.
- **MidiFilter:** input (any or named device), channel (any or n), key zone (lo–hi), velocity zone, transpose/octave.

**Real-time rules** (enforced in debug builds with `assert_no_alloc` and in CI with `rtsan-standalone`):
- No allocation, locks, syscalls or logging on the audio thread.
- Communication:
  - The UI sends commands to the engine through an `rtrb` single-producer/single-consumer queue.
  - The engine reports back (meters, note activity, CPU, xruns, plugin requests) through a queue plus atomics.
- Structural changes (add or remove a slot, swap a plugin):
  - Build the new state off-thread and send a pointer.
  - The old state comes back through a return queue and is freed on a worker thread (`basedrop`-style).
- Flush-to-zero/denormals off on the audio thread.
- `mlockall` where permitted (included in the system check).

**Idle slots sleep.** A CLAP plugin returning `CLAP_PROCESS_SLEEP` is skipped until it receives events. For VST3, processing is skipped once the tail has run out and the output is silent. This keeps many preloaded instruments cheap.

**Plugin latency compensation:** not needed (live playing). We report each plugin's latency and warn when it exceeds a few milliseconds.

**Multi-core:** slots are independent, so a real-time thread pool can process them in parallel later (Phase 2). v1 runs on one audio thread.

### 4.4 MIDI routing and note ownership (core to "seamless switching")

**Note ownership table**, keyed by (input, channel, played note) → (slot, transposed note):
- Recorded on note-on.
- Note-off and poly aftertouch always go to that slot, even after the keyboard has been switched to another instrument, a zone has changed, or the transpose has changed.

**Pedal state per slot** (CC64, CC66, CC67):
- After a switch, the old slot keeps getting pedal events until the pedal comes up.
- Whether the new slot inherits a pedal that is already down is a setting (default: no).

**Switch modes:**
- **Exclusive** (default): selecting slot B makes it the only keyboard target.
- **Layer:** a toggle adds or removes slots. Zones on layered slots give splits.
- **Old slot behaviour on switch:** the old slot keeps processing so tails ring out, then sleeps. "Instant silence" and "defer until all notes released" are per-slot options for later.

**Panic:**
1. Synthesize note-offs from the ownership table.
2. Send CC64=0, CC120 (All Sound Off) and CC123 (All Notes Off) to every slot.
3. Briefly fade the master out and back in.

**Everything else** (pitch bend, mod wheel, channel pressure) follows the current keyboard target(s). Pitch bend and mod wheel are reset on the slot that loses focus.

This is pure logic with no I/O, so it gets exhaustive unit tests (property tests: every note-on eventually receives a note-off).

### 4.5 Hosting layer

Each format implements one interface. It is split by thread, the way CLAP and VST3 themselves are:

```rust
/// Lives on the main thread: lifecycle, parameters, state, GUI.
pub trait PluginHandle {
    fn info(&self) -> &PluginInfo;                       // name, vendor, format, ports, latency
    fn activate(&mut self, sr: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>>;
    fn deactivate(&mut self, proc: Box<dyn PluginProcessor>);
    fn params(&self) -> &[ParamInfo];
    fn save_state(&mut self) -> Result<Vec<u8>>;
    fn load_state(&mut self, data: &[u8]) -> Result<()>;
    fn editor(&mut self) -> Option<&mut dyn PluginEditor>; // attach(x11_window), size, resize
    fn on_main_thread(&mut self);                          // plugin-requested callbacks
}

/// Sent to the audio thread; must be real-time-safe.
pub trait PluginProcessor: Send {
    fn process(&mut self, io: &mut AudioIo, events_in: &EventList,
               events_out: &mut EventList, transport: &Transport) -> ProcessStatus;
}
```

- **Implementations:**
  - `ClapPlugin` (clack-host)
  - `Vst3Plugin` (our own host layer on the `vst3` bindings: IHostApplication, IComponentHandler, IPlugFrame, IRunLoop, IConnectionPoint, parameter-change and event queues)
  - `Vst2Plugin` (Phase 2)
  - `Lv2Plugin` (Phase 2)
  - `BuiltinPlugin` (our own effects, so mapping, state and macros treat them the same way)
- **Sandbox later:** a future `RemotePlugin` implements the same two traits over shared memory, which gives the out-of-process sandbox without touching the engine.
- **Bridged plugins:** detected via the exported `yabridge_version` symbol. Tagged "Windows" in the UI, and per-plugin quirks are applied (for example, resize handling).

### 4.6 Plugin editor windows

- **Window creation:** the app runs as an X11 client (forced even on Wayland). Each editor gets a top-level winit window (x11 backend) whose XID is passed to the plugin:
  - CLAP: `gui.set_parent`, with floating mode as a fallback
  - VST3: `attached(kPlatformTypeX11EmbedWindowID)`
  - LV2: `ui:parent` via suil
- **Behaviour:**
  - Resizes go both ways (plugin requests ↔ window events).
  - Window title shows "Slot name — Plugin".
  - Editors stay on top optionally, and remember their position per slot.
- **Quirks database** (`quirks.toml`): per-plugin flags such as "force floating", "fixed size" and "needs a delayed show". Users can toggle them in the UI.

### 4.7 Scanning and plugin database

**`merlin-scan`** is a separate binary that loads one plugin, prints a JSON description, and exits.
- The parent runs scans in parallel (bounded) with a timeout.
- A crash or hang adds the plugin to a **quarantine list** with the reason. "Retry" is available.

**Cheap metadata first:**
- CLAP: factory descriptors.
- VST3: `moduleinfo.json` when the bundle has one.
- LV2: lilv TTL metadata (never loads binaries).
- VST2 and bridged plugins must actually be loaded. Bridged plugins start Wine, which takes seconds each, so these scans show progress and run in the background.

**Cache:**
- Location: `~/.cache/merlin/plugins.json`, keyed by path + mtime + size.
- Rescans touch only changed files.
- Folder watching via inotify is optional.

**Browser:**
- Search, favourites, Instrument/Effect categories.
- Badges for format and "Windows".
- When the same plugin is installed in several formats, show one entry with a preferred-format setting (CLAP > VST3 > VST2, native > Windows).

### 4.8 Windows plugins

**v1 (basic):**
- Detect `yabridge`, `yabridgectl` and Wine, and check their versions against our compatibility matrix.
- In Settings → Plugins → "Windows plugin folders", add folders inside Wine prefixes. Detect the standard locations automatically.
- The app runs `yabridgectl add` and `yabridgectl sync` on the user's behalf, then scans `~/.vst/yabridge`, `~/.vst3/yabridge` and `~/.clap/yabridge`.
  - This shares bridges with the user's other DAWs, which is a feature.
  - It works alongside an existing yabridgectl setup.
- Status shown inline: "Wine 11.20 Staging + yabridge <rev>: ✔ tested / ⚠ untested / ✖ known broken".

**Phase 2 (managed runtime, the "seamless" part):**
- **"Windows plugin support" component:** a one-click download into `~/.local/share/merlin/runtime/` of a **pinned portable Wine build plus a pinned yabridge build we compile from a tested commit**, with checksums.
  - Our process sets `WINELOADER` so only our app uses it.
  - The user's system Wine is untouched.
  - "Use system Wine instead" is offered for experts.
- **Managed prefix:** `~/.local/share/merlin/wineprefix`, created and configured automatically (fonts, DPI, NTSync where available).
- **"Install Windows plugin…"** lets the user pick an installer `.exe` or `.msi`. The app:
  1. runs it in the managed prefix,
  2. adds that prefix's plugin folders,
  3. syncs,
  4. scans,
  5. shows the new plugins.
- **Wine Doctor panel:** versions, prefix health, per-plugin load test, per-plugin yabridge options (`yabridge.toml`: groups, editor options), log viewer.
- **Limits stated up front:**
  - 64-bit plugins only.
  - Copy-protection or iLok-style plugins may not work.
  - Native Linux versions are preferred when they exist (u-he, Vital, Surge, Pianoteq, …).
- **Upstream risk:** we should expect to contribute fixes to yabridge upstream, or maintain a small patch set.

### 4.9 Actions (the extensibility hook)

```rust
enum Action {
    SelectInstrument(SlotId), ToggleLayer(SlotId), NextInstrument, PrevInstrument,
    SetSlotParam { slot: SlotId, target: ParamTarget, value: f32 }, SetMute(SlotId, bool),
    Panic, RecallSnapshot(SnapshotId), NextPart, PrevPart, /* later: TransportPlay, … */
}
```

- **Sources of actions:**
  - v1: UI clicks and keyboard shortcuts.
  - Phase 3: MIDI mappings.
  - Phase 4: timeline markers.
  - Phase 5: Launchpad and Launchkey surfaces.
- **One dispatcher** turns an action into engine commands plus model updates. Feedback (LEDs, displays) subscribes to model changes.

### 4.10 Persistence

| File | Location | Format |
|---|---|---|
| Settings (plugin folders, audio, MIDI devices, Wine runtime, UI) | `~/.config/merlin/settings.toml` | TOML |
| Rig (slots, plugin refs plus base64 state, FX, zones, later mappings and snapshots) | user-chosen `*.irig` | JSON, versioned schema with migrations |
| Autosave and crash marker | `~/.local/state/merlin/` | JSON plus a marker file recording "plugin X was being created or called" for safe mode |
| Plugin cache and quarantine | `~/.cache/merlin/` | JSON |

**`PluginRef`** stores the format, a stable ID (CLAP id / VST3 class UID / VST2 unique ID / LV2 URI), name, vendor and a path hint. If a plugin is missing, the slot stays in the rig as a "missing" placeholder, so nothing is silently dropped.

## 5. UI

**Main window** (one screen, no tracks):

```
┌───────────────────────────────────────────────────────────────────────────────────────┐
│ MERLIN   Rig: Sunday gig ▾        CPU ▮▮▯▯▯ 23%  xruns 0  128 @ 48k  [⚙] [PANIC]│
├───────────────┬───────────────────────────────────────────────────────────────────────┤
│ BROWSER       │ ┌─ 1 ─ GRAND PIANO ──────────────── Pianoteq 9 · CLAP ─────────────┐ │
│ [search…   ]  │ │ (●) ACTIVE   [Edit]   VOL ◔  PAN ◑  [M]  │ EQ● COMP○ DLY○ REV● │ │
│ ★ Favourites  │ │ zone C1–C8 · all inputs · transpose 0                            │ │
│ Instruments   │ └──────────────────────────────────────────────────────────────────┘ │
│   Surge XT    │ ┌─ 2 ─ WARM PAD ─────────────────── Vital · CLAP ──────────────────┐ │
│   Vital       │ │ ( ) active   [Edit]   VOL ◔  PAN ◑  [M]  │ EQ○ COMP○ DLY● REV● │ │
│   Serum 2 WIN │ └──────────────────────────────────────────────────────────────────┘ │
│ Effects       │ ┌─ 3 ─ STRINGS ───────────── Spitfire LABS · VST3 · WIN ───────────┐ │
│   …           │ │ (●) LAYER    [Edit]   VOL ◔  PAN ◑  [M]  │ EQ● COMP● DLY○ REV● │ │
│               │ └──────────────────────────────────────────────────────────────────┘ │
│               │                    [ + drag an instrument here ]                      │
├───────────────┴───────────────────────────────────────────────────────────────────────┤
│ KEYBOARD  ▕▒▒▒▒▒ STRINGS ▒▒▒▒▒▏                                                       │
│           ▕████████████████ GRAND PIANO ███████████████████████████▏   • • (live)     │
└───────────────────────────────────────────────────────────────────────────────────────┘
```

**Rack units:**
- Each looks like 1U gear with a coloured faceplate.
- Clicking the faceplate selects the instrument (exclusive mode). Shift-click layers it.
- Expanding a unit shows the FX strip: each effect is a small "pedal" with an on/off LED and 2–3 knobs. It also shows the zone editor ("learn split" by pressing two keys).

**Keyboard map** (bottom): zones coloured by slot, with live note dots. This is the "what will sound if I press this key" view that MainStage hides behind too many layers.

**Settings** (one panel, tabbed):
- **Plugins:** folders, rescan, quarantine.
- **Windows plugins:** runtime status, prefix, Install Windows plugin…, Wine Doctor.
- **Audio:** backend status, sample rate, buffer size with live latency read-out, outputs.
- **MIDI devices:** detected devices with a role (keyboard / controller / pedal), enable toggle, activity LED.
- **Performance check** (Phase 2): realtime priority, CPU governor, PipeWire quantum, memlock, kernel preemption, each with a "fix this" hint.

**Later:**
- **Perform view** (Phase 3): huge instrument/snapshot tiles and big song/part text, plus a lock mode.
- **Setlist sidebar** (Phase 4).

**Visual style:** dark stage-friendly theme, high-contrast LEDs, large hit targets, scales well on 1366×768 laptop screens and touchscreens.

## 6. Roadmap

*Durations are rough estimates for one experienced full-time developer.*

### Phase 0: technical spikes (2–3 weeks), to remove the big unknowns first

| Spike | What | Passes when |
|---|---|---|
| S1 audio | `jack` crate on PipeWire: MIDI keyboard → sine → out | Round-trip measured with `jack_iodelay` at 64 and 128 frames. Realtime priority via rtkit confirmed. PipeWire's libjack loads without `pw-jack`, or we know how to make it. |
| S2 CLAP | clack-host loads Surge XT (CLAP), plays from the keyboard, editor in a winit-x11 window under XWayland | Sound, editor, and saved state that restores. |
| S3 VST3 | Minimal Rust VST3 host on the `vst3` crate, loading Surge XT (VST3) | Process + editor (IRunLoop) + state work. **Timeboxed to 1 week.** If it bogs down, switch to the C++ shim over the official SDK hosting classes (MIT). |
| S4 Wine | Build yabridge from a pinned `master` commit. With your Wine 11.20 Staging *and* a portable Wine build, load one Windows VST3 and one Windows VST2 freebie through S3 | Editor shows and takes mouse input; load time measured. First compatibility matrix written. |
| S5 UI | egui on our own winit loop, forced X11: mock rack with 8 units and the keyboard map, plus the fd-poller wakeups | Idle CPU under ~2% with the editor open. Toolkit choice confirmed. |

**Exit:** go/no-go on Rust-only VST3, choice of Wine + yabridge pair, toolkit confirmed.

### Phase 1: MVP, a playable rack (8–12 weeks)

1. Workspace, `model`, and the `engine` with the dummy backend and offline render tests.
2. JACK backend, JACK MIDI in, automatic connection to the configured hardware ports, meters, CPU/xrun stats.
3. Router and note ownership, exclusive/layer modes, zones, transpose, panic.
4. CLAP hosting (instrument + inserts), editors, state.
5. VST3 hosting, editors, state.
6. Built-in FX (EQ, compressor, delay, reverb, master limiter).
7. Out-of-process scanner, cache, browser.
8. Rack UI, keyboard map, Settings (plugins, audio, MIDI).
9. Windows plugins (basic yabridge integration, see 4.8).
10. Save/load rig, autosave, "missing plugin" placeholders.

**Exit:**
- A 2-hour rehearsal with 6–8 instruments, including at least one Windows plugin.
- Instruments switched by click or shortcut with no dropouts and no stuck notes.
- 0 xruns at 128 frames on the dev machine.

### Phase 2: plugins really are seamless (6–8 weeks)

- VST2 support (FFI on the VeSTige/FST headers; requires D2 = GPL).
- LV2 via our own lilv + suil FFI.
- Managed Windows runtime, managed prefix, "Install Windows plugin…", Wine Doctor.
- Scanner hardening: quarantine, parallel scans, metadata-only fast paths.
- Sleeping idle slots, per-slot CPU, optional multi-threaded slot processing.
- Performance check panel.
- Crash marker → **safe mode** on next launch, with the culprit plugin disabled.

**Exit:**
- A compatibility suite of about 30 reference plugins passes (§7).
- On a fresh distro install, the first sound plays in under 5 minutes.

### Phase 3: control and snapshots (4–6 weeks)

- MIDI learn.
- Controller profiles (device → logical controls).
- Mappings as source → glue → target:
  - Glue: absolute/relative with automatic encoder detection, toggle/trigger, range/curve, takeover modes (Jump, Pickup, Pickup-tolerant, Scale).
  - Scopes: global → rig → slot.
- **Switch instruments by CC, Program Change or note**, with next/prev on a foot switch.
- 8 **macros** per slot, mapped to plugin parameters.
- **Snapshots:** which slots are active, plus zones, volumes, FX on/off and macro values. Recalled by Program Change or a button.
- **Perform view** and lock mode.

**Exit:** a whole set can be played without touching the computer.

### Phase 4: songs, setlists and backing tracks, i.e. "plan a track" (6–8 weeks)

- **Setlist → Song → Parts.** A part is a snapshot plus overrides.
  - Per-song preloading within a memory budget.
  - Slots can be shared between songs.
- **Transport:** tempo map, count-in, **click on a separate output** (cue bus), MIDI clock out (Start/Stop/SPP).
- **Backing tracks:** disk-streamed stems (symphonia + creek + rubato), per-stem gain and output.
- **Timeline markers with actions:** switch part, send Program Change or CC to external gear, show a text cue, jump. Manual advance from a foot switch.
- **Optional:** Ableton Link (subject to licence, see D2).

**Exit:** a song with stems and automatic instrument changes plays back correctly 100 times in a row in a soak test.

### Phase 5: Novation integration (3–4 weeks)

Can come before Phase 4; it only depends on Phase 3.

**Device handling:**
- Detection by Device Inquiry plus port index, with port-name regexes as a fallback.
- Hotplug.
- DAW mode entered on connect and left on exit. All LED and display state is re-sent on reconnect.

**Launchpad X / Mini MK3 / Pro MK3:**
- Columns are slots (paged in banks of 8).
- Rows are snapshots.
- The right column is song parts.
- LED states: static = loaded, pulse = active, flash = queued. Blinking follows MIDI clock sent from our transport.
- A mixer overlay gives volume faders and mute.

**Launchkey MK4 / Mini MK4:**
- Track ◄ ► selects the instrument and routes the keys to it, like Ableton's soft-arm. The name appears on the OLED.
- Encoders control the slot macros, with names and values on screen (relative mode and pickup enabled).
- Transport buttons drive the song.
- Pads are slots or snapshots.

**Exit:** a Launchpad and a Launchkey are fully usable without touching the computer.

### Phase 6: optional

- Out-of-process sandbox for native plugins (a "together" mode like Bitwig's, then per plugin), with hot reload after a crash.
- Windows and macOS builds.
- Flatpak (hard with Wine/yabridge; Bitwig's Flatpak can't use yabridge).
- Headless kiosk mode for stage PCs.
- MIDI 2.0.

**Total to the end of Phase 5:** about 8–11 months for one developer. The two biggest swing factors are VST3 hosting (S3) and Wine (S4).

## 7. Testing and reliability

**Automated tests:**
- **Unit and property tests** for the router, note ownership, zones, transpose, takeover, and later the timeline scheduler.
- **Offline engine tests** with the dummy backend:
  - Scripted MIDI in, rendered audio out.
  - Assert that note-ons and note-offs balance per slot.
  - Golden-output tests for the built-in DSP.
- **Real-time safety:** `assert_no_alloc` in debug builds; a CI job running the engine under `rtsan-standalone` with the built-in effects and a CLAP test plugin.

**Plugin compatibility suite:**
- A script that runs load → process → state round-trip → open/close editor for each reference plugin:
  - **Linux-native:** Surge XT (CLAP/VST3/LV2), Vital, Dexed, OB-Xd, Odin 2, Airwindows Consolidated, LSP, Dragonfly Reverb, Pianoteq (trial).
  - **Windows freebies via yabridge:** a handful of VST2 and VST3.
- Results feed the published Wine/yabridge compatibility matrix.

**Soak tests:** hours of random switching, MIDI floods and snapshot recalls, tracking xruns, stuck notes and memory growth.

**Hardware:** the dev machine plus one low-end laptop, and at least 2 USB audio interfaces and 2 MIDI keyboards.

## 8. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| VST3 hosting in Rust is DIY, with a long tail of plugin quirks | High | Spike S3 timeboxed. Study Maolan and vst3-host. **C++ shim over the MIT SDK hosting helpers** as Plan B. Compatibility suite and quirks database. |
| yabridge/Wine fragility: no release since 2024, maintainer short on time, Wine 11 issues | High | Pin and test the yabridge + Wine pair and ship it as a runtime. Publish a compatibility matrix. Isolate it in the `wine` crate. Contribute upstream or keep a patch set. Recommend native Linux versions. |
| Plugin GUI quirks under XWayland (DPI, focus, resize, cursor offset) | Medium | Our own X11 windows. Per-plugin quirk flags. Floating fallback. |
| A plugin crash on stage kills the show | High | Windows plugins are already out of process. Crash marker → safe mode. Scanner quarantine. Interface ready for an out-of-process `RemotePlugin` (Phase 6, could move earlier). |
| Pre-1.0 crate churn (clack, egui, winit) | Medium | Pin versions, keep wrappers thin, vendor or fork if blocked. |
| Scope creep (what stalled Meadowlark) | High | Hard "Never" list. Exit criteria per phase. Rack, not DAW. |
| Licence decided too late | Medium | Decide D2 before Phase 2 (VST2 depends on it). |
| Third-party plugins ignore real-time rules | Medium | Per-slot CPU and xrun attribution. Warn and suggest alternatives. Sleep idle slots. |

## 9. Open decisions (your call)

1. **Licence (D2):** GPL-3.0-or-later (recommended) or proprietary/commercial.
2. **Windows runtime:** downloadable pinned Wine + yabridge (recommended), or system Wine only.
3. **Phase order:** Novation integration (Phase 5) before songs/backing tracks (Phase 4)?
4. **Target hardware:** x86_64 only? Windows plugins are impossible on ARM boards. Should a mini-PC or stage-box form factor count as a target?
5. **Name:** keep "Merlin"?

## 10. Immediate next steps

1. `git init` and create the Cargo workspace skeleton (crate stubs from §4.2), plus CI (fmt, clippy, tests).
2. Run spikes S1–S5 in `spikes/`. They are throwaway code; the results get written back into this plan.
3. Install the reference plugins for testing: Surge XT, Vital (CLAP/VST3), and one Windows freebie in a test prefix.
4. Revisit D3, D6 and D7 with the spike data, then start Phase 1.
