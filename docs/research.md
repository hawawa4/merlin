# Merlin: Research Findings

*Research date: 2026-10-18. Versions and statuses can go stale quickly. Anything marked (unverified) could not be confirmed from a primary source.*

## 1. Summary

- **Every dedicated live-performance host is Windows/macOS only.** This includes Gig Performer, Cantabile, MainStage, Camelot, LiveProfessor, Unify, Tracktion StageBox and Steinberg VST Live. A June 2026 KVR thread summed up the Linux situation as "no specialized live gig host", with Carla "still a bit flaky under the lights".
- **The Linux options are generic hosts or full DAWs.** Kushview Element comes closest, with a basic MIDI set list and graph switching by Program Change. None of them combines:
  - setlists,
  - instant preloaded switching where held notes and tails carry over,
  - first-class splits and layers,
  - a stage-friendly UI,
  - every plugin format, including bridged Windows plugins.
- **Bridging Windows plugins is the weakest link in the Linux stack.** yabridge is the standard tool, but:
  - the last tagged release is 5.1.1 (Dec 2024), and it only works up to Wine 9.21;
  - support for newer Wine was merged to master in April 2026 but has never been released;
  - the maintainer is short on time;
  - Carla's Wine bridge is still marked "experimental".

  A host that makes this part simple and reliable has a real advantage.
- **Licensing:**
  - VST3 has been MIT since SDK 3.8 (Oct 2025).
  - CLAP is MIT; LV2 is ISC.
  - VST2 can no longer be licensed. The only routes are clean-room headers, both GPL: VeSTige (GPL-2+) and FST (GPL-3+).
  - yabridge does not convert formats: a Windows VST2 stays a VST2.

  So hosting Windows VST2-only plugins effectively requires a GPL host.
- **Plugin GUIs on Linux are effectively X11-only** (XWayland on Wayland desktops). Native Wayland embedding exists only as a VST3 3.8 "preview" and a CLAP draft.
- **Language: Rust.** Go is ruled out for the real-time core (garbage-collection pauses, cgo callbacks on the audio thread, no plugin-hosting ecosystem). Rust has:
  - a usable CLAP host library (`clack-host` 0.2);
  - raw VST3 bindings (`vst3` 0.3, MIT), but no ready-made VST3 host layer, so we write that ourselves;
  - JACK, PipeWire and MIDI crates;
  - two active open-source Rust hosts to learn from: Generic DAW and Maolan.

## 2. Existing products

### 2.1 Dedicated live-performance hosts (none run on Linux)

| Product | Platforms | Price | Formats | Notable for us |
|---|---|---|---|---|
| Gig Performer 5 | Win/Mac | $169–199 | VST2/VST3/AU | **Gold standard.** Rackspaces + variations, songs/parts/setlists. "Patch Persist" keeps notes and tails sounding across a switch. Predictive loading. Timeline player whose markers fire actions. Rig Manager (aliases for hardware). |
| Cantabile 4 | Windows | Free Lite / paid tiers | VST2/VST3 | Bindings model (source → mapper → target, with scopes and two-way feedback). Song/rack "states". Held notes survive state changes but are cut on song change (users complain). |
| MainStage 4 | macOS | $29.99 | AU | Concerts/sets/patches. Held keys keep sounding by default. "Defer patch change" and "instantly silence previous patch" options. Per-control takeover mode. Set-level playback plugin. |
| Camelot Pro | Win/Mac/iPad | ~$139 (free tier) | VST/VST3/AU | Setlists, scenes, backing tracks, scores. Its 2026 changelogs keep fixing scene-switch bugs (stuck sustain, glitches). Users find the UI confusing. |
| Tracktion StageBox (2025) | Win/Mac | $99 | VST3/AU | Closest commercial model to our idea: setlists, splits/layers, Program Change song change, instruments shared across songs. |
| Steinberg VST Live 3 | Win/Mac | $149.99 | VST3 | Adds video, DMX, OBS (a broader scope than ours). |
| LiveProfessor, Unify, Blue Cat PatchWork, Bidule, VEP 8 | Win/Mac | €95–199 | VST/AU | Theatre/FOH cueing, layering plugins, chain plugins, modular, and network templates respectively. None is a keys-first live host on Linux. |
| Image-Line MiniHost Modular | Win/Mac | free | — | Discontinued. |

### 2.2 What exists on Linux

| Product | What it is | Why it doesn't fill the gap |
|---|---|---|
| **Kushview Element** (GPL-3, JUCE, active, 1.2.0 Aug 2026) | Modular graph host. MIDI Set List, Program Change graph switching, Lua nodes. LV2/VST3/CLAP. | A generic graph UI with little stage UX. No Windows-plugin bridging. Splits are built by hand from filter nodes. |
| **Carla** (GPL-2+, 2.5.10 Jul 2025, 2.6 never shipped) | Rack/patchbay host with the widest format coverage and an experimental Wine bridge. | Maintenance mode. Rack mode is a stereo-only serial chain. No setlists. Dated UI. "Flaky" on stage. |
| **MOD Desktop** (AGPL, beta) | LV2 pedalboards with a web UI. | LV2 only and guitar-oriented. Changing pedalboard reloads every plugin. |
| **Elk Sushi** (AGPL, active) | Headless real-time host (gRPC). | A component, not a product. A good engine reference. |
| **Zynthian** (GPL, Raspberry Pi appliance) | Chains, splits, fast "ZS3" sub-snapshots. | A hardware appliance, LV2 only. A good UX reference. |
| Bespoke Synth, ossia score, Ildaeil | Modular / show-control / plugin-in-plugin. | Not built for a keys player. |
| **Bitwig Studio 6** | Full DAW. VST2/VST3/CLAP. Instrument Selector keeps tails on Program Change switches (user report). **Only Linux host that sandboxes plugins.** | Full DAW with no setlist/song model. No LV2. The Flatpak build can't use yabridge. |
| **REAPER 7.82** | Full DAW with every format. SWS "Live Configs" extension. | Live use means building your own setup from extensions. |
| Ardour 9.8, Qtractor 1.6, LMMS, Waveform, Zrythm v2 (alpha), Fender Studio Pro 8 (Linux beta, has a "Show Page") | DAWs. | Linear/timeline workflows. Zrythm v2 moved off Carla to JUCE (VST3) plus native CLAP/LV2, and its bridging hasn't been ported yet. LMMS has a built-in Wine bridge, but it is VST2 only and breaks with Wine versions. |

### 2.3 The gap our product targets

1. **A song/setlist/patch model with preloaded, glitch-free switching.**
2. **Notes and tails carry over a switch.** Notes are released on the instrument that received the note-on, and the sustain pedal is tracked per instrument.
3. **Splits and layers as first-class objects** (key and velocity zones, transpose), with a visible keyboard map.
4. **Every format in one host,** including bridged Windows VST2/VST3/CLAP with a managed, version-pinned Wine runtime.
5. **Stage reliability:**
   - fast cached plugin scans with crash quarantine,
   - panic,
   - a check of the realtime system setup,
   - autosave and crash recovery,
   - a lock/perform mode.
6. **A simple UI** that a non-engineer understands in minutes. A "rack", not a patchbay.

**Product owner's take on the closest products (2026-10-18):** Cantabile and MainStage are the nearest to the vision.
- Cantabile has "too much faff with the audio routing".
- MainStage is "more polished looking but feels complicated with way too many tracks".

Target: Cantabile's live model, MainStage's polish, **no routing UI and no tracks**.

## 3. Plugin hosting on Linux

### 3.1 Formats and licensing

| Format | Licence | Linux status | Notes |
|---|---|---|---|
| **VST3** | MIT since SDK 3.8.0 (2025-10-20); VSTGUI is BSD | Mature. The format most commercial plugins ship. | The host must implement `Linux::IRunLoop` (fd handlers and timers). 3.8 adds Wayland "preview" interfaces and MIDI 2.0 learn/mapping interfaces. Bundles may contain `moduleinfo.json`, which allows scanning without loading the plugin. |
| **CLAP** | MIT | Good, and growing (~400 plugins from ~90 vendors: u-he, FabFilter, TAL, Surge, Vital, Airwindows, LSP…). | Cleanest API. Supports plugin "sleep" when idle. Linux event loop via the `posix-fd-support` and `timer-support` host extensions. No Ardour support. JUCE has no CLAP *hosting*. |
| **VST2** | SDK not licensable since 2018, including for hosts | Still common among old and free Windows plugins. | Clean-room headers only: VeSTige (GPL-2+, used by Ardour and Carla) or FST (GPL-3+). Can't use the "VST" name or logo. |
| **LV2** | ISC | The native Linux ecosystem (LSP, x42, Calf, Dragonfly…). | `lilv` reads plugin metadata without loading binaries. UIs come in X11/Gtk/Qt flavours and need `suil`. The Rust crates (`livi`, `lilv`) haven't been released since Nov 2024. |

### 3.2 Windows plugins via Wine

**yabridge** (GPL-3.0) is the only serious option.
- Bridges 64-bit Windows VST2, VST3 and CLAP into Linux `.so`, `.vst3` and `.clap` files.
- **Never converts between formats.**
- Workflow:
  - `yabridgectl add <dir>`, then `yabridgectl sync`.
  - Output goes to `~/.vst/yabridge`, `~/.vst3/yabridge` and `~/.clap/yabridge`.
  - Each bridge is a tiny "chainloader" stub that loads the real yabridge library, so updating yabridge needs no re-sync.
- Configuration and hooks:
  - Config lives in `$XDG_CONFIG_HOME/yabridgectl/config.toml`.
  - `yabridge.toml` per-plugin options, including "groups" that share one Wine process.
  - `WINELOADER` env var pins which Wine binary is used.
  - Exported `yabridge_version` symbol lets a host detect bridged plugins (Ardour 8.2 uses it).
- **Wine compatibility, Oct 2026:**
  - 5.1.1 (Dec 2024) works only up to Wine 9.21.
  - Master (unreleased) supports Wine 10 and 11 via a new editor-embedding approach (by CodeWeavers' rbernon and Asahi Lina).
  - Reported issues on master: cursor/DPI offsets, Carla segfaults on Wine 11.15, WebView2-based editors crashing.
  - Some users report master + Wine 11.15 working better than 9.21.
- **32-bit Windows plugins are effectively dead.** The bitbridge is incompatible with Wine's WoW64 mode, which is the default from Wine 11.
- Editors are reparented into a host-supplied **X11** window. yabridge unsets `WAYLAND_DISPLAY`, which forces Wine's X11 driver.
- yabridgectl has no machine-readable output. Drive it by writing its config and running `sync`, or reimplement the sync logic.
- Beware fake yabridge download sites (the upstream README warns about them). Use GitHub or distro packages only.

**Alternatives are not viable:**
- LinVst: VST2 only, and every plugin must be re-converted after each update.
- Carla's bridge: experimental.
- LMMS RemoteVstPlugin: internal to LMMS, VST2 only.
- AudioGridder: needs a Windows/macOS server, adds network latency, stale.
- airwave: dead since 2020.

### 3.3 Plugin GUIs

- VST3 uses `kPlatformTypeX11EmbedWindowID`; CLAP uses `CLAP_WINDOW_API_X11` (XEmbed); LV2 uses `ui:parent` via suil.
- Native Wayland embedding:
  - VST3 3.8: preview only, and it needs the host to run a nested compositor.
  - CLAP: Wayland embedding unsupported ("use floating windows"). A nested-compositor protocol is being drafted (issue #474, Oct 2026).
- JUCE-based plugins (most commercial plugins) and Wine plugins are X11 only.
- REAPER, Bitwig and Ardour all rely on XWayland.

**Consequence:** run the whole app as an X11 client and open each plugin editor in its own top-level X11 window.

### 3.4 Audio and MIDI stack

- **Use the JACK API.** It runs unchanged on PipeWire's JACK layer (pipewire-jack, the default on modern distros) and on JACK2.
  - JACK2's last release was 1.9.22 (2023): stable but stagnant.
  - PipeWire 1.6.x is current and has had JACK/MIDI fixes in 2026.
- **Buffer size** under PipeWire: `PIPEWIRE_QUANTUM=64/48000` for one process, or `pw-metadata -n settings 0 clock.force-quantum 64` for the whole session.
- **USB interfaces:** `api.alsa.headroom=0` and `api.alsa.disable-batch=true` can cut latency noticeably.
- **Typical round-trip latency:** about 4–5 ms at 64 frames × 2 periods at 48 kHz on good hardware; about 6–10 ms at 128 frames on USB.
- **Realtime scheduling:** RLIMIT_RTPRIO (audio group) or rtkit. A `preempt=full` + `threadirqs` kernel is usually enough. PREEMPT_RT has been mainline since 6.12.
- **MIDI:**
  - JACK MIDI gives sample-accurate events inside the audio cycle, and PipeWire exposes ALSA MIDI hardware as JACK MIDI ports automatically.
  - The ALSA sequencer is useful for device enumeration, hotplug and SysEx to controllers.
  - The ALSA sequencer has supported UMP/MIDI 2.0 since kernel 6.5.

**Local machine (2026-10-18):**
- PikaOS 4, kernel 7.2.6.
- PipeWire 1.6.9 with pipewire-jack libraries (under `/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack`; `pw-jack` available).
- Wine 11.20 (Staging) at `/opt/wine-staging`. Note: yabridge 5.1.1 will not work with this Wine version.
- Wayland session.
- Rust 1.99.1, Go 1.26.3.
- User is not in an `audio` group, so realtime scheduling comes from rtkit.

### 3.5 Crash isolation

- **Bitwig** offers five sandbox modes, from in-process up to one process per plugin, and can reload a crashed plugin.
- **REAPER** can run plugins in a separate or dedicated process.
- **Ardour, Gig Performer, MainStage and Cantabile** host plugins in-process.
- **yabridge** always runs the Windows code in a separate Wine process, with a watchdog.
- **Out-of-process hosting in Rust:** Maolan does it with shared memory, one process per plugin.
- **Costs:** a context switch per process hop each audio block, passing realtime priority to the helper processes, and extra jitter at 32–64-frame buffers. It also complicates editor embedding.

### 3.6 SoundFonts (SF2/SF3)

SF3 is SF2 with Ogg Vorbis-compressed samples (from MuseScore).

| Option | Notes |
|---|---|
| **FluidSynth** (C, LGPL-2.1-or-later) | The reference SoundFont synth, current release 2.5.x (Dec 2025). SF3 needs a build with libsndfile + Vorbis. From 2.5 it builds with C++11 and an OS layer replacing GLib (GLib support deprecated). https://github.com/FluidSynth/fluidsynth/wiki/ChangeLog |
| **rustysynth** (pure Rust, MIT, 1.3.6, Aug 2025) | Port of MeltySynth. **Rejects SF3** (`UnsupportedSampleFormat`) and discards SF2 modulators (S6). The `rustysynth-ext` fork adds SF3 but still drops modulators. https://github.com/sinshu/rustysynth |
| **oxisynth** (pure Rust, LGPL-2.1, 0.1.0) | Port of FluidSynth: SF2 modulators, SF3 via `lewton`, sounds like FluidSynth, ~340× realtime. One allocation per note-on (fixable). **Chosen** (S6). https://github.com/PolyMeilex/oxisynth |
| `fluidlite` crate | Bindings to FluidLite, with an optional `with-sf3` feature. https://docs.rs/fluidlite |
| `soundfont` crate (OxiSynth author) | Reader only; SF3 marked as in progress. |

No format conversion is needed: the host plays SoundFonts with a built-in instrument.

## 4. Implementation stack

### 4.1 Language

| Option | Verdict |
|---|---|
| **Rust** | **Chosen.** Memory safety around a lot of unsafe FFI. Good real-time tooling: `rtrb`, `basedrop`, `assert_no_alloc`, `rtsan-standalone`. CLAP hosting via `clack`. MIT VST3 bindings. The `jack`, `cpal`, `midir` and `symphonia` crates. Risk: no mature VST3 *host* layer, so we write it ourselves (reference implementations exist). |
| Go | **Rejected.** The JACK/ALSA realtime callback runs on a foreign C thread, so calling into Go means re-entering its runtime, which can stall. Go threads take part in garbage-collection stop-the-world phases. Allocation-free code can't be enforced. No CLAP, VST3 or LV2 hosting. It could only be a UI layer over a C/Rust core. |
| C++ / JUCE 9 | A credible alternative. JUCE hosts VST3, LV2 and AU, but **not CLAP**, and VST2 only with headers you supply yourself. Licence is AGPLv3 or commercial. Meadowlark's author: "If I used C++, I could have just used JUCE." **Plan B for VST3:** a thin C++ shim over the now-MIT official VST3 SDK hosting helpers, exposed to Rust through a C ABI. |
| Carla as a backend | Possible through its C API (`CarlaHost.h`), but it owns the audio engine, embedding plugin UIs is "very experimental", development is slowing, and it is GPL-2+. Not recommended. |

### 4.2 Rust crates (versions as of Oct 2026)

| Concern | Crate | Status |
|---|---|---|
| CLAP hosting | `clack-host` + `clack-extensions` 0.2.0 (2026-09) | "Feature-complete", but the API may still break. Includes an example host built with cpal, winit-x11, midir and rtrb. |
| VST3 | `vst3` 0.3.0 (coupler-rs) | Raw bindings plus COM helpers. Maolan's `plugin-host` and `vst3-host` 0.10 are built on it; use both as reference, not as dependencies. |
| VST2 | none usable | `vst` crate archived 2024. We would write a small FFI layer from the VeSTige/FST headers ourselves (GPL). |
| LV2 | `livi` 0.7.5 / `lilv` 0.2.4 | Stale since Nov 2024. Expect to write our own lilv and suil FFI. |
| Audio I/O | `jack` 0.13.5 (primary); `cpal` 0.18.2 (native PipeWire/ALSA since 0.18.0) | |
| MIDI | `midir` 0.11, `wmidi`, `midly` (SMF files) | |
| Real-time infrastructure | `rtrb`, `ringbuf`, `basedrop`, `arc-swap`, `assert_no_alloc`, `rtsan-standalone`, `no_denormals` | |
| DSP | `fundsp` 0.23 (EQ, delay, reverbs, limiter; **no compressor**), `biquad` 0.6 | We write the compressor ourselves. |
| Backing tracks | `symphonia` 0.6, `rubato` 5, `creek` (disk streaming) | |
| Ableton Link | `rusty_link` 0.4.9 (wraps the official abl_link, GPL-2.0) | Check whether its licence is GPL-2.0-only (which would be incompatible with GPL-3.0). Link itself is GPLv2+. |
| Novation | `launchy` 0.4.1 (Launchpad Mini MK3 and older), `launchkey-sdk` 0.1.5 (Launchkey MK4 and Mini MK4) | |

### 4.3 GUI toolkits

The X11-only rule for plugin editors applies whatever toolkit we pick. Plugin editors get **separate top-level X11 windows that we create ourselves** (winit with the x11 backend, baseview or x11rb), so the main UI toolkit choice is largely independent of plugin hosting.

| Toolkit | Assessment |
|---|---|
| **egui** 0.37 (via `egui-winit` + `egui-wgpu`/`glow`) | **Recommended.** Excellent custom painting (rack units, knobs, meters, keyboard maps), very fast iteration, and we can own the winit event loop. Frequent breaking releases, so pin it. Repaint only on change to limit CPU use. |
| iced 0.14 | Proven by Generic DAW and Maolan for exactly this kind of app (Elm architecture). Generic DAW maintains a fork, which signals rough edges. Strong second choice. |
| Slint 1.18 | Polished and well funded, but limited custom drawing. Licence: GPLv3, royalty-free desktop licence, or commercial. |
| Vizia, Tauri, Xilem, GPUI | Small user base or slow (Vizia), awkward with native windows and slow WebKitGTK (Tauri), experimental (Xilem), or unstable (GPUI). |

### 4.4 Projects to learn from

- **Generic DAW** (Rust, GPL-3, iced, very active): CLAP hosting via clack, embedded vs floating plugin GUIs.
- **Maolan** (Rust, BSD-2, iced): out-of-process CLAP/VST3/LV2 hosting, shared-memory IPC, VST3 on the `vst3` crate.
- **Elk Sushi** (C++, AGPL): the cleanest real-time-safe engine design (lock-free event dispatch, track/processor model).
- **Kushview Element** (C++, GPL-3): closest open-source "Gig Performer". Program Change switching, set list.
- **Qtractor** (C++, GPL): small, readable native VST2/VST3/LV2/CLAP hosting without JUCE.
- **Zrythm v2** (C++23, AGPL): native CLAP and lilv hosting, scanning without loading plugin binaries.
- **Ardour** `libs/surfaces` (GPL): complete Launchpad X/Pro MK3 and Launchkey MK4 drivers.
- **yabridge**: Wine IPC reference.
- **free-audio/clap-host**: CLAP reference host.
- **Meadowlark** post-mortem (BillyDM's blog), lessons:
  - GUI work was badly underestimated.
  - Scope was too broad.
  - "The Rust community tends to overstate the capabilities of the current ecosystem."

## 5. Controllers, mapping and songs

### 5.1 Novation protocols

- **All public:** Programmer's Reference Guides cover Launchpad X, Launchpad Pro MK3, Launchkey MK3 and Launchkey MK4 (including Mini MK4). For the Launchpad Mini MK3 there are third-party mirrors only.
- **Common SysEx header:** `F0 00 20 29 02 <dev>`.

  | `<dev>` | Device |
  |---|---|
  | `0C` | Launchpad X |
  | `0D` | Launchpad Mini MK3 |
  | `0E` | Launchpad Pro MK3 |
  | `0F` | Launchkey MK3 |
  | `14` | Launchkey MK4 |
  | `13` | Launchkey Mini MK4 |

- **Separate "DAW" USB MIDI port.** DAW mode is entered by SysEx (Launchpad, `… 10 01`) or by note `9F 0C 7F` (Launchkey).
- **LEDs:** note or CC, where channels 1/2/3 select static/flashing/pulsing and the value picks one of 128 palette colours. An RGB SysEx is also available.
  - Flashing and pulsing sync to MIDI clock received from the host.
- **Launchpad grid:** notes `11 + row*10 + col`. The top row is CC 91–98; the right column is CC 89…19.
- **Launchkey MK4:**
  - Encoders are CC 85–92 on channel 16 and can be switched to relative mode.
  - 128×64 OLED with SysEx text and bitmap commands.
  - "Feature controls" switch on relative encoders, fader pickup, touch events, keyboard zones and split point.
- **Linux:**
  - All are class-compliant, and Ardour drives them natively.
  - Port names vary, so identify devices by Universal Device Inquiry reply plus port index.
  - The Launchkey MK4 port-naming bug is fixed in kernel 6.15+.

### 5.2 Mapping UX lessons

- **Separate hardware from mappings:** device profile → logical controls → mappings → targets. This is Gig Performer's Rig Manager / ReaLearn approach, and it means a rig survives a change of keyboard.
- **Each mapping is source → glue → target, with feedback.**
  - Glue options: absolute / relative (encoding auto-detected), toggle / trigger, range / curve / invert.
  - **Takeover modes:** Jump, Pickup, Pickup-tolerant, Scale/Catch-up (ReaLearn has the most complete set).
  - **Fire modes:** short, long and double press.
- **Scopes:** global, then song, then slot. Each mapping can either let the event continue or suppress it (Cantabile).
- **Patch changes:**
  - Program Change (+ Bank Select CC0/32), with a configurable 0–127 / 1–128 display.
  - A foot switch for "next part".
- **Stuck-note prevention:**
  - Note-off and poly aftertouch always go to the instrument that received the note-on.
  - The sustain pedal (CC64/66/67) is tracked per instrument.
  - Optional "defer switch until notes released" (MainStage).
  - Panic = CC120 + CC123 + synthesized note-offs + CC64=0.

### 5.3 Songs, setlists, backing tracks

- **Gig Performer's model works:** setlist → song → parts, where each part points at a rackspace variation.
  - Racks are shared across songs.
  - Predictive loading.
  - The streaming audio player has timeline markers that fire actions: switch part, send Program Change, set widgets, captions.
- **Backing tracks:**
  - Disk-streamed stems.
  - A **click/cue bus routed to a separate output** for in-ears, plus count-in.
  - Song parts can jump to markers.
- **Sync:**
  - The internal transport is the master; send MIDI clock (with Start/Stop/SPP).
  - Ableton Link is optional (GPL or Ableton's commercial licence).

## 6. Key sources

- yabridge: https://github.com/robbert-vdh/yabridge (README, CHANGELOG, issues #409, #432, #457)
- VST3 MIT licence: https://steinbergmedia.github.io/vst3_dev_portal/pages/VST+3+Licensing/Index.html
- CLAP Wayland issue: https://github.com/free-audio/clap/issues/474
- CLAP ecosystem: https://cleveraudio.org/hosts-and-plugins/
- FST header licence: https://sources.debian.org/src/fst/0.177.0-1/README.md
- clack: https://github.com/prokopyl/clack
- vst3-rs: https://github.com/coupler-rs/vst3-rs
- Generic DAW: https://github.com/generic-daw/generic-daw
- Maolan: https://github.com/maolan/maolan · https://github.com/maolan/plugin-host
- Meadowlark lessons: https://billydm.github.io/blog/why-im-taking-a-break-from-meadowlark/ · https://billydm.github.io/blog/daw-frontend-development-struggles/
- Elk Sushi: https://github.com/elk-audio/sushi
- Element: https://github.com/kushview/element
- Bitwig sandboxing: https://bitwig.com/userguide/latest/vst_plug-in_handling_and_options
- PipeWire JACK config: https://docs.pipewire.org/page_man_pipewire-jack_conf_5.html
- Novation guides:
  - Launchpad X: https://fael-downloads-prod.focusrite.com/customer/test/s3fs-public/downloads/Launchpad%20X%20-%20Programmers%20Reference%20Manual.pdf
  - Launchpad Pro MK3: https://fael-downloads-prod.focusrite.com/customer/prod/s3fs-public/downloads/LPP3_prog_ref_guide_200415.pdf
  - Launchkey MK4: https://fael-downloads-prod.focusrite.com/customer/prod/downloads/launchkey_mk4_programmer_s_reference_guide_v2_en.pdf
- Ardour Novation surfaces: https://github.com/Ardour/ardour/tree/master/libs/surfaces
- Mapping UX:
  - Cantabile bindings: https://www.cantabilesoftware.com/guides/bindings
  - ReaLearn: https://github.com/helgoboss/helgobox
  - MainStage patch-change options: https://support.apple.com/en-me/guide/mainstage/mstg62dce2b5/mac
- Ableton Link in Rust: https://github.com/anzbert/rusty_link
