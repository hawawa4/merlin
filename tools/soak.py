#!/usr/bin/env python3
"""Prints a MERLIN_SCRIPT for a soak run: random switching, layering, notes, mutes,
zones, panic, editor tabs and insert bypass for N seconds, with a `report` every 30 s and a clean
release of every key at the end (so the last report must show no held keys).

    python3 tools/soak.py SECONDS [SEED] [--windows "Six Sines"]
"""
import random
import sys

secs = float(sys.argv[1]) if len(sys.argv) > 1 else 120
seed = int(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[2].isdigit() else 1
windows = sys.argv[sys.argv.index("--windows") + 1] if "--windows" in sys.argv else None
rnd = random.Random(seed)

ev = []  # (time, command)
slots = [1, 2, 3]
ev += [(1.0, "load 1 Surge XT"), (3.0, "load 2 Surge XT"), (5.0, "load 3 Surge XT")]
ev += [(7.0, "insert 1 1 Surge XT Effects")]
if windows:
    ev += [(9.0, f"load 4 {windows}")]
    slots.append(4)
ev += [(12.0, "select 1")]
t = 15.0
held = {}  # key -> release time
while t < secs - 8:
    r = rnd.random()
    if r < 0.30:
        ev.append((t, f"select {rnd.choice(slots)}"))
    elif r < 0.38:
        ev.append((t, f"layer {rnd.choice(slots)}"))
    elif r < 0.78:
        k = rnd.randint(36, 96)
        if k not in held:
            ev.append((t, f"on {k}"))
            held[k] = t + rnd.choice([0.05, 0.2, 0.8, 2.5, 6.0])
    elif r < 0.82:
        ev.append((t, "panic"))
        held.clear()
    elif r < 0.88:
        ev.append((t, f"mute {rnd.choice(slots)}"))
    elif r < 0.92:
        lo = rnd.randint(0, 60)
        ev.append((t, f"zone {rnd.choice(slots)} {lo} {rnd.randint(lo, 127)}"))
    elif r < 0.95:
        ev.append((t, f"transpose {rnd.choice(slots)} {rnd.randint(-12, 12)}"))
    elif r < 0.98:
        ev.append((t, "bypass 1 1"))
    else:
        ev.append((t, "tab " + rnd.choice(["fx", "plugin", "insert1"])))
    t += rnd.uniform(0.05, 0.6)
    for k, until in list(held.items()):
        if until <= t:
            ev.append((until, f"off {k}"))
            del held[k]
# Finish cleanly: release everything, open every zone, then check for stuck notes.
end = secs - 8
for k in held:
    ev.append((end, f"off {k}"))
for s in slots:
    ev.append((end + 0.5, f"zone {s} 0 127"))
ev.append((end + 4, "select 1"))
for n in range(30, int(end), 30):
    ev.append((n, "report"))
ev.append((secs - 2, "report"))
ev.append((secs, "quit"))
ev.sort(key=lambda e: e[0])
print("; ".join(f"{t:.2f}:{c}" for t, c in ev))
