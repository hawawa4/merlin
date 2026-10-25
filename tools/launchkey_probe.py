#!/usr/bin/env python3
"""Shows what a Launchkey MK3 sends, with the name of each control (from Novation's programmer's
reference), so the layout in the app can be checked against the real thing.

    just launchkey-probe            # 60 seconds; press and turn everything
    just launchkey-probe 300

Reads the device's two ports straight from ALSA (`amidi`), so it works with or without the app
running. With DAW mode off (the app not running) the buttons send nothing on the DAW port: the
probe puts the device in DAW mode first and leaves it again at the end.
"""
import re
import subprocess
import sys
import threading
import time

POT = {21 + i: f"knob {i + 1}" for i in range(8)}
FADER = {53 + i: f"fader {i + 1}" for i in range(9)}
FADER_BUTTON = {37 + i: f"fader button {i + 1}" for i in range(9)}
BUTTON = {102: "track > (next)", 103: "track < (previous)", 104: "scene up", 105: "scene down",
          74: "capture MIDI", 75: "quantise", 76: "click", 77: "undo",
          115: "play", 116: "stop", 117: "record", 118: "loop", 108: "shift?", 51: "up?", 52: "down?"}
CC = {**POT, **FADER, **FADER_BUTTON, **BUTTON}
MODES = {3: "pad mode", 9: "knob mode", 10: "fader mode"}
# Drum pad mode: sixteen notes on channel 10, rows of eight (top row, then bottom row).
DRUM_TOP = [40, 41, 42, 43, 48, 49, 50, 51]
DRUM_BOTTOM = [36, 37, 38, 39, 44, 45, 46, 47]


def ports():
    out = subprocess.run(["amidi", "-l"], capture_output=True, text=True).stdout
    found = {}
    for line in out.splitlines():
        m = re.match(r"\S+\s+(hw:\d+,\d+,\d+)\s+Launchkey MK3 \d+ LKMK3 (MIDI|DAW) Out", line)
        if m:
            found[m.group(2)] = m.group(1)
    return found


def describe(kind, data):
    status, d1, d2 = (data + [0, 0])[:3]
    ch = (status & 15) + 1
    t = status & 0xF0
    if t == 0xB0:
        name = CC.get(d1) or (MODES.get(d1) if ch == 16 else None) or f"CC {d1}"
        return f"ch{ch:<2} {name} = {d2}   (CC {d1})"
    if t in (0x90, 0x80):
        on = t == 0x90 and d2 > 0
        pad = ""
        if ch == 10 and d1 in DRUM_TOP:
            pad = f" = drum pad {DRUM_TOP.index(d1) + 1}, top row"
        elif ch == 10 and d1 in DRUM_BOTTOM:
            pad = f" = drum pad {DRUM_BOTTOM.index(d1) + 1}, bottom row"
        elif 96 <= d1 <= 103:
            pad = f" = pad {d1 - 95}, top row"
        elif 112 <= d1 <= 119:
            pad = f" = pad {d1 - 111}, bottom row"
        return f"ch{ch:<2} note {d1}{pad} {'on' if on else 'off'} ({d2})"
    if t == 0xE0:
        return f"ch{ch:<2} pitch bend {(d2 << 7 | d1) - 8192}"
    if t == 0xA0:
        return f"ch{ch:<2} aftertouch note {d1} = {d2}"
    return " ".join(f"{b:02X}" for b in data)


def listen(kind, port, until):
    p = subprocess.Popen(["amidi", "-p", port, "-d"], stdout=subprocess.PIPE, text=True, bufsize=1)
    threading.Timer(max(0, until - time.time()), p.terminate).start()
    for line in p.stdout:
        try:
            data = [int(x, 16) for x in line.split()]
        except ValueError:
            continue
        if data and data[0] < 0xF0:
            print(f"{kind:>4}  {describe(kind, data)}", flush=True)


def main():
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 60
    p = ports()
    if "DAW" not in p or "MIDI" not in p:
        sys.exit("no Launchkey MK3 found (amidi -l)")
    print(f"MIDI port {p['MIDI']}, DAW port {p['DAW']}; {seconds:.0f} s. Press and turn everything.")
    subprocess.run(["amidi", "-p", p["DAW"], "-S", "9F 0C 7F"])
    until = time.time() + seconds
    threads = [threading.Thread(target=listen, args=(k, v, until)) for k, v in p.items()]
    for t in threads:
        t.start()
    try:
        for t in threads:
            t.join()
    except KeyboardInterrupt:
        pass
    subprocess.run(["amidi", "-p", p["DAW"], "-S", "9F 0C 00"])


if __name__ == "__main__":
    main()
