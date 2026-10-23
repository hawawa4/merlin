#!/usr/bin/env python3
"""Compatibility pass: loads every instrument and effect the app can find, one app process each,
and writes what happened to a matrix (docs/compat-matrix.md and dist/compat/results.json).

    just compat                      # everything
    just compat "Podolski" "Surge"   # only plugins whose label contains one of these

For each instrument the app (driven by MERLIN_SCRIPT) loads it, plays a chord, checks
that sound comes out, saves the rig, opens it again (which restores the plugin and its state),
and plays again. For each effect it loads it as an insert after a known instrument, plays, saves
and reopens. A plugin that crashes the app only fails its own row: every plugin runs in its own
process, in throwaway state folders. Run it through `just compat`, which sets up Wine the way
`just dev` does.
"""
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "target/release/merlin"
WORK = Path("/tmp/merlin-compat")
OUT = ROOT / "dist/compat"
DOC = ROOT / "docs/compat-matrix.md"

# Seconds to wait for a plugin to load, by kind (Wine hosts and big SoundFonts are slow).
LOAD_WAIT = {"native": 5, "windows": 14, "soundfont": 8}
RUN_TIMEOUT = {"native": 90, "windows": 180, "soundfont": 90}
# The instrument effects are tried behind: it exists everywhere the test assets do.
HOST_INSTRUMENT = "Surge XT"
# A plugin that does not pass is tried again, to tell a flaky one from a broken one.
MAX_ATTEMPTS = 3


def label(p):
    text = p["name"]
    if p["format"] == "vst3":
        text += " (VST3)"
    if p["format"] == "vst2":
        text += " (VST2)"
    if p["format"] == "lv2":
        text += " (LV2)"
    if p["format"] == "soundfont":
        text += " (SoundFont)"
    if p["windows"]:
        text += " (Windows)"
    return text


def kind(p):
    if p["format"] == "soundfont":
        return "soundfont"
    return "windows" if p["windows"] else "native"


def env():
    e = dict(os.environ)
    for d in ("data", "state", "cache", "config"):
        (WORK / d).mkdir(parents=True, exist_ok=True)
    e.update(
        XDG_DATA_HOME=str(WORK / "data"),
        XDG_STATE_HOME=str(WORK / "state"),
        XDG_CACHE_HOME=str(WORK / "cache"),
        XDG_CONFIG_HOME=str(WORK / "config"),
    )
    return e


def run_app(script, timeout, app_args):
    """Runs the app with a script; returns (exit code, output, seconds)."""
    e = env()
    e["MERLIN_SCRIPT"] = script
    start = time.time()
    proc = subprocess.Popen(
        [str(APP), "--fresh", *app_args],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env=e,
        start_new_session=True,
    )
    try:
        out, _ = proc.communicate(timeout=timeout)
        code = proc.returncode
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, _ = proc.communicate()
        code = "timeout"
    # Whatever Wine started goes too, and the next plugin gets a Wine with nothing left over from
    # this one (plugins keep lock and log files; a host still shutting down confused u-he's).
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    stop_wine()
    return code, out or "", time.time() - start


def stop_wine():
    """Ends everything running in the Wine prefix and waits for it to be gone."""
    wine = os.environ.get("WINELOADER")
    prefix = os.environ.get("WINEPREFIX")
    if not wine or not prefix:
        return
    server = Path(wine).parent / "wineserver"
    if server.exists():
        e = dict(os.environ, WINEPREFIX=prefix)
        subprocess.run([str(server), "-k"], env=e, capture_output=True, timeout=30)
        time.sleep(1.5)


KNOWN_NOTES = {
    # Not failures, just how these files are made.
    "FluidR3_GS (SoundFont)": "an add-on font of sound effects (rain, gunshot, ...): no piano, its default preset is a quiet noise",
}


def plugins_from_cache():
    cache = WORK / "cache/merlin/plugins.json"
    data = json.loads(cache.read_text())
    found = []
    for path, entry in data["entries"].items():
        for p in entry["plugins"]:
            found.append(
                dict(
                    name=p["name"],
                    id=p["id"],
                    format=p["format"],
                    vendor=p.get("vendor", ""),
                    version=p.get("version", ""),
                    features=p["features"],
                    windows=entry["windows"],
                    path=path,
                )
            )
    found.sort(key=lambda p: (label(p).lower()))
    return found


def quarantined():
    q = WORK / "cache/merlin/quarantine.json"
    if not q.exists():
        return []
    return json.loads(q.read_text()).get("entries", [])


def instrument_script(p, rig):
    w = LOAD_WAIT[kind(p)]
    name = label(p)
    chord = lambda t: [f"{t}:on 60", f"{t + 0.05}:on 64", f"{t + 0.1}:on 67"]
    off = lambda t: [f"{t}:off 60", f"{t + 0.05}:off 64", f"{t + 0.1}:off 67"]
    s = [f"1:load 1 {name}", f"{w}:select 1"]
    s += chord(w + 1)
    s += [f"{w + 4}:peak", f"{w + 4.1}:report"]
    s += off(w + 4.3)
    s += [f"{w + 5}:save {rig}", f"{w + 6}:new", f"{w + 7}:open {rig}"]
    t = w + 7 + w
    s += [f"{t}:select 1"]
    s += chord(t + 1)
    s += [f"{t + 4}:peak", f"{t + 4.1}:report", f"{t + 5}:quit"]
    return "; ".join(s)


def effect_script(p, rig):
    w = LOAD_WAIT[kind(p)]
    name = label(p)
    s = [f"1:load 1 {HOST_INSTRUMENT}", "4:select 1", f"5:insert 1 1 {name}"]
    t = 5 + w
    s += [f"{t}:tab insert1", f"{t + 2}:tab plugin"]
    t += 3
    s += [f"{t}:on 60", f"{t + 0.05}:on 64", f"{t + 0.1}:on 67"]
    s += [f"{t + 3}:peak", f"{t + 3.1}:report"]
    s += [f"{t + 3.5}:bypass 1 1", f"{t + 4.5}:on 72", f"{t + 6.5}:peak", f"{t + 6.6}:off 72"]
    s += [f"{t + 7}:bypass 1 1", f"{t + 8}:save {rig}", f"{t + 9}:new", f"{t + 10}:open {rig}"]
    t2 = t + 10 + w + 6
    s += [f"{t2}:select 1", f"{t2 + 1}:on 60", f"{t2 + 4}:peak", f"{t2 + 4.1}:report", f"{t2 + 5}:quit"]
    return "; ".join(s)


def parse(out):
    r = {}
    m = re.search(r"slot 1 <- .* in ([\d.]+)(ms|s|µs)", out)
    if m:
        v, unit = float(m.group(1)), m.group(2)
        r["load_s"] = v / 1000 if unit == "ms" else v / 1e6 if unit == "µs" else v
    m = re.search(r"editor 1 created in ([\d.]+)(ms|s|µs)", out)
    if m:
        v, unit = float(m.group(1)), m.group(2)
        r["editor_s"] = v / 1000 if unit == "ms" else v / 1e6 if unit == "µs" else v
    r["peaks"] = [
        max([float(x) for x in re.findall(r"1:([\d.]+)", line)] or [0.0])
        for line in re.findall(r"peaks since last check:(.*)", out)
    ]
    reports = re.findall(r"xruns (\d+), DSP ([\d.]+)%.*RSS (\d+) MB", out)
    if reports:
        r["xruns"] = max(int(x[0]) for x in reports)
        r["dsp"] = max(float(x[1]) for x in reports)
        r["rss"] = max(int(x[2]) for x in reports)
    r["errors"] = [
        l.strip()[:200]
        for l in out.splitlines()
        if l.startswith("error:") or " is missing" in l or "no editor" in l
    ]
    r["crashed"] = bool(re.search(r"crashed|quarantin|panicked|SIGSEGV|stack overflow", out))
    r["restored"] = "opened" in out
    return r


def judge(p, is_effect, code, out, r):
    """(status, notes): PASS, WARN, FAIL or SKIP."""
    notes = []
    errors = r["errors"]
    unsupported = [e for e in errors if "not supported" in e]
    if unsupported:
        return "SKIP", [unsupported[0].replace("error: ", "")]
    if code == "timeout":
        return "FAIL", ["did not finish in time (hang)"]
    if code != 0:
        notes.append(f"app exited with {code}")
    if r["crashed"]:
        return "FAIL", notes + ["the plugin crashed the app"]
    if "load_s" not in r and not is_effect:
        return "FAIL", notes + (errors[:1] or ["did not load"])
    if is_effect and not re.search(r"slot 1 insert 1 <-", out):
        return "FAIL", notes + (errors[:1] or ["did not load as an insert"])
    hard = [e for e in errors if "no editor" not in e]
    if hard:
        return "FAIL", notes + hard[:2]
    peaks = r["peaks"]
    status = "PASS"
    if code != 0:
        status = "FAIL"
    if not peaks or peaks[0] < 0.001:
        status = "WARN" if status == "PASS" else status
        notes.append("silent with the default sound" if not is_effect else "silent behind the instrument")
    if len(peaks) < 2 or (peaks[0] >= 0.001 and peaks[-1] < 0.001):
        status = "FAIL" if (peaks and peaks[0] >= 0.001) else status
        notes.append("silent after save and reopen" if peaks and peaks[0] >= 0.001 else "")
    if not r["restored"]:
        status = "FAIL"
        notes.append("the rig did not reopen")
    if any("no editor" in e for e in errors) and p["format"] != "soundfont":
        notes.append("no editor")
        status = "WARN" if status == "PASS" else status
    if r.get("load_s", 0) > 6:
        notes.append(f"slow load ({r['load_s']:.1f} s)")
        status = "WARN" if status == "PASS" else status
    if r.get("xruns", 0) > 1:
        notes.append(f"{r['xruns']} xruns")
        status = "WARN" if status == "PASS" else status
    return status, [n for n in notes if n]


def main():
    args = sys.argv[1:]
    app_args = []
    if "--" in args:
        i = args.index("--")
        args, app_args = args[:i], args[i + 1 :]
    filters = [a.lower() for a in args]
    if not APP.exists():
        sys.exit("build the app first: cargo build --release -p merlin")

    print("scanning (the first run takes a minute) ...", flush=True)
    run_app("60:quit", 120, app_args)
    plugins = plugins_from_cache()
    jobs = []
    for p in plugins:
        if filters and not any(f in label(p).lower() for f in filters):
            continue
        if "instrument" in p["features"]:
            jobs.append((p, False))
        elif "audio-effect" in p["features"]:
            jobs.append((p, True))
    print(f"{len(jobs)} plugins to try", flush=True)

    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "logs").mkdir(exist_ok=True)
    results = []
    for n, (p, is_effect) in enumerate(jobs, 1):
        name = label(p)
        slug = re.sub(r"[^A-Za-z0-9]+", "_", name).strip("_")
        rig = f"/tmp/merlin-compat/{slug}.mrig"
        script = effect_script(p, rig) if is_effect else instrument_script(p, rig)
        q = WORK / "cache/merlin/quarantine.json"
        attempts = []
        for attempt in range(MAX_ATTEMPTS):
            # Each attempt starts from a clean slate; the plugin cache is kept.
            shutil.rmtree(WORK / "state", ignore_errors=True)
            (WORK / "state").mkdir(parents=True)
            if q.exists():
                q.unlink()
            code, out, secs = run_app(script, RUN_TIMEOUT[kind(p)], app_args)
            (OUT / "logs" / f"{slug}{'' if attempt == 0 else f'.{attempt + 1}'}.log").write_text(out)
            r = parse(out)
            status, notes = judge(p, is_effect, code, out, r)
            attempts.append((status, notes, r, secs))
            if status in ("PASS", "SKIP"):
                break
        passed = sum(a[0] == "PASS" for a in attempts)
        # What to report: the first attempt, unless a retry changed the picture.
        status, notes, r, secs = attempts[0]
        if len(attempts) > 1:
            if passed:
                status = "WARN"
                notes = notes + [f"intermittent: passed {passed} of {len(attempts)} tries"]
            else:
                notes = notes + [f"failed all {len(attempts)} tries"]
        if name in KNOWN_NOTES:
            notes = notes + [KNOWN_NOTES[name]]
        row = dict(
            label=name,
            role="effect" if is_effect else "instrument",
            format=p["format"],
            windows=p["windows"],
            vendor=p["vendor"],
            version=p["version"],
            status=status,
            notes=notes,
            load_s=r.get("load_s"),
            editor_s=r.get("editor_s"),
            peak=(r["peaks"][0] if r["peaks"] else None),
            dsp=r.get("dsp"),
            rss=r.get("rss"),
            xruns=r.get("xruns"),
            seconds=round(secs, 1),
        )
        results.append(row)
        print(f"[{n}/{len(jobs)}] {status:4} {name}  {'; '.join(notes)}", flush=True)

    (OUT / "results.json").write_text(json.dumps(results, indent=2))
    write_doc(results)
    bad = [r for r in results if r["status"] == "FAIL"]
    print(f"\n{len(results)} tried: " + ", ".join(
        f"{sum(r['status'] == s for r in results)} {s}" for s in ("PASS", "WARN", "FAIL", "SKIP")))
    print(f"matrix: {DOC}")
    sys.exit(1 if bad else 0)


def wine_version():
    try:
        wine = os.environ.get("WINELOADER", "wine")
        return subprocess.run([wine, "--version"], capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:
        return "none"


def commit():
    try:
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True, cwd=ROOT).stdout.strip()
    except Exception:
        return "?"


def write_doc(results):
    def secs(v):
        return "" if v is None else (f"{v:.1f}" if v >= 1 else f"{v * 1000:.0f} ms").replace(" s", "")

    lines = [
        "# Compatibility matrix",
        "",
        "Generated by `just compat` (tools/compat.py). Every plugin the app finds is loaded in its own app "
        "process: it plays a chord, the rig is saved and reopened (restoring the plugin and its state), and "
        "it plays again. Effects are tried as inserts behind Surge XT. "
        f"App {commit()}, Wine {wine_version()}, {time.strftime('%Y-%m-%d')}.",
        "",
        "**PASS**: loads, sounds, survives save and reopen. **WARN**: works with a caveat (see notes). "
        "**FAIL**: does not work. **SKIP**: known unsupported (the app refuses it with a reason).",
        "",
        "| Plugin | Role | Format | Result | Load (s) | Editor (s) | Peak | DSP % | RSS MB | Notes |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for r in results:
        fmt = {"clap": "CLAP", "vst3": "VST3", "vst2": "VST2", "lv2": "LV2", "soundfont": "SoundFont"}.get(r["format"], r["format"])
        if r["windows"]:
            fmt += " (Windows)"
        lines.append(
            "| {} | {} | {} | **{}** | {} | {} | {} | {} | {} | {} |".format(
                r["label"].replace("|", "/"),
                r["role"],
                fmt,
                r["status"],
                secs(r["load_s"]),
                secs(r["editor_s"]),
                "" if r["peak"] is None else f"{r['peak']:.2f}",
                "" if r["dsp"] is None else f"{r['dsp']:.1f}",
                "" if r["rss"] is None else r["rss"],
                "; ".join(r["notes"]).replace("|", "/"),
            )
        )
    DOC.write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
