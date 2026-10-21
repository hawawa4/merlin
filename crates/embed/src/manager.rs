use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use merlin_hosting::{EditorSize, PluginHandle};
use merlin_model::MAX_SLOTS;

use crate::Dock;

/// Surplus editors are destroyed this long after a switch, never in the same turn as showing
/// another editor: destroying a bridged editor right when another appears makes Wine abort the
/// plugin host with `BadWindow` (found in S5).
pub const EVICT_DELAY: Duration = Duration::from_millis(300);

/// Gives the manager the plugin of a slot. The app owns the plugins; the manager only borrows
/// them for the duration of a call.
pub trait PluginLookup {
    fn plugin(&mut self, slot: usize) -> Option<&mut dyn PluginHandle>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorInfo {
    pub size: EditorSize,
    pub can_resize: bool,
    pub scroll: (u32, u32),
}

#[derive(Clone, Copy, Debug)]
pub struct ShowOutcome {
    /// The editor did not exist yet and was created.
    pub created: bool,
    pub elapsed: Duration,
}

struct Entry {
    created: bool,
    last_shown: Instant,
    size: EditorSize,
    can_resize: bool,
    scroll: (u32, u32),
}

impl Entry {
    fn new() -> Self {
        Self {
            created: false,
            last_shown: Instant::now(),
            size: EditorSize {
                width: 0,
                height: 0,
            },
            can_resize: false,
            scroll: (0, 0),
        }
    }
}

pub struct EditorManager {
    dock: Dock,
    entries: Vec<Entry>,
    /// Most editors kept alive at once.
    cache_cap: usize,
    evict_at: Option<Instant>,
    popup_open: bool,
    scale: f64,
}

impl EditorManager {
    /// `parent` is the main window's XID; `scale` the display scale handed to plugins.
    pub fn new(parent: u32, scale: f64, cache_cap: usize) -> Result<Self> {
        Ok(Self {
            dock: Dock::new(parent)?,
            entries: (0..MAX_SLOTS).map(|_| Entry::new()).collect(),
            cache_cap: cache_cap.max(1),
            evict_at: None,
            popup_open: false,
            scale,
        })
    }

    pub fn dock(&self) -> &Dock {
        &self.dock
    }

    pub fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
    }

    /// Places the dock (physical pixels, relative to the main window).
    pub fn set_rect(&mut self, x: i32, y: i32, w: u32, h: u32) -> Result<()> {
        self.dock.set_rect(x, y, w, h)
    }

    /// The overlap rule: while the UI has a popup open, the dock is unmapped so the popup is
    /// drawn over plain UI instead of under a native window.
    pub fn set_popup_open(&mut self, open: bool) -> Result<()> {
        self.popup_open = open;
        self.refresh_visibility()
    }

    fn refresh_visibility(&mut self) -> Result<()> {
        let visible = self.dock.shown().is_some() && !self.popup_open;
        self.dock.set_visible(visible)
    }

    pub fn shown(&self) -> Option<usize> {
        self.dock.shown()
    }

    pub fn info(&self, slot: usize) -> Option<EditorInfo> {
        let e = &self.entries[slot];
        e.created.then_some(EditorInfo {
            size: e.size,
            can_resize: e.can_resize,
            scroll: e.scroll,
        })
    }

    pub fn has_editor(&self, slot: usize) -> bool {
        self.entries[slot].created
    }

    pub fn live_editors(&self) -> usize {
        self.entries.iter().filter(|e| e.created).count()
    }

    /// Shows this slot's editor in the dock, creating it on first use, and hides the previous
    /// one. Surplus editors are destroyed later, from [`tick`](Self::tick).
    pub fn show(&mut self, slot: usize, plugins: &mut dyn PluginLookup) -> Result<ShowOutcome> {
        let t = Instant::now();
        if let Some(prev) = self.dock.shown().filter(|&p| p != slot)
            && let Some(editor) = plugins.plugin(prev).and_then(|p| p.editor())
        {
            editor.hide();
        }
        let created = !self.entries[slot].created;
        if created {
            let canvas = self.dock.canvas_window(slot)?;
            let editor = plugins
                .plugin(slot)
                .and_then(|p| p.editor())
                .context("this plugin has no editor")?;
            let size = editor.attach(canvas, self.scale)?;
            let e = &mut self.entries[slot];
            e.created = true;
            e.size = size;
            e.can_resize = editor.can_resize();
            e.scroll = (0, 0);
            // Size the canvas *after* attaching: the resulting ConfigureNotify is how a bridged
            // editor learns where it is on screen.
            self.dock.set_canvas_size(slot, size.width, size.height)?;
            self.dock.announce(slot)?;
        }
        if let Some(editor) = plugins.plugin(slot).and_then(|p| p.editor()) {
            editor.show();
        }
        let e = &mut self.entries[slot];
        e.last_shown = Instant::now();
        let (size, scroll) = (e.size, e.scroll);
        self.dock.set_canvas_size(slot, size.width, size.height)?;
        self.dock.scroll_to(slot, scroll.0, scroll.1)?;
        self.dock.show(Some(slot))?;
        self.refresh_visibility()?;
        self.evict_at = Some(Instant::now() + EVICT_DELAY);
        Ok(ShowOutcome {
            created,
            elapsed: t.elapsed(),
        })
    }

    /// Shows nothing (editor hidden, not destroyed).
    pub fn hide(&mut self, plugins: &mut dyn PluginLookup) -> Result<()> {
        if let Some(slot) = self.dock.shown()
            && let Some(editor) = plugins.plugin(slot).and_then(|p| p.editor())
        {
            editor.hide();
        }
        self.dock.show(None)?;
        self.refresh_visibility()
    }

    /// Destroys a slot's editor (before its plugin is removed, or to free it). The canvas stays
    /// for the slot's next editor.
    pub fn forget(&mut self, slot: usize, plugins: &mut dyn PluginLookup) -> Result<()> {
        if self.entries[slot].created {
            if let Some(editor) = plugins.plugin(slot).and_then(|p| p.editor()) {
                editor.detach();
            }
            self.entries[slot].created = false;
        }
        if self.dock.shown() == Some(slot) {
            self.dock.show(None)?;
            self.refresh_visibility()?;
        }
        Ok(())
    }

    /// Resizes the shown editor to the dock, if the plugin allows; a fitted editor needs no
    /// scrolling.
    pub fn fit(&mut self, plugins: &mut dyn PluginLookup) -> Result<()> {
        let Some(slot) = self.dock.shown() else {
            return Ok(());
        };
        let (w, h) = self.dock.size();
        let Some(editor) = plugins.plugin(slot).and_then(|p| p.editor()) else {
            return Ok(());
        };
        if !editor.can_resize() {
            return Ok(());
        }
        let size = editor.adjust_size(EditorSize {
            width: w,
            height: h,
        });
        if editor.set_size(size) {
            self.entries[slot].size = size;
            self.dock.set_canvas_size(slot, size.width, size.height)?;
        }
        self.entries[slot].scroll = (0, 0);
        self.dock.scroll_to(slot, 0, 0)
    }

    /// Scrolls the shown editor (for editors larger than the dock).
    pub fn scroll(&mut self, x: u32, y: u32) -> Result<()> {
        if let Some(slot) = self.dock.shown() {
            self.entries[slot].scroll = (x, y);
            self.dock.scroll_to(slot, x, y)?;
        }
        Ok(())
    }

    /// The plugin asked for a different editor size: accept it without calling `set_size` (an
    /// unadjusted size makes JUCE log HOST_MISBEHAVING). The dock clips; scrolling covers the
    /// rest.
    pub fn plugin_resized(
        &mut self,
        slot: usize,
        size: EditorSize,
        plugins: &mut dyn PluginLookup,
    ) -> Result<()> {
        if !self.entries[slot].created {
            return Ok(());
        }
        if let Some(editor) = plugins.plugin(slot).and_then(|p| p.editor()) {
            editor.note_size(size);
        }
        self.entries[slot].size = size;
        self.dock.set_canvas_size(slot, size.width, size.height)
    }

    /// Housekeeping: destroys surplus editors once their delay has passed. Returns when to
    /// call again, if anything is pending.
    pub fn tick(&mut self, now: Instant, plugins: &mut dyn PluginLookup) -> Option<Instant> {
        let due = self.evict_at?;
        if now < due {
            return Some(due);
        }
        self.evict_at = None;
        let keep = self.dock.shown();
        while self.live_editors() > self.cache_cap {
            let victim = (0..MAX_SLOTS)
                .filter(|&s| self.entries[s].created && Some(s) != keep)
                .min_by_key(|&s| self.entries[s].last_shown);
            let Some(victim) = victim else { break };
            let _ = self.forget(victim, plugins);
        }
        None
    }

    /// Destroys every editor. Call before the plugins are dropped.
    pub fn shutdown(&mut self, plugins: &mut dyn PluginLookup) {
        for slot in 0..MAX_SLOTS {
            let _ = self.forget(slot, plugins);
        }
    }

    /// Size of the plugin's own window as X11 sees it.
    pub fn plugin_window_size(&self, slot: usize) -> Option<(u32, u32)> {
        self.dock.plugin_window_size(slot)
    }

    pub fn dock_size(&self) -> (u32, u32) {
        self.dock.size()
    }
}
