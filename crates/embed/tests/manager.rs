//! The editor manager against a fake plugin on a real X display. Skipped without `DISPLAY`.

use std::time::{Duration, Instant};

use anyhow::Result;
use merlin_embed::{EVICT_DELAY, EditorManager, PluginLookup};
use merlin_hosting::{
    EditorSize, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor,
};
use merlin_model::Format;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, WindowClass};
use x11rb::rust_connection::RustConnection;

struct Fake {
    info: PluginInfo,
    log: Vec<String>,
    parents: Vec<u32>,
    attached: bool,
    size: EditorSize,
    native: EditorSize,
    can_resize: bool,
    /// Resizing keeps the editor's shape (it scales its picture), as Surge's does.
    keeps_shape: bool,
    /// Draws itself at the content scale it is given.
    zoomable: bool,
}

impl Fake {
    fn new(width: u32, height: u32, can_resize: bool) -> Self {
        Self {
            info: PluginInfo {
                format: Format::Clap,
                id: "fake".into(),
                name: "Fake".into(),
                vendor: String::new(),
                version: String::new(),
                features: vec![],
            },
            log: vec![],
            parents: vec![],
            attached: false,
            size: EditorSize { width, height },
            native: EditorSize { width, height },
            can_resize,
            keeps_shape: false,
            zoomable: false,
        }
    }

    fn keeping_shape(mut self) -> Self {
        self.keeps_shape = true;
        self
    }

    fn zoomable(mut self) -> Self {
        self.zoomable = true;
        self
    }
}

impl PluginEditor for Fake {
    fn attach(&mut self, parent: u32, _scale: f64) -> Result<EditorSize> {
        self.log.push("attach".into());
        self.parents.push(parent);
        self.attached = true;
        Ok(self.size)
    }
    fn is_attached(&self) -> bool {
        self.attached
    }
    fn show(&mut self) {
        self.log.push("show".into());
    }
    fn hide(&mut self) {
        self.log.push("hide".into());
    }
    fn size(&self) -> EditorSize {
        self.size
    }
    fn can_resize(&self) -> bool {
        self.can_resize
    }
    fn adjust_size(&mut self, want: EditorSize) -> EditorSize {
        let want = if self.keeps_shape {
            // The biggest copy of its own shape inside what is asked.
            let k = (f64::from(want.width) / f64::from(self.native.width))
                .min(f64::from(want.height) / f64::from(self.native.height));
            EditorSize {
                width: (f64::from(self.native.width) * k) as u32,
                height: (f64::from(self.native.height) * k) as u32,
            }
        } else {
            want
        };
        // Accepts multiples of 8 only.
        EditorSize {
            width: want.width / 8 * 8,
            height: want.height / 8 * 8,
        }
    }
    fn set_zoom(&mut self, scale: f64) -> Option<EditorSize> {
        self.zoomable.then(|| {
            self.log.push(format!("zoom {scale:.3}"));
            // Reports a size that is not trusted (as Six Sines does): a different one.
            EditorSize {
                width: (f64::from(self.native.width) * scale * 1.25) as u32,
                height: (f64::from(self.native.height) * scale * 1.25) as u32,
            }
        })
    }
    fn set_size(&mut self, size: EditorSize) -> bool {
        self.log
            .push(format!("set_size {}x{}", size.width, size.height));
        self.size = size;
        true
    }
    fn note_size(&mut self, size: EditorSize) {
        self.log
            .push(format!("note_size {}x{}", size.width, size.height));
        self.size = size;
    }
    fn detach(&mut self) {
        self.log.push("detach".into());
        self.attached = false;
    }
}

impl PluginHandle for Fake {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn activate(&mut self, _: f64, _: u32) -> Result<Box<dyn PluginProcessor>> {
        anyhow::bail!("not needed")
    }
    fn deactivate(&mut self) {}
    fn params(&mut self) -> Vec<ParamInfo> {
        vec![]
    }
    fn set_param(&mut self, _: u32, _: f64) {}
    fn save_state(&mut self) -> Result<Vec<u8>> {
        Ok(vec![])
    }
    fn load_state(&mut self, _: &[u8]) -> Result<()> {
        Ok(())
    }
    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        Some(self)
    }
    fn on_main_thread(&mut self) {}
    fn service(&mut self, _: Instant) {}
    fn next_deadline(&self) -> Option<Instant> {
        None
    }
    fn on_fd_ready(&mut self) {}
}

struct Rack(Vec<Option<Fake>>);

impl PluginLookup for Rack {
    fn plugin(&mut self, slot: usize) -> Option<&mut dyn PluginHandle> {
        self.0
            .get_mut(slot)?
            .as_mut()
            .map(|f| f as &mut dyn PluginHandle)
    }
}

impl Rack {
    fn log(&self, slot: usize) -> &[String] {
        &self.0[slot].as_ref().unwrap().log
    }
}

/// A mapped top-level window standing in for the app's main window, and a manager inside it.
fn setup(cache_cap: usize) -> Option<(EditorManager, Rack, RustConnection)> {
    let (conn, screen) = RustConnection::connect(None).ok()?;
    let win = conn.generate_id().ok()?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        win,
        conn.setup().roots[screen].root,
        0,
        0,
        800,
        600,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        // Override-redirect: no window manager in the way, the window is mapped when we say.
        &CreateWindowAux::new().override_redirect(1),
    )
    .ok()?;
    conn.map_window(win).ok()?;
    conn.get_input_focus().ok()?.reply().ok()?; // the server has mapped it
    let mut m = EditorManager::new(win, 1.0, cache_cap).ok()?;
    m.set_rect(0, 100, 640, 400).ok()?;
    let rack = Rack(
        (0..4)
            .map(|i| Some(Fake::new(300 + i * 100, 200, true)))
            .collect(),
    );
    Some((m, rack, conn))
}

macro_rules! need_x {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => {
                eprintln!("skipped: no X display");
                return;
            }
        }
    };
}

fn sync(conn: &RustConnection) {
    conn.get_input_focus().unwrap().reply().unwrap();
}

#[test]
fn first_show_attaches_and_switching_hides_the_previous_editor() {
    let (mut m, mut rack, conn) = need_x!(setup(8));
    let out = m.show(0, &mut rack).unwrap();
    assert!(out.created);
    assert_eq!(rack.log(0), ["attach", "show"]);
    let canvas0 = rack.0[0].as_ref().unwrap().parents[0];
    sync(&conn);
    assert!(m.dock().canvas_viewable(0));

    m.show(1, &mut rack).unwrap();
    assert_eq!(
        rack.log(0),
        ["attach", "show", "hide"],
        "previous editor is hidden"
    );
    sync(&conn);
    assert!(!m.dock().canvas_viewable(0));
    assert!(m.dock().canvas_viewable(1));
    assert_ne!(
        canvas0,
        rack.0[1].as_ref().unwrap().parents[0],
        "one canvas per slot"
    );

    // Back to the first: nothing is created again.
    let out = m.show(0, &mut rack).unwrap();
    assert!(!out.created);
    assert_eq!(rack.log(0), ["attach", "show", "hide", "show"]);
    assert_eq!(m.shown(), Some(0));
    assert_eq!(m.live_editors(), 2);
}

#[test]
fn surplus_editors_are_destroyed_later_least_recently_used_first() {
    let (mut m, mut rack, _conn) = need_x!(setup(2));
    for slot in [0, 1, 2] {
        m.show(slot, &mut rack).unwrap();
    }
    assert_eq!(
        m.live_editors(),
        3,
        "nothing is destroyed in the same turn as a switch"
    );
    let now = Instant::now();
    assert!(m.tick(now, &mut rack).is_some(), "eviction is pending");
    assert_eq!(m.live_editors(), 3, "not before the delay");
    assert_eq!(
        m.tick(now + EVICT_DELAY + Duration::from_millis(5), &mut rack),
        None
    );
    assert_eq!(m.live_editors(), 2);
    assert!(
        rack.log(0).contains(&"detach".to_string()),
        "slot 0 was the least recently shown"
    );
    assert!(!rack.log(1).contains(&"detach".to_string()));
    assert!(
        !rack.log(2).contains(&"detach".to_string()),
        "the shown editor is kept"
    );
    assert!(!m.has_editor(0));
}

#[test]
fn a_destroyed_editor_comes_back_on_the_same_canvas() {
    let (mut m, mut rack, _conn) = need_x!(setup(8));
    m.show(1, &mut rack).unwrap();
    m.forget(1, &mut rack).unwrap();
    assert_eq!(m.shown(), None);
    assert!(!m.has_editor(1));
    m.show(1, &mut rack).unwrap();
    let parents = &rack.0[1].as_ref().unwrap().parents;
    assert_eq!(parents.len(), 2);
    assert_eq!(
        parents[0], parents[1],
        "canvases are kept for the slot's life"
    );
}

#[test]
fn the_dock_is_unmapped_while_a_ui_popup_is_open() {
    let (mut m, mut rack, conn) = need_x!(setup(8));
    m.show(0, &mut rack).unwrap();
    sync(&conn);
    assert!(m.dock().canvas_viewable(0));
    m.set_popup_open(true).unwrap();
    sync(&conn);
    assert!(
        !m.dock().canvas_viewable(0),
        "native windows would cover the popup"
    );
    // Switching editors while the popup is open must not bring the dock back.
    m.show(1, &mut rack).unwrap();
    sync(&conn);
    assert!(!m.dock().canvas_viewable(1));
    m.set_popup_open(false).unwrap();
    sync(&conn);
    assert!(m.dock().canvas_viewable(1));
}

#[test]
fn nothing_is_visible_without_a_selected_editor() {
    let (mut m, mut rack, conn) = need_x!(setup(8));
    m.show(0, &mut rack).unwrap();
    m.hide(&mut rack).unwrap();
    sync(&conn);
    assert!(!m.dock().canvas_viewable(0));
    assert!(!m.dock().is_visible());
    assert_eq!(m.live_editors(), 1, "hidden, not destroyed");
    assert_eq!(rack.log(0), ["attach", "show", "hide"]);
}

#[test]
fn fit_asks_a_zoomable_plugin_for_an_absolute_scale_and_sizes_the_window_itself() {
    let (mut m, mut rack, _conn) = need_x!(setup(8));
    rack.0[0] = Some(Fake::new(300, 200, true).zoomable());
    m.set_rect(0, 100, 150, 100).unwrap();
    m.show(0, &mut rack).unwrap();
    m.scroll(10, 20).unwrap();
    m.fit(&mut rack).unwrap();
    // Half the size fits: half the scale, and the window is made that size (the fake takes
    // multiples of 8) whatever size the plugin claims to have.
    assert!(
        rack.log(0).contains(&"zoom 0.500".to_string()),
        "{:?}",
        rack.log(0)
    );
    assert!(rack.log(0).contains(&"set_size 144x96".to_string()));
    let info = m.info(0).unwrap();
    assert_eq!(
        info.size,
        EditorSize {
            width: 144,
            height: 96
        }
    );
    assert_eq!(info.scroll, (0, 0));
    // Asked again for another size, the scale is of the normal size, not of what it is now.
    m.set_rect(0, 100, 75, 50).unwrap();
    m.fit(&mut rack).unwrap();
    assert!(
        rack.log(0).contains(&"zoom 0.250".to_string()),
        "{:?}",
        rack.log(0)
    );
    // Room to spare: never bigger than it was made, so back to the normal scale.
    m.set_rect(0, 100, 640, 400).unwrap();
    m.fit(&mut rack).unwrap();
    assert!(
        rack.log(0).contains(&"zoom 1.000".to_string()),
        "{:?}",
        rack.log(0)
    );
}

#[test]
fn fit_resizes_a_plugin_that_keeps_its_shape_when_it_has_no_zoom() {
    let (mut m, mut rack, _conn) = need_x!(setup(8));
    rack.0[0] = Some(Fake::new(300, 200, true).keeping_shape());
    m.set_rect(0, 100, 150, 100).unwrap();
    m.show(0, &mut rack).unwrap();
    m.fit(&mut rack).unwrap();
    assert!(
        rack.log(0).contains(&"set_size 144x96".to_string()),
        "{:?}",
        rack.log(0)
    );
    assert_eq!(
        m.info(0).unwrap().size,
        EditorSize {
            width: 144,
            height: 96
        }
    );
}

#[test]
fn fit_leaves_alone_a_plugin_that_would_only_crop_and_one_that_cannot_resize() {
    let (mut m, mut rack, _conn) = need_x!(setup(8));
    // Slot 0 takes any size (a fixed layout in a bigger or smaller window): resizing it would
    // cut its picture, so it is left at its own size, for the scroll bars.
    m.set_rect(0, 100, 150, 100).unwrap();
    m.show(0, &mut rack).unwrap();
    m.fit(&mut rack).unwrap();
    assert!(!rack.log(0).iter().any(|l| l.starts_with("set_size")));
    assert_eq!(
        m.info(0).unwrap().size,
        EditorSize {
            width: 300,
            height: 200
        }
    );
    // One that cannot be resized is left alone too.
    rack.0[1] = Some(Fake::new(500, 300, false));
    m.show(1, &mut rack).unwrap();
    m.fit(&mut rack).unwrap();
    assert!(!rack.log(1).iter().any(|l| l.starts_with("set_size")));
}

#[test]
fn a_plugin_requested_resize_is_accepted_without_set_size() {
    let (mut m, mut rack, _conn) = need_x!(setup(8));
    m.show(0, &mut rack).unwrap();
    m.plugin_resized(
        0,
        EditorSize {
            width: 777,
            height: 555,
        },
        &mut rack,
    )
    .unwrap();
    assert!(rack.log(0).contains(&"note_size 777x555".to_string()));
    assert!(
        !rack.log(0).iter().any(|l| l.starts_with("set_size")),
        "JUCE flags unadjusted set_size"
    );
    assert_eq!(
        m.info(0).unwrap().size,
        EditorSize {
            width: 777,
            height: 555
        }
    );
    // Requests for slots without an editor are ignored.
    m.plugin_resized(
        3,
        EditorSize {
            width: 1,
            height: 1,
        },
        &mut rack,
    )
    .unwrap();
}

#[test]
fn shutdown_destroys_every_editor() {
    let (mut m, mut rack, _conn) = need_x!(setup(8));
    m.show(0, &mut rack).unwrap();
    m.show(2, &mut rack).unwrap();
    m.shutdown(&mut rack);
    assert_eq!(m.live_editors(), 0);
    for slot in [0, 2] {
        assert!(rack.log(slot).contains(&"detach".to_string()));
    }
    assert_eq!(m.shown(), None);
}
