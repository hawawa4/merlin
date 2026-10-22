//! The X11 windows behind the editor dock.
//!
//! - `clip`: a child of the main window, placed over the area our UI leaves free; clips editors
//!   at its edges.
//! - one `canvas` per slot, children of `clip`, sized like that slot's editor. The plugin
//!   embeds into its canvas; scrolling moves the canvas to a negative offset inside `clip`.
//!   Only the selected slot's canvas is mapped.
//!
//! Why canvases (S2/S4): bridged (Wine/yabridge) editors only learn their screen position from
//! `ConfigureNotify` events on the window they were given and on our top-level. Moving or
//! resizing the canvas itself generates a real `ConfigureNotify` that yabridge translates for
//! Wine. A canvas is kept for the slot's whole life: destroying the parent of an editor that
//! tears down asynchronously is a race (S5).

use anyhow::{Result, anyhow};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, MapState, PropMode,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// How many editors a dock can hold: instruments and their insert effects, each with a canvas of
/// its own. Editor ids are chosen by the app.
pub const MAX_EDITORS: usize = 64;

#[derive(Clone, Copy)]
struct Canvas {
    window: u32,
    size: (u32, u32),
}

pub struct Dock {
    conn: RustConnection,
    clip: u32,
    canvases: [Option<Canvas>; MAX_EDITORS],
    rect: (i32, i32, u32, u32),
    /// The slot whose canvas is mapped.
    shown: Option<usize>,
    /// Whether the clip window is mapped.
    mapped: bool,
}

impl Dock {
    /// Creates the dock inside the X11 window `parent` (our main window).
    pub fn new(parent: u32) -> Result<Self> {
        let (conn, _) = RustConnection::connect(None)?;
        // yabridge picks the topmost ancestor with WM_WINDOW_ROLE as the "host window" whose
        // moves it forwards to Wine; toolkits like winit don't set a role.
        let role = conn.intern_atom(false, b"WM_WINDOW_ROLE")?.reply()?.atom;
        conn.change_property8(
            PropMode::REPLACE,
            parent,
            role,
            AtomEnum::STRING,
            b"merlin-main",
        )?;
        let clip = conn.generate_id()?;
        Self::create(&conn, clip, parent)?;
        conn.flush()?;
        Ok(Self {
            conn,
            clip,
            canvases: [None; MAX_EDITORS],
            rect: (0, 0, 1, 1),
            shown: None,
            mapped: false,
        })
    }

    fn create(conn: &RustConnection, window: u32, parent: u32) -> Result<()> {
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            parent,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(0x151515)
                .event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        )?;
        Ok(())
    }

    /// The window to hand to the plugin (`set_parent`) for this slot; created on first use.
    pub fn canvas_window(&mut self, slot: usize) -> Result<u32> {
        if let Some(c) = self.canvases[slot] {
            return Ok(c.window);
        }
        let window = self.conn.generate_id()?;
        Self::create(&self.conn, window, self.clip)?;
        self.conn.flush()?;
        self.canvases[slot] = Some(Canvas {
            window,
            size: (1, 1),
        });
        Ok(window)
    }

    fn canvas(&self, slot: usize) -> Result<Canvas> {
        self.canvases[slot].ok_or_else(|| anyhow!("slot {slot} has no canvas"))
    }

    /// Place the dock (physical pixels, relative to the main window).
    pub fn set_rect(&mut self, x: i32, y: i32, w: u32, h: u32) -> Result<()> {
        let rect = (x, y, w.max(1), h.max(1));
        if rect != self.rect {
            let moved = (rect.0, rect.1) != (self.rect.0, self.rect.1);
            self.rect = rect;
            self.conn.configure_window(
                self.clip,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(rect.2)
                    .height(rect.3),
            )?;
            if moved && let Some(slot) = self.shown {
                // The canvas moved on screen without being configured itself.
                self.announce(slot)?;
            }
            self.conn.flush()?;
        }
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.rect.2, self.rect.3)
    }

    pub fn shown(&self) -> Option<usize> {
        self.shown
    }

    /// Map this slot's canvas and unmap all others (`None` shows nothing).
    pub fn show(&mut self, slot: Option<usize>) -> Result<()> {
        for (i, c) in self.canvases.iter().enumerate() {
            if let Some(c) = c {
                if Some(i) == slot {
                    self.conn.map_window(c.window)?;
                } else {
                    self.conn.unmap_window(c.window)?;
                }
            }
        }
        self.shown = slot.filter(|&s| self.canvases[s].is_some());
        self.conn.flush()?;
        if let Some(s) = self.shown {
            self.announce(s)?;
        }
        Ok(())
    }

    /// Size the canvas to the editor.
    pub fn set_canvas_size(&mut self, slot: usize, w: u32, h: u32) -> Result<()> {
        let mut c = self.canvas(slot)?;
        c.size = (w.max(1), h.max(1));
        self.canvases[slot] = Some(c);
        self.conn.configure_window(
            c.window,
            &ConfigureWindowAux::new().width(c.size.0).height(c.size.1),
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Make the canvas emit a real `ConfigureNotify` so embedded (bridged) editors recompute
    /// their screen position: resize by one pixel and back.
    pub fn announce(&self, slot: usize) -> Result<()> {
        let c = self.canvas(slot)?;
        let (w, h) = c.size;
        for height in [h + 1, h] {
            self.conn
                .configure_window(c.window, &ConfigureWindowAux::new().width(w).height(height))?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Map or unmap the whole dock (used while a UI popup would be covered by it).
    pub fn set_visible(&mut self, visible: bool) -> Result<()> {
        if visible != self.mapped {
            self.mapped = visible;
            if visible {
                self.conn.map_window(self.clip)?;
            } else {
                self.conn.unmap_window(self.clip)?;
            }
            self.conn.flush()?;
        }
        Ok(())
    }

    pub fn is_visible(&self) -> bool {
        self.mapped
    }

    pub fn scroll_to(&mut self, slot: usize, x: u32, y: u32) -> Result<()> {
        let c = self.canvas(slot)?;
        self.conn.configure_window(
            c.window,
            &ConfigureWindowAux::new().x(-(x as i32)).y(-(y as i32)),
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Size of the plugin's own window inside the canvas, as X11 sees it (what the plugin
    /// actually did).
    pub fn plugin_window_size(&self, slot: usize) -> Option<(u32, u32)> {
        let c = self.canvases[slot]?;
        let tree = self.conn.query_tree(c.window).ok()?.reply().ok()?;
        let child = *tree.children.first()?;
        let g = self.conn.get_geometry(child).ok()?.reply().ok()?;
        Some((g.width as u32, g.height as u32))
    }

    /// Is this slot's canvas mapped *and* is the dock mapped, i.e. could it be seen?
    pub fn canvas_viewable(&self, slot: usize) -> bool {
        let Some(c) = self.canvases[slot] else {
            return false;
        };
        self.conn
            .get_window_attributes(c.window)
            .ok()
            .and_then(|r| r.reply().ok())
            .is_some_and(|a| a.map_state == MapState::VIEWABLE)
    }
}
