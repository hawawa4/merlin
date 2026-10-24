#!/usr/bin/env python3
"""End-to-end check of MIDI mappings with a pretend controller (no hardware, needs JACK/PipeWire).

    just e2e-controls

Starts the app with three SoundFont slots and mappings for a foot switch (next, previous), a
button (select slot 2), Program Change (slot by number), a fader with pick-up and the master
volume, and a "Fake Controller" that presses them in turn. The app reports its state every
second; the sequence of selected slots and volumes must be what the mappings say.
"""
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "target/release/merlin"
CONTROLLER = ROOT / "target/release/examples/fake_controller"

SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb", "1.2:load 2 TimGM6mb", "1.4:load 3 TimGM6mb",
        "4:map cc 20 select 2", "4.1:map cc 21 next", "4.2:map cc 22 prev",
        "4.3:map program byvalue 1", "4.4:map cc 7 volume 1 pickup", "4.5:map cc 9 master",
        "5:select 1",
    ]
    + [f"{t}:report" for t in (7.5, 8.5, 9.5, 10.5, 11.5, 12.5, 13.5, 14.5, 15.5, 16.5, 17.5)]
    + ["18:quit"]
)
# What the controller sends, one thing per second (data bytes in decimal).
CONTROLS = (
    "1:B0 20 127; 2:B0 20 0; 3:B0 21 127; 4:B0 22 127; 5:C0 0; 6:C0 2; "
    "7:B0 7 10; 8:B0 7 64; 9:B0 7 127; 10:B0 7 64; 11:B0 9 64"
)


def run(script, controls, seconds, keep=None, args=(), controller=None, name="Fake Controller", config=None):
    """Runs the app with the script and the controller; returns the app's output (with `controller`
    set to another program, its output too: `(app, controller)`)."""
    work = Path(tempfile.mkdtemp(prefix="merlin-e2e-"))
    env = dict(os.environ, MERLIN_SCRIPT=script)
    for var, d in (("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache"), ("XDG_CONFIG_HOME", "config")):
        env[var] = str(work / d)
    if config:
        (work / "config/merlin").mkdir(parents=True)
        (work / "config/merlin/settings.toml").write_text(config)
    app = subprocess.Popen([str(APP), "--fresh", *args], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env)
    time.sleep(6)
    ctl = subprocess.Popen([str(controller or CONTROLLER), name, controls, str(seconds)],
                           stdout=subprocess.PIPE if controller else subprocess.DEVNULL, stderr=subprocess.DEVNULL, text=True)
    try:
        out, _ = app.communicate(timeout=60)
    except subprocess.TimeoutExpired:
        app.kill()
        out, _ = app.communicate()
    ctl_out = ""
    if controller:
        # It outlives the app by a moment, to hear what the app said on leaving.
        try:
            ctl_out, _ = ctl.communicate(timeout=seconds + 5)
        except subprocess.TimeoutExpired:
            ctl.terminate()
            ctl_out, _ = ctl.communicate()
    else:
        ctl.terminate()
    if keep is not None:
        keep(work)
    shutil.rmtree(work, ignore_errors=True)
    return (out, ctl_out) if controller else out


def main():
    for p in (APP, CONTROLLER):
        if not p.exists():
            sys.exit(f"missing {p}: just e2e-controls (builds both)")
    controls_check()
    snapshots_check()
    macros_check()
    selection_check()
    setlist_check()
    transport_check()
    backing_check()
    timeline_check()
    targets_check()
    touch_check()
    capture_check()
    launchkey_check()


def read_smf(data):
    """Reads the files merlin writes (no running status): (format, division, [(name, [(tick, bytes)])])."""
    import struct

    assert data[:4] == b"MThd"
    fmt, count, division = struct.unpack(">HHH", data[8:14])
    i, tracks = 14, []
    for _ in range(count):
        assert data[i:i + 4] == b"MTrk"
        (n,) = struct.unpack(">I", data[i + 4:i + 8])
        body, i = data[i + 8:i + 8 + n], i + 8 + n
        j, at, name, events = 0, 0, "", []

        def vlq():
            nonlocal j
            v = 0
            while True:
                b = body[j]
                j += 1
                v = v << 7 | (b & 0x7F)
                if not b & 0x80:
                    return v

        while j < len(body):
            at += vlq()
            status = body[j]
            j += 1
            if status == 0xFF:
                kind = body[j]
                j += 1
                size = vlq()
                if kind == 0x03:
                    name = body[j:j + size].decode()
                j += size
            else:
                size = 1 if status & 0xF0 in (0xC0, 0xD0) else 2
                events.append((at, tuple([status, *body[j:j + size]])))
                j += size
        tracks.append((name, events))
    return fmt, division, tracks


CAPTURE_SCRIPT = "4:map cc 25 capture; 14:quit"
# A note-off whose note-on is before the window, notes with a pedal, a note still held when the
# capture button (a mapped control, CC 25) is pressed, and a controller of no interest.
CAPTURE_CONTROLS = (
    # (Status bytes need the 0x: the pretend controller reads two plain digits as decimal.)
    "0.5:0x80 62 0; 1:0x90 60 100; 1.5:0x80 60 0; 2:0x90 64 100; 2.5:0xB0 64 127; 3:0x80 64 0; "
    "3.5:0x90 67 100; 4:0xB0 20 5; 5:0xB0 25 127"
)


def capture_check():
    out_dir = Path(tempfile.mkdtemp(prefix="merlin-e2e-capture-"))
    try:
        out = run(CAPTURE_SCRIPT, CAPTURE_CONTROLS, 8, config=f'capture_dir = "{out_dir}"\n')
        files = sorted(out_dir.glob("Merlin capture *.mid"))
        problems = []
        if len(files) != 1:
            problems.append(f"{len(files)} capture files: {[f.name for f in files]}")
        else:
            fmt, division, tracks = read_smf(files[0].read_bytes())
            names = [t[0] for t in tracks]
            if fmt != 1 or division != 960 or len(tracks) != 2 or names[1] != "Fake Controller":
                problems.append(f"format {fmt}, division {division}, tracks {names}")
            else:
                events = tracks[1][1]
                notes = [e[1] for e in events]
                want = [
                    (0x90, 60, 100), (0x80, 60, 0), (0x90, 64, 100), (0xB0, 64, 127),
                    (0x80, 64, 0), (0x90, 67, 100), (0xB0, 20, 5), (0x80, 67, 0), (0xB0, 64, 0),
                ]
                if notes != want:
                    problems.append(f"messages {notes}\n  wanted {want}")
                else:
                    # At 120 bpm: half a second is 960 ticks (the pretend device is not exact).
                    for k, ticks in ((1, 960), (2, 1920), (3, 2880), (4, 3840)):
                        if abs(events[k][0] - events[0][0] - ticks) > 200:
                            problems.append(f"message {k} at {events[k][0]}, wanted about {ticks}")
        if "saved " not in out:
            problems.append("the app did not say it saved")
        if problems:
            print("FAIL (capture)\n  " + "\n  ".join(problems))
            print(out[-3000:])
            sys.exit(1)
        print(f"ok: the capture button (a mapped control) saved {files[0].name}: 2 tracks, notes, pedal and a held note closed")
    finally:
        shutil.rmtree(out_dir, ignore_errors=True)


SNAP_FILE = Path(tempfile.gettempdir()) / "merlin-e2e-snapshots.mrig"
SNAP_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb", "1.2:load 2 TimGM6mb", "1.4:load 3 TimGM6mb",
        # State A: slot 1 plays at full volume. State B: slot 3 plays, slot 1 at 0.4.
        "4:select 1", "4.1:volume 1 1.0", "4.2:snapshot save",
        "4.3:select 3", "4.4:volume 1 0.4", "4.5:snapshot save",
        "4.6:map cc 30 snap 1", "4.7:map cc 31 snap 2", "4.8:map cc 32 snapnext",
        "4.9:map program snapbyvalue 1",
        f"5:save {SNAP_FILE}",
    ]
    + [f"{t}:report" for t in (7.5, 8.5, 9.5, 10.5)]
    + ["12:quit"]
)
SNAP_CONTROLS = "1:B0 30 127; 2:B0 31 127; 3:B0 32 127; 4:C0 1"


def snapshots_check():
    import json
    out = run(SNAP_SCRIPT, SNAP_CONTROLS, 8)
    seq = [(float(v), t.strip()) for v, t in re.findall(r"vol1 ([\d.]+), targets \[([\d, ]*)\]", out)]
    wanted = [(1.0, "1"), (0.4, "3"), (1.0, "1"), (0.4, "3")]
    problems = []
    if seq[:4] != wanted:
        problems.append(f"states {seq}, wanted {wanted}")
    try:
        rig = json.loads(SNAP_FILE.read_text())
        if len(rig.get("snapshots", [])) != 2:
            problems.append(f"saved rig has {len(rig.get('snapshots', []))} snapshots")
    except Exception as e:  # noqa: BLE001
        problems.append(f"saved rig unreadable: {e}")
    finally:
        SNAP_FILE.unlink(missing_ok=True)
    if problems:
        print("FAIL (snapshots)\n  " + "\n  ".join(problems))
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: snapshots recalled by button, next and Program Change: {seq[:4]}")


MACRO_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb",
        # Macro 1 of slot 1 drives the SoundFont's level (range 0..2); it starts where the level is.
        "4:macrotarget 1 1 sampler:level", "4.1:map cc 40 macro 1 1", "4.2:snapshot save",
        "4.3:map program snapbyvalue 1",
    ]
    + [f"{t}:report" for t in (5, 7.5, 8.5, 9.5, 10.5)]
    + ["12:quit"]
)
MACRO_CONTROLS = "1:B0 40 127; 2:B0 40 0; 3:C0 0; 4:B0 40 64"


def macros_check():
    out = run(MACRO_SCRIPT, MACRO_CONTROLS, 8)
    seq = [(float(m), float(g)) for m, g in re.findall(r"macro1 ([\d.]+), gain1 ([\d.-]+)", out)]
    wanted = [(0.5, 1.0), (1.0, 2.0), (0.0, 0.0), (0.5, 1.0), (0.5, 1.0)]
    problems = []
    if len(seq) != 5 or any(abs(a - c) > 0.02 or abs(b - d) > 0.02 for (a, b), (c, d) in zip(seq, wanted)):
        problems.append(f"(macro, gain) {seq}, wanted {wanted}")
    if problems:
        print("FAIL (macros)\n  " + "\n  ".join(problems))
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: a macro knob moves a SoundFont's level, and a snapshot restores it: {seq}")


SEL_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb", "1.2:load 2 TimGM6mb",
        "4:macrotarget 1 1 sampler:level", "4.1:macrotarget 2 1 sampler:level", "4.2:select 1",
        # An endless encoder (two's complement, 10% a step) turning the macro of the slot that plays.
        "4.3:map cc 41 selmacro 1 rel2c step=0.1",
        "9.8:select 2",
    ]
    + [f"{t}:report" for t in (5, 7.5, 8.5, 10.5)]
    + ["12:quit"]
)
# Up two steps, down one, then (after slot 2 is selected) up five: that must go to slot 2.
SEL_CONTROLS = "1:B0 41 2; 2:B0 41 127; 4:B0 41 5"


def selection_check():
    out = run(SEL_SCRIPT, SEL_CONTROLS, 8)
    seq = [(float(m), float(g)) for m, g in re.findall(r"macro1 ([\d.]+), gain1 ([\d.-]+)", out)]
    wanted = [(0.5, 1.0), (0.7, 1.4), (0.6, 1.2), (0.6, 1.2)]
    if len(seq) != 4 or any(abs(a - c) > 0.02 or abs(b - d) > 0.03 for (a, b), (c, d) in zip(seq, wanted)):
        print(f"FAIL (selection)\n  (macro, gain) {seq}, wanted {wanted}")
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: an encoder moves the macro of the slot that plays, and follows the selection: {seq}")


SET_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb",
        "4:song add", "4.05:song add", "4.1:song tempo 1 100", "4.15:song tempo 2 90",
        # Three sounds, told apart by slot 1's volume: A 1.0 and B 0.4 in song 1, C 0.7 in song 2.
        "4.2:volume 1 1.0", "4.25:part add 1",
        "4.3:volume 1 0.4", "4.35:part add 1", "4.4:part tempo 1 2 140",
        "4.5:volume 1 0.7", "4.55:part add 2",
        "4.6:volume 1 0.0",
        "4.7:map cc 60 nextpart", "4.71:map cc 61 prevpart", "4.72:map cc 62 nextsong",
    ]
    + [f"{t}:report" for t in (7.5, 8.5, 9.5, 10.5, 11.5)]
    + ["13:quit"]
)
SET_CONTROLS = "1:B0 60 127; 2:B0 60 127; 3:B0 60 127; 4:B0 61 127; 5:B0 62 127"


def setlist_check():
    out = run(SET_SCRIPT, SET_CONTROLS, 9)
    seq = [
        (float(v), p, int(tempo))
        for v, p, tempo in re.findall(r"vol1 ([\d.]+),.*position (None|Some\(\(\d+, \d+\)\)), tempo (\d+)", out)
    ]
    wanted = [(1.0, "Some((1, 1))", 100), (0.4, "Some((1, 2))", 140), (0.7, "Some((2, 1))", 90),
              (0.4, "Some((1, 2))", 140), (0.7, "Some((2, 1))", 90)]
    if seq != wanted:
        print(f"FAIL (setlist)\n  (volume, position, tempo) {seq}\n  wanted {wanted}")
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: foot switches walk the setlist, each part recalls its snapshot and tempo: {seq}")


TRANSPORT_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb",
        "2:tempo 240", "2.1:countin 1", "2.2:click off",
        "3:transport start",
        # 4 beats of count-in at 240 bpm = 1 s, then it runs.
        "3.5:report", "5:report", "7:report",
        "7.5:transport stop", "8:report",
        "9:quit",
    ]
)


def transport_check():
    out = run(TRANSPORT_SCRIPT, "1:B0 1 0", 3)
    rows = re.findall(r"transport (\w+) bar (\d+) beat (\d+) beats (\d+)", out)
    problems = []
    if [r[0] for r in rows] != ["counting", "running", "running", "stopped"]:
        problems.append(f"states {rows}")
    else:
        # 240 bpm = 4 beats a second: ~4 beats counted in by 3.5 s... then running, ~7 beats by 5 s.
        beats = [int(r[3]) for r in rows]
        if not (beats[1] > beats[0] and beats[2] > beats[1] + 4 and beats[3] >= beats[2]):
            problems.append(f"the beats do not advance at tempo: {beats}")
        if int(rows[2][1]) < 3:
            problems.append(f"the bar counter did not advance: {rows[2]}")
    if problems:
        print("FAIL (transport)\n  " + "\n  ".join(problems))
        print(out[-2500:])
        sys.exit(1)
    print(f"ok: the transport counts in, runs and stops: {rows}")


TIMELINE_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb", "2:song add", "2.1:song tempo 1 240",
        # Part 1 (volume 1.0) and part 2 (volume 0.4).
        "2.2:volume 1 1.0", "2.3:part add 1", "2.4:volume 1 0.4", "2.5:part add 1",
        # At 240 bpm a bar of 4 is one second.
        "2.6:marker 1 2 1 part 2", "2.7:marker 1 3 1 cue Chorus is coming", "2.8:marker 1 4 1 stop",
        "2.9:marker 1 1 1 pc 1 5",
        "3:go 1 1", "3.1:countin 0", "3.2:click off",
        "5:transport start",
        "5.5:report", "6.5:report", "7.5:report", "8.5:report",
        "9.5:quit",
    ]
)


def timeline_check():
    out = run(TIMELINE_SCRIPT, "1:B0 1 0", 3)
    rows = re.findall(
        r"vol1 ([\d.]+),.*position (Some\(\(\d+, \d+\)\)).*transport (\w+) bar.*cue (None|Some\(\"[^\"]*\"\))", out
    )
    wanted = [
        ("1.00", "Some((1, 1))", "running", "None"),
        ("0.40", "Some((1, 2))", "running", "None"),
        ("0.40", "Some((1, 2))", "running", 'Some("Chorus is coming")'),
        ("0.40", "Some((1, 2))", "stopped", 'Some("Chorus is coming")'),
    ]
    if rows != wanted:
        print(f"FAIL (timeline)\n  {rows}\n  wanted {wanted}")
        print(out[-3000:])
        sys.exit(1)
    print("ok: markers enter a part, show a cue and stop the transport on their bars")


def write_wav(path, rate, seconds, hz, amp):
    import math
    import struct
    import wave
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate)
        frames = bytearray()
        for i in range(int(rate * seconds)):
            s = int(math.sin(2 * math.pi * hz * i / rate) * amp * 32767)
            frames += struct.pack("<hh", s, s)
        w.writeframes(bytes(frames))


def backing_check():
    work = Path(tempfile.mkdtemp(prefix="merlin-stems-"))
    a, b = work / "stem a.wav", work / "stem b.wav"
    write_wav(a, 44100, 20, 440, 0.3)  # 44.1 kHz: resampled to the engine's rate
    write_wav(b, 48000, 20, 880, 0.2)
    script = "; ".join(
        [
            "1:load 1 TimGM6mb", "2:song add", "2.1:song tempo 1 120", f"2.2:track add 1 {a}", f"2.3:track add 1 {b}",
            "2.4:go 1 1", "2.5:countin 0", "2.6:click off",
            "4:peak", "4.5:transport start", "5:peak", "5.5:report", "7:report", "7.5:transport stop",
            "8.5:report", "9:transport start", "11:report", "12:peak", "13:quit",
        ]
    )
    out = run(script, "1:B0 1 0", 3)
    shutil.rmtree(work, ignore_errors=True)
    rows = re.findall(r"transport (\w+) bar \d+ beat \d+ beats \d+, run ([\d.]+)s, underruns (\d+), stems (\w+)", out)
    masters = [float(m) for m in re.findall(r"master:([\d.]+)", out)]
    problems = []
    if len(rows) != 4:
        problems.append(f"reports {rows}")
    else:
        running1, running2, stopped, restarted = rows
        if not (running1[0] == "running" and running1[3] == "true" and 0.5 < float(running1[1]) < 3):
            problems.append(f"first run {running1}")
        if not (float(running2[1]) > float(running1[1]) + 1.0):
            problems.append(f"the song clock does not advance: {running1} {running2}")
        if not (stopped[0] == "stopped" and stopped[3] == "false"):
            problems.append(f"not stopped: {stopped}")
        if not (restarted[0] == "running" and restarted[3] == "true" and float(restarted[1]) < 3):
            problems.append(f"the second start did not restart the tracks from the top: {restarted}")
        if any(int(r[2]) > 0 for r in rows):
            problems.append(f"the decoder fell behind: {[r[2] for r in rows]}")
    # Master peak: silent before the start; about 0.5 (0.3 + 0.2) with both tracks playing.
    if len(masters) != 3 or masters[0] > 0.01 or not (0.35 < masters[1] < 0.6) or not (0.35 < masters[2] < 0.6):
        problems.append(f"master peaks {masters}")
    if problems:
        print("FAIL (backing tracks)\n  " + "\n  ".join(problems))
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: two stems (one resampled) play with the run, stop with it and start again from the top: {rows}, peaks {masters}")


def controls_check():
    out = run(SCRIPT, CONTROLS, 13)
    states = re.findall(r"master ([\d.]+), vol1 ([\d.]+), targets \[([\d, ]*)\]", out)
    seq = [(float(m), float(v), t.strip()) for m, v, t in states]
    # The slot that plays, after each control (the reports run a second apart, the controller's
    # messages too, so the sequence has them in order, with repeats while nothing happens).
    slots = []
    for _, _, t in seq:
        if not slots or slots[-1] != t:
            slots.append(t)
    wanted = ["2", "3", "2", "1", "3"]
    problems = []
    if slots != wanted:
        problems.append(f"selected slots {slots}, wanted {wanted}")
    vols = [v for _, v, _ in seq]
    masters = [m for m, _, _ in seq]
    # The fader sits at 1.0; it is ignored until it passes that, then follows (64/127 = 0.50).
    if not (vols[0] == 1.0 and abs(vols[-1] - 64 / 127) < 0.01 and all(v == 1.0 or abs(v - 64 / 127) < 0.01 for v in vols)):
        problems.append(f"fader (pick-up) volumes {vols}")
    if not (masters[0] == 1.0 and abs(masters[-1] - 64 / 127) < 0.01):
        problems.append(f"master {masters}")
    if problems:
        print("FAIL\n  " + "\n  ".join(problems))
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: slots {slots}, fader {sorted(set(vols))}, master {sorted(set(masters))}")


TARGET_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb", "1.5:load 2 Surge XT",
        "5:map cc 50 pan 1", "5.1:map cc 51 fx 1 reverb", "5.2:map cc 52 fx 1 eq latch",
        "5.3:map cc 53 clicklevel", "5.4:map cc 54 tempo", "5.5:mapparam cc 55 2 inst Volume",
        "5.6:mapmode on", "5.7:map cc 56 fxknob 1 reverb mix", "5.8:map cc 57 soundknob 1 brightness",
    ]
    + [f"{t}:report" for t in (7.5, 8.5, 9.5, 10.5, 11.5, 12.5, 13.5, 14.5, 15.5)]
    + ["17:quit"]
)
# Pan hard right then hard left; reverb on; the latching switch on, off, and off again (stays
# off); click level, tempo, and a parameter's knob in the middle.
TARGET_CONTROLS = (
    "1:B0 50 127; 2:B0 50 0; 3:B0 51 127; 4:B0 52 127; 5:B0 52 0; 6:B0 52 0; "
    "1.5:B0 56 127; 2.5:B0 57 0; 7:B0 53 127; 8:B0 54 127; 9:B0 55 64"
)


def targets_check():
    out = run(TARGET_SCRIPT, TARGET_CONTROLS, 11, args=("--plugin-dir", str(ROOT / "test-assets/dl/surge-linux")))
    rows = re.findall(
        r"tempo (\d+),.*pan1 ([\d.-]+), fx1 (\d+), bypass1 \S*, click \w+ level ([\d.]+), mapmode (\w+), mappings (\d+), params \[(.*?)\], reverbmix1 ([\d.]+), bright1 ([\d.-]+)",
        out,
    )
    if len(rows) != 9:
        print(f"FAIL (targets)\n  {len(rows)} reports, wanted 9")
        print(out[-3000:])
        sys.exit(1)
    seq = [(int(t), float(p), fx, float(c)) for t, p, fx, c, *_ in rows]
    wanted = [
        (120, 1.0, "0000", 0.5), (120, -1.0, "0000", 0.5), (120, -1.0, "0001", 0.5),
        (120, -1.0, "1001", 0.5), (120, -1.0, "0001", 0.5), (120, -1.0, "0001", 0.5),
        (120, -1.0, "0001", 1.0), (300, -1.0, "0001", 1.0), (300, -1.0, "0001", 1.0),
    ]
    problems = []
    if seq != wanted:
        problems.append(f"(tempo, pan, fx, click level) {seq}\n  wanted {wanted}")
    last = rows[-1]
    if last[4] != "true" or last[5] != "8":
        problems.append(f"map mode {last[4]}, mappings {last[5]}")
    if not re.search(r"2#\d+=0\.50", last[6]):
        problems.append(f"the parameter did not follow its knob: {last[6]!r}")
    # A reverb knob (a built-in effect) and a SoundFont knob follow their controls too: the
    # reverb mix to the top of its range, the brightness to the bottom (-3 octaves).
    if float(last[7]) != 1.0 or float(last[8]) != -3.0:
        problems.append(f"effect / sound knobs: reverb mix {last[7]}, brightness {last[8]} (wanted 1.00 and -3.00)")
    if problems:
        print("FAIL (targets)\n  " + "\n  ".join(problems))
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: pan, effect switches (flip and latching), click level, tempo, a plugin parameter, a reverb knob and a SoundFont knob follow their controls: {seq[-1]}, {last[6]}, reverb mix {last[7]}, brightness {last[8]}")


TOUCH_SCRIPT = "; ".join(
    [
        "1:load 1 Surge XT",
        "5:mapparam cc 55 1 inst Volume",
        "6:touch 1",
        "15.5:report",
        "17:quit",
    ]
)
# CC 55 moves Surge's Volume (an old mapping); the app, learning by touch, sees the parameter move
# and takes it as the one to map; the next control moved, CC 70, is mapped to it and replaces CC 55
# (pick-up: it does nothing until it reaches the parameter, which is near the middle).
TOUCH_CONTROLS = "2:B0 55 64; 4:B0 70 100; 5:B0 70 64; 6:B0 70 127; 7:B0 70 32; 8:B0 55 0"


def touch_check():
    out = run(TOUCH_SCRIPT, TOUCH_CONTROLS, 9, args=("--plugin-dir", str(ROOT / "test-assets/dl/surge-linux")))
    last = re.findall(r"mappings (\d+), params \[(.*?)\]", out)
    problems = []
    if "now move a control on your device" not in out:
        problems.append("the app never found the parameter that moved")
    if not last:
        problems.append("no report")
    else:
        n, params = last[-1]
        if n != "1":
            problems.append(f"mappings {n}, wanted 1 (the learned one replaces the old one)")
        if not re.search(r"1#\d+=0\.25", params):
            problems.append(f"the learned control did not drive the parameter: {params!r}")
    if problems:
        print("FAIL (touch)\n  " + "\n  ".join(problems))
        print(out[-3000:])
        sys.exit(1)
    print(f"ok: a parameter that moved was found, mapped to the next control and followed it: {last[-1][1]}")


FAKE_LAUNCHKEY = ROOT / "target/release/examples/fake_launchkey"
LK_NAME = "Launchkey MK3 61 LKMK3 DAW In"
# The real one (if plugged in) is left alone: the test only talks to the pretend 61-key one.
LK_CONFIG = """
[[midi_devices]]
name = "Launchkey MK3 49: LKMK3 DAW In"
enabled = false

[[midi_devices]]
name = "Launchkey MK3 49: LKMK3 MIDI In"
enabled = false
"""
LK_SCRIPT = "; ".join(
    [
        "1:load 1 TimGM6mb", "1.2:load 2 TimGM6mb", "1.4:load 3 TimGM6mb", "4:select 1", "4.1:snapshot save",
        # Macro 3 of slot 1 drives its level: knob 3 is that macro, the other knobs their defaults.
        "4.2:macrotarget 1 3 sampler:level",
    ]
    + [f"{t}:report" for t in [7.5 + i for i in range(14)]]
    + ["23:quit"]
)
# Top pad 2 selects slot 2; Track > (CC 102 on the real device) goes on to slot 3; bottom pad 1 recalls snapshot 1 (slot 1
# again); fader button 1 mutes slot 1. Knob 1 (pan, default) picks up at the middle and goes
# right; knob 2 (reverb) picks up at the bottom and switches the reverb on; knob 3 is a macro and
# raises the level. Then Device Select held (pad 3 of the select row), back to Session, then Drum.
LK_CONTROLS = (
    "1:144 97 100; 2:BF 102 127; 3:144 112 100; 4:BF 37 127; 5:BF 21 64; 6:BF 21 127; "
    "7:BF 22 0; 8:BF 22 64; 9:BF 23 64; 10:BF 23 127; 11:BF 3 9; 12:144 66 100; 13:BF 3 2; 14:BF 3 1"
)


def launchkey_check():
    out, got = run(LK_SCRIPT, LK_CONTROLS, 20, args=(), controller=FAKE_LAUNCHKEY, name=LK_NAME, config=LK_CONFIG)
    msgs = [tuple(int(x, 16) for x in line.split()[2:]) for line in got.splitlines() if line.startswith("recv ")]
    targets = [t.strip() for t in re.findall(r"targets \[([\d, ]*)\]", out)]
    problems = []
    if not msgs or msgs[0] != (0x9F, 0x0C, 0x7F):
        problems.append(f"the first message to the device should enter DAW mode: {msgs[:1]}")
    # (PipeWire turns a Note On of velocity 0 into a Note Off, which the device takes the same.)
    if (0x9F, 0x0C, 0x00) not in msgs and not any(m[:2] == (0x8F, 0x0C) for m in msgs):
        problems.append("the app did not leave DAW mode on quitting")
    # Pad lights: slot 1 plays (pulsing, channel 3) then goes steady as slot 2, then 3 take over,
    # and pulses again when it is selected back.
    want = [(0x92, 96, 5), (0x90, 96, 5), (0x92, 97, 41), (0x92, 98, 21), (0x92, 96, 5)]
    last = -1
    for w in want:
        try:
            last = msgs.index(w, last + 1)
        except ValueError:
            problems.append(f"missing light {w} after message {last}")
            break
    snap = (0x90, 112, 21)
    if snap not in msgs:
        problems.append("the current snapshot's pad was not lit green")
    text = [bytes(m[8:-1]).decode() for m in msgs if len(m) > 8 and m[0] == 0xF0 and m[6] == 4 and m[7] == 0]
    if not any(t.startswith("1 TimGM6mb") for t in text) or not any(t.startswith("2 TimGM6mb") for t in text):
        problems.append(f"screen lines {text}")
    knob_names = [bytes(m[8:-1]).decode() for m in msgs if len(m) > 8 and m[0] == 0xF0 and m[6] == 7 and m[7] == 0x38]
    if not knob_names:
        problems.append("no name for the first knob")
    # What the controls did.
    if targets[:4] != ["2", "3", "1", "1"]:
        problems.append(f"selected slots {targets}, wanted 2, 3, 1, 1")
    if targets[11:14] != ["3", "3", "3"]:
        problems.append(f"Device Select pad 3 did not select slot 3: {targets}")
    pan = [float(x) for x in re.findall(r"pan1 ([\d.-]+)", out)]
    fx = re.findall(r"fx1 (\d+)", out)
    gain = [float(x) for x in re.findall(r"gain1 ([\d.-]+)", out)]
    if len(pan) < 14 or abs(pan[4]) > 0.02 or pan[5] < 0.99:
        problems.append(f"knob 1 (default: pan) {pan}")
    if len(fx) < 14 or fx[6] != "0000" or fx[7] != "0001":
        problems.append(f"knob 2 (default: reverb) {fx}")
    if len(gain) < 14 or abs(gain[8] - 1.0) > 0.02 or abs(gain[9] - 2.0) > 0.05:
        problems.append(f"knob 3 (a macro, so it wins over the default) {gain}")
    # Pad modes: Device Select lights the select notes, Drum mode the drum pads in the colour of
    # the slot that plays (slot 3: green).
    if not any(m[:2] in ((0x90, 64), (0x92, 64)) for m in msgs):
        problems.append("Device Select mode did not light the select row")
    if (0x99, 0x28, 21) not in msgs:
        problems.append("Drum mode did not light the drum pads in the slot's colour")
    if problems:
        print("FAIL (launchkey)\n  " + "\n  ".join(problems))
        print(out[-2500:])
        print("\n".join(got.splitlines()[:60]))
        sys.exit(1)
    print(f"ok: a Launchkey MK3's pads, buttons and screen follow the rig and drive it ({len(msgs)} messages to the device, targets {targets[:4]})")


if __name__ == "__main__":
    main()
