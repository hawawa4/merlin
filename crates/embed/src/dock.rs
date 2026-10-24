//! The X11 windows behind the editor dock.
//!
//! - `clip`: a child of the main window, placed over the area our UI leaves free; clips editors
//!   at its edges.
//! - one `canvas` per slot, children of `clip`, sized like that slot's editor. The plugin
//!   embeds into its canvas; scrolling moves the canvas to a negative offset inside `clip`.
//!   Only the selected slot's canvas is mapped.
//!
//! - a slot's canvas can be moved out of `clip` into a top-level window of its own (a "float"),
//!   sized exactly like the editor, and moved back.
//!
//! Why canvases (S2/S4): bridged (Wine/yabridge) editors only learn their screen position from
//! `ConfigureNotify` events on the window they were given and on our top-level. Moving or
//! resizing the canvas itself generates a real `ConfigureNotify` that yabridge translates for
//! Wine. A canvas is kept for the slot's whole life: destroying the parent of an editor that
//! tears down asynchronously is a race (S5).

use anyhow::{Result, anyhow};
use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
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

/// A top-level window holding a slot's canvas.
#[derive(Clone, Copy)]
struct Float {
    window: u32,
}

pub struct Dock {
    conn: RustConnection,
    root: u32,
    wm_protocols: u32,
    wm_delete: u32,
    clip: u32,
    floats: [Option<Float>; MAX_EDITORS],
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
        let (conn, screen) = RustConnection::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS")?.reply()?.atom;
        let wm_delete = conn.intern_atom(false, b"WM_DELETE_WINDOW")?.reply()?.atom;
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
            root,
            wm_protocols,
            wm_delete,
            clip,
            floats: [None; MAX_EDITORS],
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
                if self.floats[i].is_some() {
                    continue;
                }
                if Some(i) == slot {
                    self.conn.map_window(c.window)?;
                } else {
                    self.conn.unmap_window(c.window)?;
                }
            }
        }
        self.shown = slot.filter(|&s| self.canvases[s].is_some() && self.floats[s].is_none());
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
        if let Some(f) = self.floats[slot] {
            self.conn.configure_window(
                f.window,
                &ConfigureWindowAux::new().width(c.size.0).height(c.size.1),
            )?;
            self.fix_size(f.window, c.size)?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Tells the window manager the window is exactly this size and cannot be resized.
    fn fix_size(&self, window: u32, size: (u32, u32)) -> Result<()> {
        let mut hints = WmSizeHints::new();
        let wh = (size.0 as i32, size.1 as i32);
        hints.min_size = Some(wh);
        hints.max_size = Some(wh);
        hints.set_normal_hints(&self.conn, window)?;
        Ok(())
    }

    /// Moves the slot's canvas into a top-level window of its own, as big as the canvas.
    pub fn float(&mut self, slot: usize, title: &str) -> Result<()> {
        let c = self.canvas(slot)?;
        if self.floats[slot].is_some() {
            return Ok(());
        }
        let window = self.conn.generate_id()?;
        self.conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            self.root,
            0,
            0,
            c.size.0 as u16,
            c.size.1 as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(0x151515)
                .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_NOTIFY),
        )?;
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        )?;
        let net_name = self.conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom;
        let utf8 = self.conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
        self.conn
            .change_property8(PropMode::REPLACE, window, net_name, utf8, title.as_bytes())?;
        // yabridge wants a role on the top-level window it forwards moves of.
        let role = self
            .conn
            .intern_atom(false, b"WM_WINDOW_ROLE")?
            .reply()?
            .atom;
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            role,
            AtomEnum::STRING,
            b"merlin-editor",
        )?;
        self.conn.change_property32(
            PropMode::REPLACE,
            window,
            self.wm_protocols,
            AtomEnum::ATOM,
            &[self.wm_delete],
        )?;
        self.fix_size(window, c.size)?;
        self.conn.reparent_window(c.window, window, 0, 0)?;
        self.conn.map_window(c.window)?;
        self.conn.map_window(window)?;
        self.floats[slot] = Some(Float { window });
        self.conn.flush()?;
        self.announce(slot)
    }

    /// Moves a floating canvas back into the dock (unmapped; `show` maps it when it is shown).
    pub fn unfloat(&mut self, slot: usize) -> Result<()> {
        let Some(f) = self.floats[slot].take() else {
            return Ok(());
        };
        let c = self.canvas(slot)?;
        self.conn.unmap_window(c.window)?;
        self.conn.reparent_window(c.window, self.clip, 0, 0)?;
        self.conn.destroy_window(f.window)?;
        self.conn.flush()?;
        Ok(())
    }

    pub fn is_floating(&self, slot: usize) -> bool {
        self.floats[slot].is_some()
    }

    /// Slots whose floating window the user closed since the last call.
    pub fn closed_floats(&mut self) -> Vec<usize> {
        let mut closed = Vec::new();
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            if let Event::ClientMessage(m) = event
                && m.type_ == self.wm_protocols
                && m.data.as_data32()[0] == self.wm_delete
                && let Some(slot) = self
                    .floats
                    .iter()
                    .position(|f| f.is_some_and(|f| f.window == m.window))
            {
                closed.push(slot);
            }
        }
        closed
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
