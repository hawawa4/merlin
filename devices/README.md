# Device support

Controllers (pads, knobs, faders, screens) are supported by small crates in this folder that
know **nothing about the app**. The app, a DAW plugin, or a command-line tool can use them; so
can anyone who forks one to make the pads do something else.

```
devices/
  surface-api/     the vocabulary and the two traits (Driver, Surface). No dependencies.
  launchkey-mk3/   a driver: Novation Launchkey MK3 (25/37/49/61/88)
  <your-device>/   one crate per device family
```

## How it fits together

```
 MIDI ports                host (this app, a plugin, ...)                     driver
 ─────────                 ──────────────────────────────                     ──────
 device -> bytes ───────►  mappings from the device's Description    ◄─────   Description (what it has)
                           what the controls are *for* (policy)
 device <- bytes ◄───────  View (lights, text, labels)  ───────────►  render() -> bytes
                           connect()/disconnect()/input(bytes)
```

* A **driver** says what a device *has* (`Description`: pads in pages, knobs, faders, buttons
  with well-known roles, a screen) and turns a **`View`** (colours per control id, text) into
  MIDI bytes. It knows the SysEx, the colour table, the mode switches.
* The **host** decides what controls *do*. This app's policy (first pad row selects instruments,
  knobs are sound controls, ...) lives in `crates/control/src/surface.rs` and works from the
  `Description`, so any device with pads and knobs gets it.
* No I/O in the drivers: bytes in, bytes out. A host can run one anywhere.

## Writing a driver for a new device

1. `cargo new --lib devices/<name>`, depend on `surface-api`.
2. Implement `Driver::detect(port_name)` (match the device's control port by name) and a
   `Surface`: `description()`, `connect()`, `disconnect()`, `input()`, `page()`, `render()`.
   `launchkey-mk3` is a complete example (about 500 lines with tests).
3. Test it by bytes: no hardware is needed for most of it (see its tests). `render` must send
   only what changed.
4. Add it to `app/Cargo.toml` as an optional dependency and to `app/src/drivers.rs`.

## Where this is going

Today the drivers are compiled into the app (each behind a cargo feature, so a build can leave
them out). The API is built so that is not the only way:

* all its data is plain values (with the `serde` feature they serialise), and nothing in it
  holds a callback or a pointer;
* a driver could therefore run **out of process** (a small program per device family that speaks
  JSON lines on stdin/stdout, found in a drivers folder and downloaded on demand), or as a
  **WebAssembly module**, without the app's policy changing;
* the same drivers can sit behind a DAW plugin or a standalone tool.

Not built yet; see `docs/plan.md` (Phase 4).
