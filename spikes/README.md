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
| S3 VST3 | ✅ Rust-only hosting works (`vst3` crate, no C++ shim): sound, embedded editor, run loop, state; ⏳ editor→host edits and keyboard need a manual test |
| S4 Wine | ✅ Windows Surge XT (CLAP) embedded via yabridge master + Wine 11.20; ⏳ mouse after scroll/move needs a manual test |
| S5 UI | ✅ rack of 8 slots (Linux + Windows CLAP), one dock, keyboard map, editor cache, overlap rule; ⏳ clicking the rack and keyboard needs a manual test |
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
the knob moved (~2×). **Update (round 3): fixed by the two-level dock** (S4 fix below).
JUCE also learns its screen position from `ConfigureNotify`, so its warp-back after a drag
went to the wrong place. The explanation below is wrong, kept for the record. Likely cause: Surge hides the cursor while dragging and warps it back
onto the knob on release (JUCE `setMousePosition` → `XWarpPointer`). XWayland/KWin ignores
pointer warps from X11 clients, so the cursor stays where the physical mouse went, and
Surge's drag sensitivity is not 1:1. Not an embedding bug: any X11 host on Wayland
should show it. To confirm: switch Surge's mouse behaviour to "Exact" (Surge menu →
Mouse Behavior), or try the same plugin in another X11 host (Carla, REAPER) on this desktop.
Long-term fix is the native-Wayland path (nested compositor, plan §4.6), where we can honour
warps for embedded editors.

## S3: VST3 hosting (`spikes/s3-vst3-embed`)

`just dev` (or `just s3 [plugin.vst3]`; `just s3-script` for a scripted run). The S2 shell with a
minimal VST3 host written on the `vst3` 0.3 crate (MIT/Apache, raw bindings plus COM helpers).
**Decision: go for Rust-only VST3; Plan B (C++ shim) not needed.** About 1,200 lines, built in
one session.

**Result (Surge XT 1.3.4 and Surge XT Effects, Linux VST3):**

- Loads in 37–59 ms, editor embeds in 130 ms (reopen 61 ms), idle CPU ~2% of one core,
  0 xruns at 128 frames, clean shutdown.
- Sound: output peak 0.27 on a held note, 0 otherwise. Effects plugin (two input buses, no
  event input) loads and processes too.
- State: `component.getState` gives 51,900 bytes (Surge's patch XML); restoring brings the
  processor back **byte-identically** (re-saved blob equals the saved one) and the editor
  follows (verified by screenshots of the Volume slider: 0.2 → 1.0 → restored 0.2).

**Findings**

1. **Editor embedding:** `createView("editor")` → `setContentScaleFactor` → `setFrame` →
   `getSize` → `attached(dock window, "X11EmbedWindowID")`, then size the dock canvas (same
   two-level dock and `ConfigureNotify` trick as S4). Fit: `checkSizeConstraint` then `onSize`.
   Plugin-initiated resize arrives in `IPlugFrame::resizeView`; the host must answer with
   `onSize`, and must ignore the echo of its own `onSize` or it loops.
2. **`IRunLoop` lives on the frame object** (`IPlugFrame` + `IRunLoop` in one class). Surge
   registers 2 fds (JUCE message queue) and no timers; we service them from the winit loop like
   S2's CLAP timers/fds.
3. **The host must carry parameters between controller and processor.**
   - `IComponentHandler::performEdit` (editor edits) → input `IParameterChanges` on the next
     `process()`.
   - Processor `outputParameterChanges` → `controller.setParamNormalized` on the main thread.
     Without this the controller's values are stale (all 0.000 at start).
   - Call `controller.setComponentState(component state)` once after activation.
4. **Surge's controller cache lags after `setState`:** `getParamNormalized` keeps the old
   value although the editor and processor are restored. Don't use it as proof of a restore.
   `setComponentState` itself triggers `restartComponent(kParamValuesChanged)`, so re-syncing
   on that flag recurses forever (seen).
5. **Audio thread:** event list, parameter changes and their queues are preallocated COM
   objects (`ComWrapper` + `UnsafeCell`); no allocation in `process()`. VST3 has no raw MIDI:
   notes are note events, CC/aftertouch/pitch bend go through `IMidiMapping` → parameters.
6. **Known gaps (the long tail):** `IHostApplication::createInstance` returns "false" (no
   `IMessage`/`IAttributeList`), so plugins that message component↔controller will fail;
   no `IUnitInfo`/programs, no multi-bus (only the first audio bus is activated and played),
   no sample-accurate param ramps, no `IProcessContextRequirements`. The main thread is not
   woken for processor-reported parameter changes (spike polls on any wakeup).
7. **API notes:** `Class::Interfaces` tuples give `queryInterface` for free
   (`(IPlugFrame, IRunLoop)`); `ComPtr::from_raw` takes ownership, so retain plugin-passed
   handlers via `ComRef::to_com_ptr`. Call `ModuleEntry(dlopen handle)` and `ModuleExit` after
   all objects are released.

**Manual test still needed:** drag a knob in the embedded editor and check the sound changes
(exercises `performEdit` → processor), type a patch name (keyboard focus), and play from a
MIDI keyboard.

## S4: Windows plugins via Wine + yabridge (`just s4-setup`, `just s4`)

- **yabridge** built from master `b580a9f7` (2026-08-02, 57 commits after 5.1.1: Wine 10/11
  embedding rework, cursor-offset fixes) in its upstream CI image
  `robbert/docker-yabridge:focal` (at the time of the spike; it is now vendored in `vendor/yabridge`
  and built against our own Wine by `tools/build-yabridge.sh`, `just yabridge-build`). No host
  build dependencies; the system Wine has no headers, and we need a reproducible build to
  ship anyway. Output in `runtime/yabridge-<commit>/` (gitignored).
- **Wine:** 11.20 Staging from `/opt/wine-staging`, pinned with `WINELOADER`. Test prefix in
  `test-assets/wine/prefix`; **first `wineboot` takes ~25 s** (the app needs a progress
  step for this).
- **No global install needed.** A bridge is two files in a folder we own:
  `Surge XT.clap` (a copy of `libyabridge-chainloader-clap.so`) and `Surge XT.clap-win`
  (link to the Windows plugin inside the prefix; yabridge finds the prefix by walking up
  from it). The chainloader finds `libyabridge-clap.so` and the host next to
  `yabridge-host.exe` on `PATH`. So the app sets `PATH` and `WINELOADER` for itself and keeps
  its bridges in its own folder: nothing in `~/.clap`, no yabridgectl.
- **Result (Surge XT 1.3.4 win64 CLAP, through the unchanged S2 host):**
  - Loads in **3.5–3.6 s** (Wine start-up), editor embeds in 556–644 ms, **reopen 76 ms**.
  - Window tree: our dock → yabridge wrapper → Wine window "yabridge plugin". Renders
    correctly at the same size and HiDPI scale as the Linux build (1427×889 at 1.25).
  - Fit to dock (1335×833), close/reopen and scrolling (moving the wrapper) all work.
  - State save/restore works (51,869 bytes, same as the Linux build).
  - 0 xruns at 128 frames; idle CPU **2.9%** of one core including Wine (wineserver ~1.8%).
- **yabridge warnings:** `realtime: no` and a memlock limit of 8 MiB (it wants to lock its
  shared audio buffers). Both go away with the same fix as S1 (the `pipewire` group grants
  rtprio 95 and 4 GiB memlock), so the app's performance check must cover memlock too.
- **Round 2 (manual test): no input at all reached the Windows editor.** It rendered and
  played, but knobs and buttons ignored the mouse. Root cause (yabridge editor debug log,
  `YABRIDGE_DEBUG_LEVEL=1+editor`): Wine learns its window's screen position only from the
  synthetic `ConfigureNotify` that yabridge sends when *our* parent window (or the window
  it believes is our top-level) gets a `ConfigureNotify`. Our dock was placed before
  `set_parent` and never configured again, so Wine got **zero** position updates and mapped
  every click elsewhere. Two more traps found on the way:
  - Scrolling by moving the plugin's window would desync Wine by the scroll amount.
  - yabridge picks the "host window" as the topmost ancestor with `WM_WINDOW_ROLE`, else the
    direct parent; winit sets no role, so window moves would be missed.
  **Fix (in `dock.rs`):** clip window + canvas window (the plugin's parent); size the canvas
  after `set_parent`, nudge it (+1 px and back) after embedding and whenever the clip moves,
  scroll by moving the canvas, and set `WM_WINDOW_ROLE` on our top-level. Debug log now
  shows `Translated coords … +170+100`, following scrolls (`+170+70`) and fits; host window
  = our top-level. Native Surge unaffected.
- **Round 3 (manual test): still "completely broken".** Wine's event trace
  (`WINEDEBUG=+event,+cursor`) showed the user's real clicks and drags arriving at the right
  coordinates and Surge reacting (`request_flush` after a drag). Input worked; the **editor
  never redrew after its first frame**. Proven with Surge's voice counter: native shows
  "3 / 16" during a chord, the Windows build stays at "0 / 16" (also on the real screen via
  Spectacle). Same on Wine 9.21 + yabridge 5.1.1 and with both wined3d renderers.
- **Compatibility matrix** (`just s4-matrix`, `tools/s4-matrix.sh`): loads each bridge,
  screenshots the plugin window, sets a parameter from the host and plays a chord,
  screenshots again; a live editor changes pixels. Control: native Surge `yes(756)`.

  | Plugin (Windows CLAP) | GUI framework | Plain Wine 11.20 | + DXVK 3.1.1 | Our patched Wine (WaitForVBlank) |
  |---|---|---|---|---|
  | TyrellN6 3.0 beta | u-he | ✅ redraws | ✅ redraws | ✅ redraws |
  | Surge XT 1.3.4 | JUCE | ❌ frozen | ✅ redraws | ✅ redraws |
  | Six Sines (nightly) | JUCE | ❌ frozen | ✅ redraws | ✅ redraws |
  | OB-Xf (nightly) | JUCE 8 + Direct2D | ❌ first frame, then frozen | ❌ white, never draws | ✅ redraws (with the DirectComposition patches below) |
  | CardinalSynth 26.02 | DPF/OpenGL | 💥 Wine stack overflow in `set_parent` | not tried | 💥 same |
  | Podolski 1.2.3 | u-he | VST3 only, needs S3 | |

  Load 1.5 s warm (3.5 s cold), embed 0.1–1.1 s.
- **Root cause of the frozen JUCE editors: Wine stubs `IDXGIOutput::WaitForVBlank`.**
  Wine master `dlls/dxgi/output.c` returns `E_NOTIMPL` (the log shows
  `dxgi_output_WaitForVBlank stub!`). Modern JUCE (`juce_VBlank_windows.cpp`) drives *all*
  repaints from a per-monitor thread that loops on `WaitForVBlank() == S_OK`; on failure
  it just `sleep(1)`s forever, so `onVBlank` never fires and nothing repaints. DXVK ships its
  own `dxgi.dll` with a working `WaitForVBlank`, which is why it fixes Surge and Six Sines.
  JUCE is the most common plugin framework, so this affects a large share of plugins.
- **Fix options:** (1) **patch our bundled Wine** to implement `WaitForVBlank` (wait for the
  next refresh via `D3DKMTWaitForVerticalBlankEvent`, or emulate with a refresh-rate timer),
  ~15 lines, and upstream it: fixes JUCE without DXVK side effects; (2) DXVK, which helps
  but broke OB-Xf (white editor), so it can't be the blanket answer; per-plugin quirks are
  possible as a fallback.
- **Patched Wine (`just wine-build`, `just s4-matrix-vblank`).** `tools/build-wine.sh` builds
  Wine Staging 11.20 plus `build-support/wine-patches/*.patch` in Docker (64-bit only, about
  4 minutes on 16 cores, output `runtime/wine-11.20-vblank/`; the prefix is a copy of the
  plain one, `test-assets/wine/prefix-vblank`). The one patch (`dlls/dxgi/output.c`) makes
  `WaitForVBlank` sleep to the next refresh boundary (QPC-based, refresh rate from the
  wined3d display mode, 60 Hz fallback) and return `S_OK`. Result: **Surge XT and Six Sines
  redraw** (Surge's voice counter shows "4 / 16" during the chord) with no DXVK, and the
  `WaitForVBlank stub!` fixme is gone. No other plugin changed behaviour, except OB-Xf:
  - **(Fixed later, see "DirectComposition" below.) OB-Xf was blank instead of frozen.** Plain Wine paints one correct frame; with a
    working vblank JUCE's repaint thread runs, and its Direct2D renderer (JUCE 8) calls
    `IDXGIFactory2::CreateSwapChainForComposition` on every frame (1773 times in 15 s). Wine
    stubs it (`dxgi_factory_CreateSwapChainForComposition ... stub!`), so nothing is drawn.
    DXVK is white for the same reason. This is a second Wine gap behind the first one: JUCE 8
    plugins built with the Direct2D renderer need composition swap chains (or a DirectComposition
    implementation). Options: implement it in Wine (swap chain for composition backed by a
    plain HWND swap chain, which is what the first frame used), or a per-plugin fallback.
  - Untested: CPU cost of the vblank threads (one per output, 60 wakeups/s each, no spinning).
- **DirectComposition (patches 0002 and 0003): JUCE 8 / Direct2D plugins such as OB-Xf now draw.**
  JUCE's Direct2D renderer needs `CreateSwapChainForComposition` plus `dcomp.dll`, which Wine
  stubbed completely. 0002 implements the composition swapchain in dxgi: it lives on an internal
  hidden window and presents to the HWND that dcomp attaches it to (`dst_window_override`, private
  data `WINE_DXGI_COMPOSITION_TARGET`). JUCE repaints only dirty rectangles and relies on the
  compositor keeping the rest of the previous frame, but wined3d does not keep the back buffer's old
  contents across a present (first frame complete, then only the repainted rectangles showed, found by
  hovering in the real app). So the swapchain keeps a private "shadow" texture of the last composed
  frame: each present copies the dirty rectangles into it and copies the whole frame back before
  presenting. 0003 is a minimal `dcomp.dll`: device, target and visual objects; `Commit` binds each
  visual's swapchain to its target window; transforms, clips and surfaces are not implemented.
  Verified on the real desktop (`import -window` shows GL-drawn windows unreliably; use Spectacle).
  Proton does not help: its `dcomp.dll` is the stock stub and DXVK only offers a dummy composition
  swapchain that never shows anything.
  Rebuild after editing the Wine tree: `tools/wine-rebuild.sh` (seconds, incremental).
- **First-run dialogs:** a fresh prefix offers to install Wine Mono (and Gecko). The app must
  set `WINEDLLOVERRIDES="mscoree,mshtml="` when creating prefixes, or ship Mono preinstalled.
- **Manual test still needed:** click and drag the Windows Surge's controls (does the
  pointer land where you click?), then scroll the dock and move the main window and click
  again (the cursor-offset bugs reported for Wine 10+ show up after moving the window),
  open its menus, and type in its patch-name field.

## S5: rack UI (`spikes/s5-rack`, `just dev` / `just s5` / `just s5-script`)

An egui rack of 8 slots on our winit loop (forced X11), a keyboard map, and one editor dock.
Each slot holds any CLAP plugin (Linux, or a Windows plugin through yabridge on our patched
Wine); **one JACK callback runs all of them**, routes MIDI and mixes the outputs. About 1,500
lines on top of the S2 host code. CLAP only: VST3 slots would reuse S3 and add nothing new to
the questions S5 asks. Selection: click = exclusive, shift-click = layer. Plugins are found in
`test-assets/dl/surge-linux` and `test-assets/bridges-vblank/clap` (or the directories given as
arguments). `S5_PRELOAD="A; B"` fills slots 1.. at start; `S5_SCRIPT` runs timed actions (`load`,
`select`, `layer`, `zone`, `on`/`off`, `peak`, `cpu` with a per-process breakdown, `menu`, `report`,
`quit`; see `parse_script`).

**Result (Linux Surge XT x2, Windows Surge XT, Six Sines, TyrellN6, OB-Xf in six slots):**

- **Six live editors at once, instant switching.** Each slot has its own canvas window in the
  dock; the selected one is mapped, the rest are `gui.hide()`n and unmapped. Switching to a
  created editor takes 15-350 µs. First creation: Linux 57-135 ms, Windows 175-530 ms. Reopening
  after a destroy: Surge 78 ms, Six Sines 10-50 ms. Plugin load: Linux 30-50 ms, Windows 0.65 s
  (2.7 s for the first Wine start).
- **Idle cost with an editor open: 2.0-2.3% of one core** for our process, of which the JACK
  data-loop thread running the six plugins is 1.4% and the UI thread 0.5%; DSP load 4.6% of the
  buffer budget at 128 frames, **0 xruns**, also with a 4-note chord layered over all six slots
  (our process 3.8%). The Windows plugins add **about 12% more across their yabridge hosts and
  wineserver** (14% total idle, 24% under that chord), and it does not depend on which editor
  is visible: Six Sines' host burns 6-7% with its editor hidden, so `hide()` does not quiet a
  bridged plugin. Budget this when the app offers Windows plugins (a "sleeping slot" policy,
  plan Phase 2).
- **Routing and note ownership work:** a note-on goes to every active, unmuted slot whose zone
  contains the key and is remembered per key; the note-off goes to the remembered slots, so
  changing selection or zones with a held key leaves no stuck note (the keyboard map shows
  each held note's owner). Per-slot peaks confirmed that exclusive selection sounds only the
  selected slot. A slot is installed and removed through a lock-free command queue; a removed
  processor is handed back to the main thread, so the callback never allocates or frees.
- **Overlap rule works:** while an egui popup is open (`Popup::is_any_open`) the dock is
  unmapped and the popup draws over plain egui ("editor hidden while a menu is open"); the
  editor returns when the menu closes. Checked with screenshots (`menu` script action opens a
  slot's menu).
- **Per-slot fd poller** (one helper thread per slot, ended on unload) and per-slot timers are
  serviced from the one winit loop; wakeups stay under 1 per second while idle. The UI redraws at
  2 Hz when nothing sounds and 20 Hz while notes or meters move.

**Findings that change the plan**

1. **Never destroy a bridged editor in the same turn as showing another.** Destroying Surge's
   (hidden) editor right after creating Six Sines' made Wine's x11drv abort the Surge host with
   `BadWindow (X_GetProperty)` on its own wrapper window, deterministically, with the system Wine
   too. The same destroy a moment later (any age, shown or hidden) is fine, and so is
   destroying an older editor in the Six-Sines-first order. Fix: evict editors from the main
   loop 300 ms after a switch (`EVICT_DELAY`). Three bridged plugins and constant churn ran clean.
2. **Keep a slot's canvas window for the slot's life** (reuse it for the next editor). It is cheap,
   and destroying the parent of an editor that tears down asynchronously is the same class of
   race.
3. **Editor eviction is needed at 8 slots only for Windows plugins**, which cost a Wine process
   each; Linux editors are cheap to keep. The cap (`S5_EDITOR_CACHE`, default 8) is ready.
4. **JUCE 8 / Direct2D plugins (OB-Xf) stay blank** under our patched Wine, as in S4: they
   load, route and sound, but their editor is empty. Not an S5 problem.
5. egui text lacks some symbols (●, ✕ rendered as boxes); draw LEDs with the painter.

**Manual test still needed:** click the rack (select, shift-click layer, M, zone fields, Load/
Replace menus, x), click and drag on the keyboard map (plays the held key into the active
slots), then use a plugin's knobs after switching slots and after a menu closed (mouse mapping
after the dock was unmapped and mapped again).

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
