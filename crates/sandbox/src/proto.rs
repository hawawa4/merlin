//! What the app and a plugin host process say to each other over their socket.
//!
//! A message is a JSON header and an optional blob (plugin state can be megabytes, which JSON
//! would bloat): `[u32 json length][u32 blob length][json][blob]`, little-endian. Audio does not
//! travel here; it goes through shared memory (see [`crate::shm`]).

use std::io::{Read, Write};

use anyhow::{Result, bail};
use merlin_hosting::{EditorSize, HostEvent, ParamInfo, PluginInfo};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Refuse headers and blobs bigger than this: a corrupt length must not allocate gigabytes.
const MAX_JSON: usize = 16 << 20;
const MAX_BLOB: usize = 1 << 30;

/// App to host.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// First message: open the plugin file and create an instance.
    Load {
        path: std::path::PathBuf,
        id: String,
        sample_rate: f64,
        max_block: u32,
    },
    /// Does nothing; proves the host's main thread still answers.
    Ping,
    Activate {
        sample_rate: f64,
        max_block: u32,
    },
    Deactivate,
    Params,
    /// Where every parameter is now.
    ParamValues,
    /// No reply.
    SetParam {
        id: u32,
        value: f64,
    },
    SaveState,
    /// The state is the blob.
    LoadState,
    EditorAttach {
        parent: u32,
        scale: f64,
    },
    /// No reply.
    EditorShow,
    /// No reply.
    EditorHide,
    EditorAdjust(EditorSize),
    EditorSet(EditorSize),
    /// No reply.
    EditorNote(EditorSize),
    EditorZoom(f64),
    EditorDetach,
    /// Close the plugin and exit. No reply.
    Shutdown,
}

impl Request {
    pub fn wants_reply(&self) -> bool {
        !matches!(
            self,
            Request::SetParam { .. }
                | Request::EditorShow
                | Request::EditorHide
                | Request::EditorNote(_)
                | Request::Shutdown
        )
    }
}

/// Host to app, answering the request with the same id.
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Err(String),
    Loaded {
        info: PluginInfo,
        has_editor: bool,
    },
    Params(Vec<ParamInfo>),
    ParamValues(Vec<(u32, f64)>),
    /// The state is the blob.
    State,
    Attached {
        size: EditorSize,
        can_resize: bool,
    },
    Size(EditorSize),
    Resized(bool),
    Zoomed(Option<EditorSize>),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ToApp {
    Reply {
        id: u64,
        response: Response,
    },
    /// Something the plugin asked of the main thread.
    Event(HostEvent),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToHost {
    pub id: u64,
    pub request: Request,
}

pub fn write_message<T: Serialize>(w: &mut impl Write, msg: &T, blob: &[u8]) -> Result<()> {
    let json = serde_json::to_vec(msg)?;
    let mut out = Vec::with_capacity(8 + json.len() + blob.len());
    out.extend((json.len() as u32).to_le_bytes());
    out.extend((blob.len() as u32).to_le_bytes());
    out.extend(&json);
    out.extend(blob);
    // One write, so two threads taking turns on the same socket never interleave halves.
    w.write_all(&out)?;
    w.flush()?;
    Ok(())
}

/// `Ok(None)` at a clean end of stream.
pub fn read_message<T: DeserializeOwned>(r: &mut impl Read) -> Result<Option<(T, Vec<u8>)>> {
    let mut head = [0u8; 8];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let json_len = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
    let blob_len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
    if json_len > MAX_JSON || blob_len > MAX_BLOB {
        bail!("message too big ({json_len} + {blob_len} bytes)");
    }
    let mut json = vec![0u8; json_len];
    r.read_exact(&mut json)?;
    let mut blob = vec![0u8; blob_len];
    r.read_exact(&mut blob)?;
    Ok(Some((serde_json::from_slice(&json)?, blob)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_and_its_blob_round_trip() {
        let mut wire = Vec::new();
        let msg = ToHost {
            id: 7,
            request: Request::LoadState,
        };
        write_message(&mut wire, &msg, b"state bytes").unwrap();
        write_message(
            &mut wire,
            &ToHost {
                id: 8,
                request: Request::Ping,
            },
            &[],
        )
        .unwrap();
        let mut r = &wire[..];
        let (a, blob): (ToHost, _) = read_message(&mut r).unwrap().unwrap();
        assert_eq!(a.id, 7);
        assert!(matches!(a.request, Request::LoadState));
        assert_eq!(blob, b"state bytes");
        let (b, blob): (ToHost, _) = read_message(&mut r).unwrap().unwrap();
        assert_eq!(b.id, 8);
        assert!(blob.is_empty());
        assert!(read_message::<ToHost>(&mut r).unwrap().is_none());
    }

    #[test]
    fn garbage_is_an_error_not_an_allocation() {
        let wire = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];
        assert!(read_message::<ToHost>(&mut &wire[..]).is_err());
        // A cut-off message is an error too (the peer died mid-write).
        let mut wire = Vec::new();
        write_message(
            &mut wire,
            &ToHost {
                id: 1,
                request: Request::Ping,
            },
            b"abc",
        )
        .unwrap();
        wire.truncate(wire.len() - 2);
        assert!(read_message::<ToHost>(&mut &wire[..]).is_err());
    }
}
