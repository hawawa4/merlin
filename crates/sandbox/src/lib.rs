//! Plugins in processes of their own (plan: isolation).
//!
//! A plugin is foreign code: when it crashes or hangs it must not take the app, the other
//! plugins or the audio with it. [`RemotePlugin`] starts a `merlin-plugin-host` process that
//! loads the plugin and looks, to the rest of the app, like any other [`PluginHandle`]:
//!
//! - control (parameters, state, the editor) goes over a Unix socket ([`proto`]), every call
//!   with a timeout;
//! - audio goes through shared memory with futex handoff ([`shm`]), so the audio thread never
//!   allocates or waits on a socket, and gives up on a plugin that does not answer;
//! - a plugin whose process dies is silent from then on, and the app is told with
//!   [`merlin_hosting::HostEvent::Crashed`].
//!
//! Plugin editors still sit in the app's window: the host process creates the plugin's X11
//! window as a child of the window the app names, as any X11 client can.

mod client;
pub mod host;
pub mod proto;
pub mod shm;

pub use client::{PluginFailed, RemotePlugin, RemoteProcessor};
