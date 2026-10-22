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


def run(script, controls, seconds, keep=None):
    """Runs the app with the script and the controller; returns the app's output."""
    work = Path(tempfile.mkdtemp(prefix="irig-e2e-"))
    env = dict(os.environ, MERLIN_SCRIPT=script)
    for var, d in (("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache"), ("XDG_CONFIG_HOME", "config")):
        env[var] = str(work / d)
    app = subprocess.Popen([str(APP), "--fresh"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env)
    time.sleep(6)
    ctl = subprocess.Popen([str(CONTROLLER), "Fake Controller", controls, str(seconds)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        out, _ = app.communicate(timeout=60)
    except subprocess.TimeoutExpired:
        app.kill()
        out, _ = app.communicate()
    ctl.terminate()
    if keep is not None:
        keep(work)
    shutil.rmtree(work, ignore_errors=True)
    return out


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


SNAP_FILE = Path(tempfile.gettempdir()) / "irig-e2e-snapshots.irig"
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
    work = Path(tempfile.mkdtemp(prefix="irig-stems-"))
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


if __name__ == "__main__":
    main()
