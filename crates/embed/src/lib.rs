//! Embedding plugin editors into our main window (plan §4.6).
//!
//! The app owns the main X11 window and tells the [`EditorManager`] where the dock goes. The
//! manager keeps one canvas window per slot, creates editors on first use, switches between
//! them by mapping and unmapping canvases, destroys surplus editors (least recently used
//! first), and enforces the rule that native child windows draw above our UI: while the UI has
//! a popup open the dock is unmapped.

mod dock;
mod manager;

pub use dock::{Dock, MAX_EDITORS};
pub use manager::{EVICT_DELAY, EditorInfo, EditorManager, PluginLookup, ShowOutcome};
