//! Looks for a newer Merlin among the GitHub releases and installs it in place. A bare-binary
//! install (the tarball) replaces its own files; a `.deb` install hands the downloaded package
//! to apt through `pkexec`. Nothing here runs by itself: the app calls [`release::latest`] only
//! when the user asks. Like the Windows plugin runtime, downloads go through `curl`, and every
//! download is checked against the release's `SHA256SUMS.txt` (a release without one is refused).

mod http;
pub mod install;
pub mod kind;
pub mod release;
pub mod version;

pub use install::{Progress, install};
pub use kind::{Install, Kind, deb_prerequisites, detect};
pub use release::{Asset, Release, latest};
pub use version::{is_release, newer};
