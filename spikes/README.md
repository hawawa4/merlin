# Phase 0 spikes

Throwaway experiments that answer the questions in [plan §6, Phase 0](../docs/plan.md).
Each spike is a workspace binary. None of this code is meant to survive into the app as is;
the findings are what matter, and they are logged below.

Test machine: PikaOS 4 (Debian-based), kernel 7.2, PipeWire 1.6.9, KDE Wayland, Ryzen,
Rust 1.99. Test assets (plugins, SoundFonts) go in `test-assets/` (gitignored).

| Spike | Status |
|---|---|
| S1 audio | ✅ passes in code; ⚠️ machine has no realtime scheduling; round-trip measurement pending a loopback cable |
| S2 CLAP + embed | ✅ embedding, clip/scroll, fit, reopen, state save/load, idle CPU; ⏳ mouse/keyboard input needs a manual test |
| S3 VST3 | not started |
| S4 Wine | not started |
| S5 UI | partly answered by S2: idle CPU 1.3% with an embedded editor open |
| S6 SoundFont | ✅ **decided: oxisynth** (needs a one-line note-on allocation fix) |

## S1: audio (`spikes/s1-audio`)

`cargo run -p s1-audio --release -- [--demo] [--frames 64|128] [--seconds N]`

JACK client on PipeWire: MIDI in → 32-voice sine synth → stereo out, auto-connected to the
first physical outputs and every MIDI source. Debug builds abort on any allocation in the
process callback (`assert_no_alloc`).

**Findings**

1. **Building needs a stub `jack.pc`.** `jack-sys` runs pkg-config even with runtime loading
   (`dynamic_loading`, the default). `build-support/pkgconfig/jack.pc` has no link flags and
   `.cargo/config.toml` points `PKG_CONFIG_PATH` at it, so no `libjack-dev` is needed.
2. **The system `libjack.so.0` is jackd2's, not PipeWire's** (Debian/Ubuntu/PikaOS). Without
   `pw-jack` the client fails with `SERVER_FAILED`. Fix: `dlopen` PipeWire's copy
   (`/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack/libjack.so.0`) by full path with
   `RTLD_GLOBAL` before opening a client. glibc matches on soname, so the jack crate's own
   `dlopen("libjack.so.0")` then returns PipeWire's copy. Works: client opens in ~12 ms.
   Fedora and Arch ship PipeWire's libjack as the system one, so the preload is a no-op there.
3. **Buffer size:** PipeWire's default quantum here is 1024 (21 ms). Setting
   `PIPEWIRE_QUANTUM=<frames>/48000` before opening the client switches the graph: 375
   cycles/s at 128 frames, 750 cycles/s at 64. Reported playback latency 128 / 64 frames.
   `buffer_size()` read before activation still shows the old value; read it after.
4. **0 xruns** at 64 and 128 frames over 10 s runs; DSP load ~2%; process callback max
   ~10 µs (sine synth), occasional ~100 µs outliers (no realtime priority, see 5).
5. ⚠️ **No realtime scheduling on this machine**, for PipeWire itself as well as for us: the
   process thread runs `SCHED_OTHER`. rtkit is not installed (PipeWire logs
   `RTKit error: ServiceUnknown`), and the user is not in the `pipewire` group that
   `/etc/security/limits.d/25-pw-rlimits.conf` grants `rtprio 95`. Fix (needs sudo):
   `sudo usermod -aG pipewire $USER` and log in again, or install `rtkit`.
   **App consequence:** the performance check (plan P2) must be in v1: detect a
   non-realtime process thread and show the exact fix.
6. MIDI: the ALSA → PipeWire bridge exposes hardware as JACK MIDI ports
   (`Midi-Bridge:…`, `BLE MIDI 1:out`), so auto-connection by port type works.

**Pending:** round-trip latency with `jack_iodelay` (needs a loopback cable and
`jackd2`'s example clients); retest with realtime scheduling enabled.

## S2: CLAP + embedded editor (`spikes/s2-clap-embed`)

`cargo run -p s2-clap-embed --release -- [plugin.clap]` (default: Surge XT 1.3.4 from
`test-assets/dl/surge-linux/`). Optional `S2_SCRIPT="3:save; 5:scroll 0 200; 7:load; 9:fit;
11:toggle; 13:toggle; 15:quit"` runs timed actions; `S2_DEBUG_REPAINT=1` logs redraw causes.

egui 0.37 + egui-wgpu (Vulkan) on our own winit 0.30 loop, forced to X11 (XWayland on KDE
Wayland). clack-host 0.2 instance processed in the JACK callback (MIDI in, on-screen keys).
The editor is embedded in an X11 child window ("dock") that we place over the space egui
leaves free.

**Findings**

1. **Embedding works with no compositor tricks.** Window tree: main window → dock
   (`x11rb` child window) → Surge's editor window. The editor renders inside our window,
   and the dock clips it at its edges.
2. **Scrolling works** by moving the plugin's window to a negative offset inside the dock
   (`ConfigureWindow`); range = editor size − dock size.
3. **Fit to dock works:** `adjust_size` + `set_size`. Surge keeps its aspect ratio
   (asked 1580×833, got 1337×833). After fitting, reset the scroll offset.
4. **Close/reopen works:** first embed 130–154 ms, reopen 62 ms (size remembered).
   Supports the editor-cache idea in plan §4.6: keep recently used editors created and only
   hide them, rather than destroying them.
5. **Plugin requests resize:** accept and *don't* call `set_size` with the requested size.
   JUCE logs `HOST_MISBEHAVING` if `set_size` gets a size that didn't go through
   `adjust_size` first (1427 vs 1426).
6. **HiDPI:** winit reports scale 1.25 on this display; `gui.set_scale(1.25)` gives Surge a
   correctly scaled editor (1427×889 physical px).
7. **Main-thread services:** Surge (JUCE) registers one 20 ms timer and no fds. Timers run in
   `about_to_wait` with `ControlFlow::WaitUntil(next deadline)`. The fd poller thread is in
   place but untested by Surge; JUCE-based Windows plugins via yabridge or u-he plugins may use it.
8. **State:** `state.save` gives 51,869 bytes for Surge's init patch; `state.load` accepts it.
   Round-trip with a changed patch still needs the manual test below.
9. **Idle CPU: 1.3% of one core** for the whole process with the editor open (Surge redraws
   itself every 20 ms), egui at 2.3 frames/s (status refresh only). Pitfall found:
   `egui_winit::State::on_window_event` returns `repaint = true` for `RedrawRequested`
   itself; honouring it redraws forever (~140 fps).
10. **Audio:** 0 xruns at 128 frames, DSP ~1.5% with Surge idle.
11. **Gotchas:** egui 0.37 has `default = []`, so enable `default_fonts` or all text is tofu.
    `jack.pc` stub and libjack preload as in S1.
12. **Test automation limit:** KWin does not let `xdotool` inject clicks or warp the pointer
    into XWayland windows (even our own egui buttons ignore it). Screenshots via
    `import -window <xid>` work. Input tests need a human, or a nested X server (Xvfb /
    Xephyr) in CI.

**Manual test still needed** (input and focus are S2's remaining questions):
click and drag Surge's controls, open its menus (they are popup windows: do they appear at
the right place?), type a patch name in Save (keyboard focus inside the embedded editor),
then use our egui buttons again (does focus come back?), change the patch, Save state,
change again, Restore state.

**Round 2 (manual test): mouse.** Dragging Surge's knobs works and menus pop up in the
right place; no pointer offset. But after a knob drag the cursor ends up farther away than
the knob moved (~2×). Likely cause: Surge hides the cursor while dragging and warps it back
onto the knob on release (JUCE `setMousePosition` → `XWarpPointer`). XWayland/KWin ignores
pointer warps from X11 clients, so the cursor stays where the physical mouse went, and
Surge's drag sensitivity is not 1:1. Not an embedding bug: any X11 host on Wayland
should show it. To confirm: switch Surge's mouse behaviour to "Exact" (Surge menu →
Mouse Behavior), or try the same plugin in another X11 host (Carla, REAPER) on this desktop.
Long-term fix is the native-Wayland path (nested compositor, plan §4.6), where we can honour
warps for embedded editors.

## S6: SoundFont engines (`spikes/s6-soundfont`)

`cargo run -p s6-soundfont --release -- <file.sf2|sf3>...` writes
`spikes/s6-soundfont/out/<engine>-<file>.wav` (piano chord, strings chord, guitar arpeggio,
drum groove).

RealtimeSanitizer build (uses the system LLVM runtime, nothing downloaded):

```sh
RTSAN_ENABLE=1 RTSAN_LIB_PATH=/usr/lib/llvm-21/lib/clang/21/lib/linux/libclang_rt.rtsan-x86_64.a \
CARGO_TARGET_DIR=target/rtsan cargo build -p s6-soundfont --release
RTSAN_OPTIONS=halt_on_error=false ./target/rtsan/release/s6-soundfont <files>
```

Every call the audio thread would make (note on/off, program change, render in 128-frame
blocks) runs inside `realtime_enter()/realtime_exit()`.

**Results** (48 kHz, release build)

| File | Engine | Load | Memory | Speed | RTSan |
|---|---|---|---|---|---|
| FluidR3_GM.sf2 (141 MiB) | FluidSynth 2.6 (system) | 91 ms | +167 MiB | 324× | ❌ |
| | rustysynth 1.3.7 | 19 ms | +141 MiB | 290× | ✅ clean |
| | rustysynth-ext 1.5.0-alpha.1 | 28 ms | +141 MiB | 289× | ✅ clean |
| | oxisynth 0.1.0 | 55 ms | +182 MiB | 360× | ⚠️ 1 site |
| MuseScore_General.sf3 (38 MiB) | FluidSynth | 369 ms | +218 MiB | 261× | ❌ |
| | rustysynth | fails: `UnsupportedSampleFormat` | | | |
| | rustysynth-ext | 2316 ms | +206 MiB | 287× | ✅ clean |
| | oxisynth | 2351 ms | +230 MiB | 341× | ⚠️ 1 site |

**Findings**

1. **rustysynth rejects SF3** (upstream has a test for exactly that). The fork
   **rustysynth-ext** adds SF3 through the pure-Rust `lewton` Vorbis decoder, decoding all
   samples to PCM at load time.
2. **FluidSynth is not realtime-clean as packaged.** Every note-on, note-off, program change
   and render takes a pthread mutex (uncontended, so cheap in practice, but RTSan rightly
   flags it). Turning off `synth.threadsafe-api` (accepted, verified) removes the API lock
   but not the internal ones (voice setup, `fluid_voice_update_param`). Debian also builds
   it with **OpenMP**, so the first render allocates and starts a libgomp thread pool. A
   bundled FluidSynth would need our own build (no OpenMP) and still carries the locks.
3. Runtime loading of `libfluidsynth.so.3` works fine (no -dev package), so FluidSynth
   stays available as a fallback engine.
4. rustysynth-ext's SF3 load is 6× slower than FluidSynth's (single-threaded Vorbis
   decoding). Fine for a background loader; can be improved by decoding samples in parallel
   or caching decoded PCM in `~/.cache/merlin`.
5. SF3 costs full PCM memory once loaded (~206 MiB for MuseScore General). Plan §4.9's
   shared sample loading (one copy per file across slots) matters.

**Round 2: oxisynth** (what `../musepractice` uses with MuseScore_General.sf3).

6. **rustysynth (and the -ext fork) discard SF2 modulators** (`pmod`/`imod` are parsed and
   dropped; musepractice found this the hard way). MuseScore General relies on them, which
   matches the listening report: rustysynth-ext left some sounds missing in the SF3 render.
7. **oxisynth** is a pure-Rust port of FluidSynth: same presets, names and levels as
   FluidSynth on both files, SF2 modulators, SF3 via lewton, and the fastest render
   (341–360× realtime). SF3 load is as slow as rustysynth-ext's (~2.4 s, single-threaded
   Vorbis decode), which a background loader hides.
8. **RTSan on oxisynth: one allocation site.** `core::midi::inner_noteon` builds
   `let mod_list_new: Vec<Option<&Mod>> = (0..64).map(|_| None).collect();` on every
   note-on (malloc + realloc). The fix is a stack array (`[None; 64]`); upstream it or carry
   a patch (`[patch.crates-io]`). Everything else (note-off, program change, render) is clean.
9. The WAVs are now written RMS-matched (−20 dBFS), because the engines' default gains
   differ a lot (FluidSynth/oxisynth 0.2) and louder tends to sound "better".
   `just listen` plays FluidSynth → oxisynth → rustysynth-ext per file.

**Decision: oxisynth** ✅ (confirmed by listening test: "way better"), with the note-on allocation fixed. FluidSynth's sound
and SoundFont compatibility without its locks, OpenMP or a C dependency, and already proven
in musepractice with the same SF3. rustysynth-ext is out (no modulators). FluidSynth stays a
fallback only.
