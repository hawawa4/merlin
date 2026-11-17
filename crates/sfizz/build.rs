//! Builds sfizz as a static library, with the `cc` crate only (no CMake).
//!
//! The source is sfizz's release tarball, which is not kept in the repository: the first build
//! downloads it (pinned by version and SHA-256) and unpacks it into `vendor/sfizz-VERSION/`
//! (gitignored), where later builds find it. Offline, download [`URL`] yourself and unpack it
//! there, or point `MERLIN_SFIZZ_SRC` at an unpacked copy. Merlin changes none of it.
//!
//! The source list and the flags follow sfizz's own `common.mk` (its Make build, used by the
//! DPF and VCV Rack ports), minus OpenMP (nothing we build uses it) and libsndfile (the bundled
//! readers handle WAV, FLAC, Ogg, AIFF and WavPack). Flags that are ours: `-include cstdint` (the
//! bundled Abseil predates GCC 13, whose headers stopped including it), `NDEBUG` (sfizz's asserts
//! would print or trap on the audio thread), `-O2` even in debug builds, hidden visibility.
//! Moving to a new sfizz version: change the three constants below, compare the new `common.mk`
//! with the lists, and run `cargo test -p merlin-sfizz -p merlin-hosting`.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const VERSION: &str = "1.2.3";
const URL: &str = "https://github.com/sfztools/sfizz/releases/download/1.2.3/sfizz-1.2.3.tar.gz";
const SHA256: &str = "a9339eac7620d7f0f6b44bdfe860680fab73e66efad4b5f15b21198dd9436822";

/// sfizz itself.
const SFIZZ: &[&str] = &[
    "ADSREnvelope.cpp",
    "AudioReader.cpp",
    "BeatClock.cpp",
    "Curve.cpp",
    "Defaults.cpp",
    "effects/Apan.cpp",
    "Effects.cpp",
    "modulations/ModId.cpp",
    "modulations/ModKey.cpp",
    "modulations/ModKeyHash.cpp",
    "modulations/ModMatrix.cpp",
    "modulations/sources/ADSREnvelope.cpp",
    "modulations/sources/ChannelAftertouch.cpp",
    "modulations/sources/PolyAftertouch.cpp",
    "modulations/sources/Controller.cpp",
    "modulations/sources/FlexEnvelope.cpp",
    "modulations/sources/LFO.cpp",
    "effects/Compressor.cpp",
    "effects/Disto.cpp",
    "effects/Eq.cpp",
    "effects/Filter.cpp",
    "effects/Fverb.cpp",
    "effects/Gain.cpp",
    "effects/Gate.cpp",
    "effects/impl/ResonantArray.cpp",
    "effects/impl/ResonantString.cpp",
    "effects/Limiter.cpp",
    "effects/Lofi.cpp",
    "effects/Nothing.cpp",
    "effects/Rectify.cpp",
    "effects/Strings.cpp",
    "effects/Width.cpp",
    "EQPool.cpp",
    "FileId.cpp",
    "FileMetadata.cpp",
    "FilePool.cpp",
    "FilterPool.cpp",
    "FlexEGDescription.cpp",
    "FlexEnvelope.cpp",
    "Interpolators.cpp",
    "Layer.cpp",
    "LFO.cpp",
    "LFODescription.cpp",
    "Messaging.cpp",
    "Metronome.cpp",
    "MidiState.cpp",
    "OpcodeCleanup.cpp",
    "Opcode.cpp",
    "Oversampler.cpp",
    "Panning.cpp",
    "parser/Parser.cpp",
    "parser/ParserPrivate.cpp",
    "PolyphonyGroup.cpp",
    "PowerFollower.cpp",
    "Region.cpp",
    "RegionSet.cpp",
    "RegionStateful.cpp",
    "Resources.cpp",
    "RTSemaphore.cpp",
    "ScopedFTZ.cpp",
    "sfizz.cpp",
    "sfizz_wrapper.cpp",
    "SfzFilter.cpp",
    "SIMDHelpers.cpp",
    "Smoothers.cpp",
    "Synth.cpp",
    "SynthMessaging.cpp",
    "Tuning.cpp",
    "utility/spin_mutex/SpinMutex.cpp",
    "Voice.cpp",
    "VoiceManager.cpp",
    "VoiceStealing.cpp",
    "Wavetables.cpp",
    "WindowedSinc.cpp",
];

/// sfizz files with SSE or AVX code paths, built with those instructions allowed (sfizz picks
/// one at run time from what the CPU has).
const SFIZZ_SSE: &[&str] = &[
    "effects/impl/ResonantArraySSE.cpp",
    "effects/impl/ResonantStringSSE.cpp",
    "simd/HelpersSSE.cpp",
];
const SFIZZ_AVX: &[&str] = &[
    "effects/impl/ResonantArrayAVX.cpp",
    "effects/impl/ResonantStringAVX.cpp",
    "simd/HelpersAVX.cpp",
];

/// The parts of Abseil sfizz uses (`absl::base`, `strings`, `flat_hash_map`, `synchronization`,
/// `time` and what those need).
const ABSEIL: &[&str] = &[
    "base/internal/cycleclock.cc",
    "base/internal/spinlock.cc",
    "base/internal/sysinfo.cc",
    "base/internal/thread_identity.cc",
    "base/internal/unscaledcycleclock.cc",
    "base/internal/low_level_alloc.cc",
    "base/internal/raw_logging.cc",
    "base/internal/spinlock_wait.cc",
    "base/internal/throw_delegate.cc",
    "strings/ascii.cc",
    "strings/charconv.cc",
    "strings/escaping.cc",
    "strings/internal/charconv_bigint.cc",
    "strings/internal/charconv_parse.cc",
    "strings/internal/memutil.cc",
    "strings/match.cc",
    "strings/numbers.cc",
    "strings/str_cat.cc",
    "strings/str_replace.cc",
    "strings/str_split.cc",
    "strings/string_view.cc",
    "strings/substitute.cc",
    "container/internal/hashtablez_sampler.cc",
    "container/internal/hashtablez_sampler_force_weak_definition.cc",
    "container/internal/raw_hash_set.cc",
    "synchronization/barrier.cc",
    "synchronization/blocking_counter.cc",
    "synchronization/internal/create_thread_identity.cc",
    "synchronization/internal/per_thread_sem.cc",
    "synchronization/internal/waiter.cc",
    "synchronization/notification.cc",
    "synchronization/mutex.cc",
    "synchronization/internal/graphcycles.cc",
    "time/civil_time.cc",
    "time/clock.cc",
    "time/duration.cc",
    "time/format.cc",
    "time/time.cc",
    "time/internal/cctz/src/time_zone_fixed.cc",
    "time/internal/cctz/src/time_zone_format.cc",
    "time/internal/cctz/src/time_zone_if.cc",
    "time/internal/cctz/src/time_zone_impl.cc",
    "time/internal/cctz/src/time_zone_info.cc",
    "time/internal/cctz/src/time_zone_libc.cc",
    "time/internal/cctz/src/time_zone_lookup.cc",
    "time/internal/cctz/src/time_zone_posix.cc",
    "time/internal/cctz/src/zone_info_source.cc",
    "debugging/stacktrace.cc",
    "debugging/symbolize.cc",
    "debugging/internal/demangle.cc",
    "debugging/internal/address_is_readable.cc",
    "debugging/internal/elf_mem_image.cc",
    "debugging/internal/vdso_support.cc",
    "hash/internal/hash.cc",
    "hash/internal/city.cc",
    "hash/internal/low_level_hash.cc",
    "numeric/int128.cc",
];

/// Other C++ the library carries in `src/external`.
const EXTERNAL_CPP: &[&str] = &[
    "src/external/spline/spline/spline.cpp",
    "src/external/cpuid/src/cpuid/cpuinfo.cpp",
    "src/external/cpuid/src/cpuid/version.cpp",
    "src/external/pugixml/src/pugixml.cpp",
    "src/external/tunings/src/Tunings.cpp",
];

/// C: the audio file readers, kissfft and two cephes functions.
const EXTERNAL_C: &[&str] = &[
    "external/st_audiofile/src/st_audiofile.c",
    "external/st_audiofile/src/st_audiofile_common.c",
    "external/st_audiofile/src/st_audiofile_libs.c",
    "external/st_audiofile/src/st_audiofile_sndfile.c",
    "external/st_audiofile/thirdparty/wavpack/src/common_utils.c",
    "external/st_audiofile/thirdparty/wavpack/src/decorr_utils.c",
    "external/st_audiofile/thirdparty/wavpack/src/entropy_utils.c",
    "external/st_audiofile/thirdparty/wavpack/src/extra1.c",
    "external/st_audiofile/thirdparty/wavpack/src/extra2.c",
    "external/st_audiofile/thirdparty/wavpack/src/open_utils.c",
    "external/st_audiofile/thirdparty/wavpack/src/open_filename.c",
    "external/st_audiofile/thirdparty/wavpack/src/open_legacy.c",
    "external/st_audiofile/thirdparty/wavpack/src/open_raw.c",
    "external/st_audiofile/thirdparty/wavpack/src/pack.c",
    "external/st_audiofile/thirdparty/wavpack/src/pack_dns.c",
    "external/st_audiofile/thirdparty/wavpack/src/pack_floats.c",
    "external/st_audiofile/thirdparty/wavpack/src/pack_utils.c",
    "external/st_audiofile/thirdparty/wavpack/src/read_words.c",
    "external/st_audiofile/thirdparty/wavpack/src/tags.c",
    "external/st_audiofile/thirdparty/wavpack/src/tag_utils.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack_floats.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack_seek.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack_utils.c",
    "external/st_audiofile/thirdparty/wavpack/src/write_words.c",
    "external/st_audiofile/thirdparty/wavpack/src/pack_dsd.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack_dsd.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack3.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack3_open.c",
    "external/st_audiofile/thirdparty/wavpack/src/unpack3_seek.c",
    "external/st_audiofile/thirdparty/libaiff/libaiff.all.c",
    "src/external/kiss_fft/kiss_fft.c",
    "src/external/kiss_fft/kiss_fftr.c",
    "external/cephes/src/chbevl.c",
    "external/cephes/src/i0.c",
];

/// Include folders for C and C++.
const INCLUDES: &[&str] = &[
    "src",
    "src/sfizz",
    "src/sfizz/utility/bit_array",
    "src/sfizz/utility/spin_mutex",
    "external/st_audiofile/src",
    "external/st_audiofile/thirdparty/dr_libs",
    "external/st_audiofile/thirdparty/stb_vorbis",
    "external/st_audiofile/thirdparty/wavpack/include",
    "external/st_audiofile/thirdparty/wavpack/src",
    "external/st_audiofile/thirdparty/libaiff",
    "external/abseil-cpp",
    "src/external/spline",
    "src/external/cpuid/src",
    "src/external/cpuid/platform/src",
    "external/simde",
    "src/external/pugixml/src",
    "src/external/kiss_fft",
];

/// Include folders for C++ only.
const INCLUDES_CPP: &[&str] = &[
    "src/external/hiir",
    "external/threadpool",
    "external/atomic_queue/include",
    "external/filesystem/include",
    "src/external/tunings/include",
    "external/jsl/include",
];

fn base(root: &Path, cpp: bool) -> cc::Build {
    let mut b = cc::Build::new();
    b.cpp(cpp)
        // Always optimised: the sampler runs on the audio thread, and an -O0 build of it would
        // not keep up even in a debug build of the app.
        .opt_level(2)
        .debug(false)
        .pic(true)
        .warnings(false)
        .define("NDEBUG", None)
        .define("ENABLE_DSD", None)
        .define("ENABLE_LEGACY", None)
        .flag("-pthread")
        // Keep sfizz's (and its Abseil's) symbols out of anything we export: plugins loaded
        // into the same process may carry their own Abseil.
        .flag("-fvisibility=hidden");
    for i in INCLUDES {
        b.include(root.join(i));
    }
    if cpp {
        b.std("c++17")
            .flag("-fvisibility-inlines-hidden")
            // The vendored Abseil predates GCC 13, whose headers stopped pulling in <cstdint>.
            .flag("-include")
            .flag("cstdint");
        for i in INCLUDES_CPP {
            b.include(root.join(i));
        }
    }
    b
}

/// Where the unpacked source is, downloading it first if it is not there yet.
fn source(workspace: &Path) -> PathBuf {
    println!("cargo:rerun-if-env-changed=MERLIN_SFIZZ_SRC");
    if let Some(dir) = std::env::var_os("MERLIN_SFIZZ_SRC") {
        return PathBuf::from(dir);
    }
    let root = workspace.join(format!("vendor/sfizz-{VERSION}"));
    if root.join("src/sfizz.h").is_file() {
        return root;
    }
    let fail = |what: String| -> ! {
        panic!(
            "{what}\nsfizz's source is needed to build Merlin. Download {URL} and unpack it as \
             {} (or set MERLIN_SFIZZ_SRC to an unpacked copy).",
            root.display()
        )
    };
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let tarball = out.join(format!("sfizz-{VERSION}.tar.gz"));
    let got = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(&tarball)
        .arg(URL)
        .status();
    if !got.is_ok_and(|s| s.success()) {
        fail(format!(
            "could not download {URL} (is curl installed, are we online?)"
        ));
    }
    let bytes = std::fs::read(&tarball).unwrap_or_else(|e| fail(format!("{e}")));
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if hash != SHA256 {
        fail(format!("{URL} has SHA-256 {hash}, expected {SHA256}"));
    }
    // Unpacked beside the target and renamed into place, so a build that stops halfway (or
    // another build doing the same at once) never leaves a half-unpacked folder behind.
    let tmp = workspace.join(format!("vendor/.sfizz-{VERSION}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap_or_else(|e| fail(format!("{e}")));
    let unpacked = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&tmp)
        .status();
    if !unpacked.is_ok_and(|s| s.success()) {
        fail(format!("could not unpack {}", tarball.display()));
    }
    let moved = std::fs::rename(tmp.join(format!("sfizz-{VERSION}")), &root);
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&tarball);
    if moved.is_err() && !root.join("src/sfizz.h").is_file() {
        fail(format!("could not move the source to {}", root.display()));
    }
    root
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest.join("../..");
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    println!("cargo:rerun-if-changed=build.rs");
    let root = source(&workspace);
    let x86 = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "x86_64" || a == "x86");

    let sfizz = |f: &&str| root.join("src/sfizz").join(f);
    let mut cpp = base(&root, true);
    cpp.files(SFIZZ.iter().map(sfizz))
        .files(
            ABSEIL
                .iter()
                .map(|f| root.join("external/abseil-cpp/absl").join(f)),
        )
        .files(EXTERNAL_CPP.iter().map(|f| root.join(f)));
    // Files that need flags of their own are built first and go into the same archive, so the
    // link never depends on the order of several.
    if x86 {
        let mut sse = base(&root, true);
        sse.flag("-msse").files(SFIZZ_SSE.iter().map(sfizz));
        cpp.objects(sse.compile_intermediates());
        let mut avx = base(&root, true);
        avx.flag("-mavx").files(SFIZZ_AVX.iter().map(sfizz));
        cpp.objects(avx.compile_intermediates());
    } else {
        // Without SSE/AVX the same files build their portable paths.
        cpp.files(SFIZZ_SSE.iter().chain(SFIZZ_AVX).map(sfizz));
    }
    let mut c = base(&root, false);
    c.files(EXTERNAL_C.iter().map(|f| root.join(f)));
    cpp.objects(c.compile_intermediates());
    cpp.compile("sfizz");
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=m");
}
