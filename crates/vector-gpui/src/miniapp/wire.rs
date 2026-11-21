//! The pipe between the app and its Linux webview host process: a JSON header and a raw body
//! per frame, each length-prefixed, so served files cross without an encoding pass.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Open {
    pub label: String,
    pub title: String,
    pub url: String,
    pub init: String,
    pub farewell: String,
    pub proxy: Option<(String, u16)>,
    pub isolated: Option<Isolated>,
}

#[derive(Serialize, Deserialize)]
pub struct Isolated {
    pub data_dir: PathBuf,
    pub boot_url: String,
    pub sink: String,
    /// Hosts the null proxy lets through: the app's own origin and the realtime socket.
    pub bypass: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub enum ToHost {
    Open(Open),
    Player { title: String, url: String, size: (f32, f32), init: String },
    Focus { label: String, navigate: Option<String> },
    Eval { label: String, js: String },
    Destroy { label: String },
    Close { label: String },
    /// Answers `FromHost::Serve`; the body travels in the frame.
    Reply { id: u64, status: u16, headers: Vec<(String, String)> },
}

#[derive(Serialize, Deserialize)]
pub enum FromHost {
    Ipc { label: String, body: String },
    Closed { label: String },
    Failed { label: String, error: String },
    Player { body: String },
    Browse { url: String },
    /// A `webxdc://` request; the body travels in the frame.
    Serve { id: u64, label: String, method: String, uri: String, headers: Vec<(String, String)> },
}

pub fn write<T: Serialize>(w: &mut impl Write, msg: &T, body: &[u8]) -> io::Result<()> {
    let head = serde_json::to_vec(msg).map_err(io::Error::other)?;
    w.write_all(&(head.len() as u32).to_le_bytes())?;
    w.write_all(&head)?;
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(body)?;
    w.flush()
}

pub fn read<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<(T, Vec<u8>)> {
    let head = chunk(r)?;
    let msg = serde_json::from_slice(&head).map_err(io::Error::other)?;
    Ok((msg, chunk(r)?))
}

fn chunk(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}
