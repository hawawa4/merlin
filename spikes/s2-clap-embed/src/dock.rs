//! The editor dock: two X11 windows inside our main window.
//!
//! - `clip`: placed over the area egui leaves free; clips the editor at its edges.
//! - `canvas`: child of `clip`, sized like the editor. The plugin embeds into the canvas,
//!   and scrolling moves the canvas to a negative offset inside `clip`.
//!
//! Why two levels: bridged (Wine/yabridge) editors only learn their screen position from
//! `ConfigureNotify` events on the window they were given (and on our top-level). Moving
//! the plugin's own window, or moving an ancestor, sends nothing they listen to, and Wine
//! then maps every click to the wrong place (S4: no input at all). Moving or resizing the
//! canvas itself generates a real `ConfigureNotify` that yabridge translates for Wine.

use anyhow::Result;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, InputFocus, PropMode,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub struct Dock {
    conn: RustConnection,
    clip: u32,
    /// The window handed to the plugin (`set_parent`).
    pub window: u32,
    rect: (i32, i32, u32, u32),
    canvas_size: (u32, u32),
    scroll: (u32, u32),
    mapped: bool,
}

impl Dock {
    pub fn new(parent: u32) -> Result<Self> {
        let (conn, _) = RustConnection::connect(None)?;
        // yabridge picks the topmost ancestor with WM_WINDOW_ROLE as the "host window" whose
        // moves it forwards to Wine; without one it watches the canvas's direct parent (our
        // clip window), which never moves when the app window does. winit doesn't set a role.
        let role = conn.intern_atom(false, b"WM_WINDOW_ROLE")?.reply()?.atom;
        conn.change_property8(
            PropMode::REPLACE,
            parent,
            role,
            AtomEnum::STRING,
            b"merlin-main",
        )?;
        let create = |window: u32, parent: u32| {
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
            )
        };
        let clip = conn.generate_id()?;
        create(clip, parent)?;
        let window = conn.generate_id()?;
        create(window, clip)?;
        conn.map_window(window)?;
        conn.flush()?;
        Ok(Self {
            conn,
            clip,
            window,
            rect: (0, 0, 1, 1),
            canvas_size: (1, 1),
            scroll: (0, 0),
            mapped: false,
        })
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
            if moved {
                // The canvas moved on screen without being configured itself.
                self.announce()?;
            }
            self.conn.flush()?;
        }
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.rect.2, self.rect.3)
    }

    /// Size the canvas to the editor.
    pub fn set_canvas_size(&mut self, w: u32, h: u32) -> Result<()> {
        self.canvas_size = (w.max(1), h.max(1));
        self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .width(self.canvas_size.0)
                .height(self.canvas_size.1),
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Make the canvas emit a real `ConfigureNotify` so embedded (bridged) editors
    /// recompute their screen position: resize by one pixel and back.
    pub fn announce(&self) -> Result<()> {
        let (w, h) = self.canvas_size;
        for height in [h + 1, h] {
            self.conn.configure_window(
                self.window,
                &ConfigureWindowAux::new().width(w).height(height),
            )?;
        }
        self.conn.flush()?;
        Ok(())
    }

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

    /// The plugin's top window inside the canvas, if it has created one.
    pub fn plugin_window(&self) -> Option<u32> {
        let tree = self.conn.query_tree(self.window).ok()?.reply().ok()?;
        tree.children.first().copied()
    }

    /// Scroll by moving the canvas to a negative offset inside the clip window.
    pub fn scroll_to(&mut self, x: u32, y: u32) -> Result<()> {
        self.scroll = (x, y);
        self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new().x(-(x as i32)).y(-(y as i32)),
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Size of the plugin's window as X11 sees it (what the plugin actually did).
    pub fn plugin_window_size(&self) -> Option<(u32, u32)> {
        let child = self.plugin_window()?;
        let g = self.conn.get_geometry(child).ok()?.reply().ok()?;
        Some((g.width as u32, g.height as u32))
    }

    /// Who has keyboard focus right now (for the focus experiments).
    pub fn focus_owner(&self) -> Option<u32> {
        Some(self.conn.get_input_focus().ok()?.reply().ok()?.focus)
    }

    /// Give keyboard focus to the plugin's window.
    pub fn focus_plugin(&self) -> Result<()> {
        if let Some(child) = self.plugin_window() {
            self.conn
                .set_input_focus(InputFocus::PARENT, child, x11rb::CURRENT_TIME)?;
            self.conn.flush()?;
        }
        Ok(())
    }
}
