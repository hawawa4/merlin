//! Opening a plugin file of any format. Shared by the app (in-process loading, SoundFonts) and
//! by the plugin host process.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::clap::ClapLibrary;
use crate::lv2::Lv2Library;
use crate::vst2::Vst2Library;
use crate::vst3::Vst3Library;
use crate::{PluginHandle, Waker, soundfont};

/// Loads the plugin `id` from `path` and creates an instance (main thread, not activated). The
/// format follows the file: `.vst3`, `.so` (VST2), `.lv2`, SoundFonts, anything else is CLAP.
/// LV2 needs the audio setup up front.
pub fn open_plugin(
    path: &Path,
    id: &str,
    waker: Arc<dyn Waker>,
    sample_rate: f64,
    max_block: u32,
) -> Result<Box<dyn PluginHandle>> {
    let ext = path.extension();
    Ok(if soundfont::is_source(path) {
        Box::new(soundfont::SoundFontPlugin::open(path)?)
    } else if ext.is_some_and(|e| e == "vst3") {
        Box::new(Vst3Library::load(path)?.create(id, waker)?)
    } else if ext.is_some_and(|e| e == "so") {
        Box::new(Vst2Library::load(path)?.create(id, waker)?)
    } else if ext.is_some_and(|e| e == "lv2") {
        Box::new(Lv2Library::open(path)?.create(id, waker, sample_rate, max_block)?)
    } else {
        Box::new(ClapLibrary::load(path)?.create(id, waker)?)
    })
}
