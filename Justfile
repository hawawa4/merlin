# Merlin task runner. `just` lists everything; `just dev` runs this round's test.

set shell := ["bash", "-euo", "pipefail", "-c"]

assets := "test-assets"
surge_clap := assets / "dl/surge-linux/Surge XT.clap"
sf3 := assets / "MuseScore_General.sf3"

# List recipes
default:
    @just --list --unsorted

# The app. Loads plugins from the standard CLAP folders plus test-assets (Linux and, through
# our patched Wine, Windows ones). Try: just dev --fresh      just dev --rig my.mrig
# This round: MIDI capture. The app always keeps the last 30 minutes you played (keyboards and the on-screen keys);
# play something, press "Capture MIDI" in the header and open ~/Music/Merlin/Merlin capture *.mid in a sequencer.
# Try it with `just fake-keyboard` in a second terminal, and map the action to a pad or a foot switch (MIDI map).
# See docs/qa-checklist.md 5n.
dev *args:
    cargo build -p merlin --release
    PATH="{{yabridge_dir}}:$PATH" WINELOADER={{patched_wine}} \
        WINEPREFIX="{{justfile_directory()}}/{{assets}}/wine/prefix-vblank" \
        target/release/merlin \
        --plugin-dir {{assets}}/dl/surge-linux --plugin-dir {{assets}}/sf {{args}}

# Packs the Wine + yabridge pair into dist/runtime/ (a 40 MB .tar.zst and runtime-manifest.json),
# the files "Download Windows plugin support" fetches. To publish them, put both in a GitHub
# release tagged `runtime` on the repository, e.g.
#   gh release create runtime dist/runtime/* --title "Windows plugin support"   (first time)
#   gh release upload runtime dist/runtime/* --clobber                           (later)
# To try the download without publishing: serve the folder (`python3 -m http.server -d dist/runtime`)
# and set runtime_url = "http://127.0.0.1:8000/runtime-manifest.json" in settings.toml.
# Plays a short song with two stems and markers over and over (default 100 passes, about 8 minutes): every pass
# must end stopped by its marker with no underruns or xruns. Needs PipeWire running.
soak-song passes="100":
    cargo build --release -p merlin
    python3 tools/soak_song.py {{passes}}

# Compatibility pass: loads every instrument and effect it finds, one app process each, plays, saves
# and reopens the rig, and writes docs/compat-matrix.md. `just compat` tries everything (about a
# minute per Windows plugin); `just compat Podolski Surge` only labels containing those words.
compat *filters:
    cargo build --release -p merlin
    PATH="{{yabridge_dir}}:$PATH" WINELOADER={{patched_wine}} \
        WINEPREFIX="{{justfile_directory()}}/{{assets}}/wine/prefix-vblank" \
        python3 tools/compat.py {{filters}} -- \
        --plugin-dir {{assets}}/dl/surge-linux --plugin-dir {{assets}}/sf

# Checks MIDI mappings end to end with a pretend controller (needs JACK/PipeWire running).
e2e-controls:
    cargo build --release -p merlin
    cargo build --release -p merlin-audio-io --examples
    python3 tools/e2e_controls.py

# Shows what a Launchkey MK3 sends, named control by control (and puts it in DAW mode meanwhile).
launchkey-probe seconds="60":
    python3 tools/launchkey_probe.py {{seconds}}

# A pretend MIDI keyboard (a JACK MIDI source, like a USB keyboard through PipeWire) for trying the
# MIDI settings without hardware. It plays a scale and unplugs itself after SECONDS.
fake-keyboard name="Fake Keyboard" seconds="60":
    cargo run -q -p merlin-audio-io --release --example fake_keyboard -- "{{name}}" {{seconds}}

package-runtime:
    tools/package-runtime.sh

# Soak run: random switching, layering, notes and editor tabs for SECS seconds with Surge XT,
# in throwaway state directories. Look at the `state:` lines (xruns, held keys, RSS, threads);
# the last one must show 0 held keys. Seed varies the run; add `--windows "Six Sines"` for Wine.
soak secs="180" seed="1" *args:
    cargo build -p merlin --release
    rm -rf /tmp/merlin-soak && mkdir -p /tmp/merlin-soak
    XDG_STATE_HOME=/tmp/merlin-soak/state XDG_CACHE_HOME=/tmp/merlin-soak/cache \
        XDG_CONFIG_HOME=/tmp/merlin-soak/config \
        PATH="{{yabridge_dir}}:$PATH" WINELOADER={{patched_wine}} \
        MERLIN_SCRIPT="$(python3 tools/soak.py {{secs}} {{seed}} {{args}})" \
        target/release/merlin --fresh \
        --plugin-dir {{assets}}/dl/surge-linux --plugin-dir {{assets}}/bridges-vblank/clap \
        2>&1 | grep -E "state:|error|crash|quarantin|panick" || true

# Download test plugins and SoundFonts into test-assets/ (skips what's already there)
setup:
    mkdir -p {{assets}}/dl/surge-linux
    [ -f "{{sf3}}" ] || curl -fL -o "{{sf3}}" \
        https://ftp.osuosl.org/pub/musescore/soundfont/MuseScore_General/MuseScore_General.sf3
    mkdir -p {{assets}}/sf && ln -sf ../MuseScore_General.sf3 {{assets}}/sf/MuseScore_General.sf3
    [ -d "{{surge_clap}}" ] || [ -f "{{surge_clap}}" ] || { \
        curl -fL -o {{assets}}/dl/surge-linux.tar.gz \
            https://github.com/surge-synthesizer/releases-xt/releases/download/1.3.4/surge-xt-linux-1.3.4-pluginsonly.tar.gz; \
        tar -xzf {{assets}}/dl/surge-linux.tar.gz -C {{assets}}/dl/surge-linux; }
    @echo "test assets ready in {{assets}}/"

# Does a bridge work the way another DAW would load it (symlinked chainloader, empty environment)?
check-standalone-bridge *args:
    cargo build --release -p merlin
    tools/check-standalone-bridge.sh {{args}}

# Build the vendored yabridge against our Wine in Docker; output in runtime/
yabridge-build wine="wine-11.20-vblank":
    tools/build-yabridge.sh {{wine}}

yabridge_dir := justfile_directory() / "runtime/yabridge-b580a9f7-wine-11.20-vblank"

# Build our patched Wine (Wine Staging + build-support/wine-patches/) in Docker; output in runtime/
wine-build version="11.20" tag="vblank":
    tools/build-wine.sh {{version}} {{tag}}

patched_wine := justfile_directory() / "runtime/wine-11.20-vblank/bin/wine"

# Format, lint and test: run before committing
check: fmt-check clippy test

# Build everything (release)
build:
    cargo build --workspace --release

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# Phase 1 tests that need Wine: Windows plugins through yabridge, on our patched Wine
test-wine:
    PATH="{{yabridge_dir}}:$PATH" WINELOADER={{patched_wine}} \
        cargo test -p merlin-audio-io --test clap windows -- --nocapture

# Is realtime scheduling available? (see docs/plan.md, Phase 0 result)
rt-status:
    @echo "ulimit -r (max rtprio): $(ulimit -r)"
    @id -nG | tr ' ' '\n' | grep -qx pipewire && echo "in 'pipewire' group: yes" || echo "in 'pipewire' group: NO  -> sudo usermod -aG pipewire $USER, then log in again"
    @busctl --system status org.freedesktop.RealtimeKit1 >/dev/null 2>&1 && echo "rtkit: running" || echo "rtkit: not available"
    @for pid in $(pidof pipewire); do ps -L -o cls=,comm= -p $pid | grep -q "^ *FF" && echo "pipewire $pid: realtime" || echo "pipewire $pid: NOT realtime"; done

# Phase 1: the engine on JACK with two zoned sine slots and an arpeggio (silent with `--quiet`)
jack-demo *args:
    cargo run -p merlin-audio-io --release --example jack_demo -- {{args}}

# Windows plugins for other plugin hosts (REAPER, Bitwig, Ardour...). Try: just bridge --help
bridge *args:
    cargo run --release -q -p merlin-bridge -- {{args}}
