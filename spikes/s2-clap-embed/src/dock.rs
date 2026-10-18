//! The editor dock: an X11 child window of our main window, placed over the area egui
//! leaves free for it. The plugin embeds its editor into the dock; the dock clips it, and
//! scrolling moves the plugin's window inside the dock.

use anyhow::Result;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, InputFocus, WindowClass,
};
use x11rb::rust_connection::RustConnection;

pub struct Dock {
    conn: RustConnection,
    pub window: u32,
    rect: (i32, i32, u32, u32),
    mapped: bool,
}

impl Dock {
    pub fn new(parent: u32) -> Result<Self> {
        let (conn, _) = RustConnection::connect(None)?;
        let window = conn.generate_id()?;
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
        conn.flush()?;
        Ok(Self {
            conn,
            window,
            rect: (0, 0, 1, 1),
            mapped: false,
        })
    }

    /// Place the dock (physical pixels, relative to the main window).
    pub fn set_rect(&mut self, x: i32, y: i32, w: u32, h: u32) -> Result<()> {
        let rect = (x, y, w.max(1), h.max(1));
        if rect != self.rect {
            self.rect = rect;
            self.conn.configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(rect.2)
                    .height(rect.3),
            )?;
            self.conn.flush()?;
        }
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.rect.2, self.rect.3)
    }

    pub fn set_visible(&mut self, visible: bool) -> Result<()> {
        if visible != self.mapped {
            self.mapped = visible;
            if visible {
                self.conn.map_window(self.window)?;
            } else {
                self.conn.unmap_window(self.window)?;
            }
            self.conn.flush()?;
        }
        Ok(())
    }

    /// The plugin's top window inside the dock, if it has created one.
    pub fn plugin_window(&self) -> Option<u32> {
        let tree = self.conn.query_tree(self.window).ok()?.reply().ok()?;
        tree.children.first().copied()
    }

    /// Scroll by moving the plugin's window to a negative offset inside the dock.
    pub fn scroll_to(&self, x: u32, y: u32) -> Result<()> {
        if let Some(child) = self.plugin_window() {
            self.conn.configure_window(
                child,
                &ConfigureWindowAux::new().x(-(x as i32)).y(-(y as i32)),
            )?;
            self.conn.flush()?;
        }
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
