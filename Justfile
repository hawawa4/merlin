# Merlin task runner. `just` lists everything; `just dev` runs this round's test.

set shell := ["bash", "-euo", "pipefail", "-c"]

assets := "test-assets"
surge_clap := assets / "dl/surge-linux/Surge XT.clap"
sf3 := assets / "MuseScore_General.sf3"
sf2 := "/usr/share/sounds/sf2/FluidR3_GM.sf2"
rtsan_lib := "/usr/lib/llvm-21/lib/clang/21/lib/linux/libclang_rt.rtsan-x86_64.a"

# List recipes
default:
    @just --list --unsorted

# This round's test: the S5 rack, Linux and Windows plugins side by side. Try:
#   just dev                      default rack      just dev "Surge XT; Six Sines (Windows)"
dev preload="Surge XT; Surge XT (Windows); Six Sines (Windows)": (s5 preload)

# S3: Surge XT as a VST3 in our own VST3 host, editor embedded. Try:
#   just s3-dev                   Surge XT      just s3-dev "Surge XT Effects"
s3-dev name="Surge XT": (s3 (assets / "dl/surge-linux" / name + ".vst3"))

# S4 with DXVK (fixes frozen JUCE editors under Wine, but blanks OB-Xf). Try:
#   just s4-dxvk                  Surge XT      just s4-dxvk "Six Sines"
s4-dxvk name="Surge XT": (s4 (assets / "bridges-dxvk/clap" / name + ".clap"))

# Download test plugins and SoundFonts into test-assets/ (skips what's already there)
setup:
    mkdir -p {{assets}}/dl/surge-linux
    [ -f "{{sf3}}" ] || curl -fL -o "{{sf3}}" \
        https://ftp.osuosl.org/pub/musescore/soundfont/MuseScore_General/MuseScore_General.sf3
    [ -d "{{surge_clap}}" ] || [ -f "{{surge_clap}}" ] || { \
        curl -fL -o {{assets}}/dl/surge-linux.tar.gz \
            https://github.com/surge-synthesizer/releases-xt/releases/download/1.3.4/surge-xt-linux-1.3.4-pluginsonly.tar.gz; \
        tar -xzf {{assets}}/dl/surge-linux.tar.gz -C {{assets}}/dl/surge-linux; }
    @echo "test assets ready in {{assets}}/"

# Build yabridge at the pinned commit in Docker (S4); output in runtime/
yabridge-build commit="b580a9f7":
    tools/build-yabridge.sh {{commit}}

yabridge_dir := justfile_directory() / "runtime/yabridge-b580a9f7"
wine_loader := "/opt/wine-staging/bin/wine"
wine_prefix := justfile_directory() / assets / "wine/prefix"
bridges := assets / "bridges"

# S4 setup: Wine prefix, Surge XT (Windows) inside it, and a yabridge CLAP bridge for it
s4-setup: yabridge-build
    #!/usr/bin/env bash
    set -euo pipefail
    export WINEPREFIX="{{wine_prefix}}" WINEDEBUG=-all
    [ -d "$WINEPREFIX/drive_c" ] || { echo "creating Wine prefix…"; {{wine_loader}}boot -u >/dev/null 2>&1; }
    if [ ! -d "{{assets}}/dl/surge-win" ]; then
        curl -fL -o {{assets}}/dl/surge-win.zip             https://github.com/surge-synthesizer/releases-xt/releases/download/1.3.4/surge-xt-win64-1.3.4-pluginsonly.zip
        mkdir -p {{assets}}/dl/surge-win && (cd {{assets}}/dl/surge-win && unzip -oq ../surge-win.zip)
    fi
    clapdir="$WINEPREFIX/drive_c/Program Files/Common Files/CLAP"
    mkdir -p "$clapdir" {{bridges}}/clap
    cp -f "{{assets}}/dl/surge-win/Surge XT.clap" "$clapdir/"
    # The bridge: yabridge's chainloader named like the plugin, plus a .clap-win link to the
    # Windows plugin. yabridge finds the prefix by walking up from the Windows file.
    cp -f "{{yabridge_dir}}/libyabridge-chainloader-clap.so" "{{bridges}}/clap/Surge XT.clap"
    ln -sf "$clapdir/Surge XT.clap" "{{bridges}}/clap/Surge XT.clap-win"
    ls -la {{bridges}}/clap

# S4: Windows Surge XT (CLAP) through yabridge + Wine 11, editor embedded like S2
s4 plugin=(bridges / "clap/Surge XT.clap"):
    PATH="{{yabridge_dir}}:$PATH" WINELOADER={{wine_loader}} \
        cargo run -p s2-clap-embed --release -- "{{plugin}}"

# S4 compatibility matrix: every bridged plugin, load/embed time and live-redraw check
# (plain system Wine by default; `just s4-matrix-vblank` for our patched build)
s4-matrix *bridges:
    tools/s4-matrix.sh {{bridges}}

# Build our patched Wine (Wine Staging + build-support/wine-patches/) in Docker; output in runtime/
wine-build version="11.20" tag="vblank":
    tools/build-wine.sh {{version}} {{tag}}

patched_wine := justfile_directory() / "runtime/wine-11.20-vblank/bin/wine"

# S4 with our patched Wine (WaitForVBlank implemented), on its own prefix. Try:
#   just s4-vblank                Surge XT      just s4-vblank "Six Sines"
s4-vblank name="Surge XT":
    PATH="{{yabridge_dir}}:$PATH" WINELOADER={{patched_wine}} \
        cargo run -p s2-clap-embed --release -- "{{assets}}/bridges-vblank/clap/{{name}}.clap"

# S4 compatibility matrix on our patched Wine
s4-matrix-vblank:
    WINELOADER={{patched_wine}} tools/s4-matrix.sh {{justfile_directory()}}/{{assets}}/bridges-vblank/clap/*.clap

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

# Is realtime scheduling available? (S1 finding: it isn't on this machine yet)
rt-status:
    @echo "ulimit -r (max rtprio): $(ulimit -r)"
    @id -nG | tr ' ' '\n' | grep -qx pipewire && echo "in 'pipewire' group: yes" || echo "in 'pipewire' group: NO  -> sudo usermod -aG pipewire $USER, then log in again"
    @busctl --system status org.freedesktop.RealtimeKit1 >/dev/null 2>&1 && echo "rtkit: running" || echo "rtkit: not available"
    @for pid in $(pidof pipewire); do ps -L -o cls=,comm= -p $pid | grep -q "^ *FF" && echo "pipewire $pid: realtime" || echo "pipewire $pid: NOT realtime"; done

# Phase 1: the engine on JACK with two zoned sine slots and an arpeggio (silent with `--quiet`)
jack-demo *args:
    cargo run -p merlin-audio-io --release --example jack_demo -- {{args}}

# S1: JACK sine synth from your MIDI keyboard (frames: 64 or 128)
s1 frames="128" seconds="30":
    cargo run -p s1-audio --release -- --frames {{frames}} --seconds {{seconds}}

# S1 without a keyboard: plays an arpeggio
s1-demo frames="128":
    cargo run -p s1-audio --release -- --demo --frames {{frames}} --seconds 5

# S2: Surge XT CLAP with its editor embedded in our window
s2 plugin=surge_clap:
    cargo run -p s2-clap-embed --release -- "{{plugin}}"

# S2 scripted run: save, scroll, restore, fit, close/reopen, quit
s2-script plugin=surge_clap:
    S2_SCRIPT="3:save; 5:scroll 0 200; 7:load; 9:fit; 11:toggle; 13:toggle; 15:on 60; 16:off 60; 18:quit" \
        cargo run -p s2-clap-embed --release -- "{{plugin}}"

# S3: Surge XT as a VST3 in our minimal Rust VST3 host, editor embedded like S2
s3 plugin=(assets / "dl/surge-linux/Surge XT.vst3"):
    cargo run -p s3-vst3-embed --release -- "{{plugin}}"

# S3 scripted run: sound, parameter edit, save, change, restore, close/reopen, fit
s3-script plugin=(assets / "dl/surge-linux/Surge XT.vst3"):
    S3_SCRIPT="2:peak; 3:on 60; 4:peak; 5:off 60; 6:param 237 0.5; 7:save; 8:param 237 0.2; 9:load; 11:toggle; 12:toggle; 13:fit; 15:quit" \
        cargo run -p s3-vst3-embed --release -- "{{plugin}}"

# S5: rack of 8 CLAP slots (Linux and bridged Windows plugins), one editor dock, keyboard map.
# `preload` is a ";"-separated list of plugin names for slots 1.. (see the list printed at start)
s5 preload="Surge XT; Surge XT (Windows); Six Sines (Windows)":
    PATH="{{yabridge_dir}}:$PATH" WINELOADER={{patched_wine}} S5_PRELOAD="{{preload}}" \
        cargo run -p s5-rack --release

# S5 scripted run: load, switch editors, play, measure CPU (about 25 s)
s5-script:
    PATH="{{yabridge_dir}}:$PATH" WINELOADER={{patched_wine}} \
    S5_PRELOAD="Surge XT; Surge XT (Windows); Six Sines (Windows)" \
    S5_SCRIPT="8:select 1; 9:on 60; 10:peak; 10.5:off 60; 11:select 2; 12:on 60; 13:peak; 13.5:off 60; 14:select 3; 15:on 60; 16:peak; 16.5:off 60; 17:cpu; 27:cpu; 28:report; 29:quit" \
        cargo run -p s5-rack --release

# S6: render the SoundFont test pattern with all engines to spikes/s6-soundfont/out/
s6 *files=(sf2 + " " + sf3):
    cargo run -p s6-soundfont --release -- {{files}}

# S6 under RealtimeSanitizer: reports any lock/allocation in audio-thread calls
s6-rtsan *files=(sf2 + " " + sf3):
    RTSAN_ENABLE=1 RTSAN_LIB_PATH={{rtsan_lib}} CARGO_TARGET_DIR=target/rtsan \
        cargo build -p s6-soundfont --release
    RTSAN_OPTIONS=halt_on_error=false:print_stats_on_exit=true \
        ./target/rtsan/release/s6-soundfont {{files}} 2>&1 | grep -E "^(===|fluid|rusty|oxi)|error count"

# S6: play each file's renders, loudness-matched: FluidSynth (reference), oxisynth, rustysynth-ext
listen:
    @for f in spikes/s6-soundfont/out/fluidsynth-*.wav; do \
        for engine in fluidsynth oxisynth rustysynth-ext; do \
            w="${f/fluidsynth-/$engine-}"; \
            [ -f "$w" ] && { echo "▶ $(basename "$w")"; pw-play "$w"; }; \
        done; \
    done
