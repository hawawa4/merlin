//! Keeping a new window from taking the keyboard focus.

use anyhow::Result;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Tells the window manager that nobody interacted with `window` yet, by setting its
/// `_NET_WM_USER_TIME` to 0 (EWMH). A window manager then shows the window without giving it
/// the focus, so starting the app never pulls the keyboard away from what the user is typing
/// in. Set it before the window is mapped.
pub fn show_without_focus(window: u32) -> Result<()> {
    let (conn, _) = RustConnection::connect(None)?;
    let atom = conn.intern_atom(false, b"_NET_WM_USER_TIME")?.reply()?.atom;
    conn.change_property32(PropMode::REPLACE, window, atom, AtomEnum::CARDINAL, &[0])?;
    // Wait until the server has it: the caller maps the window right after.
    conn.get_input_focus()?.reply()?;
    Ok(())
}
