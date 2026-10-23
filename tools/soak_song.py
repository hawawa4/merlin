#!/usr/bin/env python3
"""Soak test of a song with backing tracks and a timeline (plan §5 exit): plays a short song over
and over, start to automatic stop, and checks every pass.

    just soak-song [PASSES]        (default 100)

Each pass: START with a one-bar count-in, two stems play, at bar 2 a marker enters part 2, at bar 3
a marker stops the transport. Every pass must show: part 2 reached, transport stopped, stems
stopped, a run of about 2 s, master peak from the stems and no underruns, and the process must
not grow. Xruns are listed, not failed on (see below). Needs PipeWire/JACK, like `just e2e-controls`.
"""
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "target/release/merlin"
PASS_SECS = 5.0
START = 5.0


def write_wav(path, rate, seconds, hz, amp, channels=2):
    with wave.open(str(path), "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(rate)
        frames = bytearray()
        for i in range(int(rate * seconds)):
            s = int(math.sin(2 * math.pi * hz * i / rate) * amp * 32767)
            frames += struct.pack("<" + "h" * channels, *([s] * channels))
        w.writeframes(bytes(frames))


def main():
    passes = int(sys.argv[1]) if len(sys.argv) > 1 else 100
    if not APP.exists():
        sys.exit(f"missing {APP}: cargo build --release -p merlin")
    work = Path(tempfile.mkdtemp(prefix="merlin-songsoak-"))
    write_wav(work / "a.wav", 44100, 8, 440, 0.3)
    write_wav(work / "b.wav", 48000, 8, 660, 0.2, channels=1)
    steps = [
        "1:load 1 TimGM6mb", "2:song add", "2.1:song tempo 1 240",
        "2.2:volume 1 1.0", "2.3:part add 1", "2.4:volume 1 0.4", "2.5:part add 1",
        f"2.6:track add 1 {work / 'a.wav'}", f"2.7:track add 1 {work / 'b.wav'}",
        "2.8:marker 1 2 1 part 2", "2.9:marker 1 3 1 stop",
        "3:countin 1", "3.1:click off",
    ]
    for p in range(passes):
        t = START + p * PASS_SECS
        steps += [
            f"{t:.1f}:go 1 1", f"{t + 0.3:.1f}:transport start",
            f"{t + 2.5:.1f}:peak", f"{t + 3.4:.1f}:report",
        ]
    steps.append(f"{START + passes * PASS_SECS + 1:.1f}:quit")
    env = dict(os.environ, MERLIN_SCRIPT="; ".join(steps))
    for var, d in (("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache"), ("XDG_CONFIG_HOME", "config")):
        env[var] = str(work / d)
    out = subprocess.run([str(APP), "--fresh"], capture_output=True, text=True, env=env).stdout
    shutil.rmtree(work, ignore_errors=True)

    rows = re.findall(
        r"xruns (\d+),.*RSS (\d+) MB.*position (Some\(\(\d+, \d+\)\)).*transport (\w+) bar.*run ([\d.]+)s, underruns (\d+), stems (\w+)", out
    )
    peaks = [float(m) for m in re.findall(r"master:([\d.]+)", out)]
    problems = []
    if len(rows) != passes:
        problems.append(f"{len(rows)} reports for {passes} passes")
    for i, (xr, rss, pos, tr, run, under, stems) in enumerate(rows):
        bad = []
        if pos != "Some((1, 2))":
            bad.append(f"position {pos}")
        if tr != "stopped" or stems != "false":
            bad.append(f"transport {tr}, stems {stems}")
        if not 1.6 < float(run) < 2.4:
            bad.append(f"run {run}s")
        if int(under):
            bad.append(f"underruns {under}: the decoder fell behind")
        if i < len(peaks) and not 0.35 < peaks[i] < 0.6:
            bad.append(f"peak {peaks[i]}")
        if bad:
            problems.append(f"pass {i + 1}: " + "; ".join(bad))
    if rows:
        first, last = int(rows[0][1]), int(rows[-1][1])
        if last > first + 30:
            problems.append(f"memory grew from {first} to {last} MB")
    # An xrun is a missed audio deadline, which this machine (no realtime scheduling) can have for
    # reasons that have nothing to do with the song: report where the counter rose, do not fail.
    xruns = [int(r[0]) for r in rows]
    risen = [i + 1 for i in range(len(xruns)) if xruns[i] > (xruns[i - 1] if i else 0)]
    if problems:
        print("FAIL\n  " + "\n  ".join(problems[:20]))
        print(out[-2500:])
        sys.exit(1)
    note = f"; xruns rose in pass {risen} (total {xruns[-1]})" if risen else "; no xruns"
    print(f"ok: {passes} passes, all stopped by their marker; memory {rows[0][1]} -> {rows[-1][1]} MB{note}")


if __name__ == "__main__":
    main()
