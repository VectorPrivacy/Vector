//! The call's wire format over one Iroh connection: a reliable control stream for
//! the few messages that must arrive, unreliable QUIC datagrams for audio, where a
//! late frame is worth less than nothing, and one unidirectional QUIC stream per
//! video frame, so a keyframe always lands whole and a stale delta can be reset.

use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const CALL_ALPN: &[u8] = b"vector/call/1";

/// Datagram header: sequence, milliseconds since the sender's media start, flags.
pub const HEADER_LEN: usize = 8;

pub struct Frame<'a> {
    pub seq: u16,
    pub flags: u16,
    pub payload: &'a [u8],
}

pub fn pack(seq: u16, ts_ms: u32, flags: u16, payload: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(HEADER_LEN + payload.len());
    b.put_u16(seq);
    b.put_u32(ts_ms);
    b.put_u16(flags);
    b.put_slice(payload);
    b.freeze()
}

pub fn unpack(data: &[u8]) -> Option<Frame<'_>> {
    if data.len() < HEADER_LEN {
        return None;
    }
    Some(Frame {
        seq: u16::from_be_bytes([data[0], data[1]]),
        flags: u16::from_be_bytes([data[6], data[7]]),
        payload: &data[HEADER_LEN..],
    })
}

/// Control messages, length-prefixed JSON on the bidirectional stream.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Control {
    /// First message from the caller: which call this connection carries.
    Hello { call_id: String },
    Mute { on: bool },
    Bye,
    /// What I am sending on the video streams from now on: either, both or neither,
    /// and whether the screen comes with its sound.
    Video {
        camera: bool,
        screen: bool,
        #[serde(default)]
        audio: bool,
    },
    /// My decoder lost that chain: its next frame must be a keyframe.
    KeyframeRequest { kind: VideoKind },
    /// My view of your video is hidden; stop spending upload on it (or resume).
    VideoPause { on: bool },
    /// My decoder cannot take this codec after all; send with another.
    VideoUnsupported { codec: VideoCodec },
    /// A message from a newer peer: skipped, never fatal.
    #[serde(other)]
    Unknown,
}

/// The two pictures a side can send at once; a frame's flags say which it is.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VideoKind {
    Camera,
    Screen,
}

impl VideoKind {
    pub fn index(self) -> usize {
        match self {
            Self::Camera => 0,
            Self::Screen => 1,
        }
    }
    pub fn from_flags(flags: u8) -> Self {
        if flags & VIDEO_SCREEN != 0 { Self::Screen } else { Self::Camera }
    }
}

/// Which of the two a side is sending.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tracks {
    pub camera: bool,
    pub screen: bool,
}

impl Tracks {
    pub fn any(self) -> bool {
        self.camera || self.screen
    }
    pub fn has(self, kind: VideoKind) -> bool {
        match kind {
            VideoKind::Camera => self.camera,
            VideoKind::Screen => self.screen,
        }
    }
    pub fn with(mut self, kind: VideoKind, on: bool) -> Self {
        match kind {
            VideoKind::Camera => self.camera = on,
            VideoKind::Screen => self.screen = on,
        }
        self
    }
}

/// The codec byte in a video frame header. A fixed table, so nothing from the wire
/// is ever handed to a decoder as a string.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum VideoCodec {
    H264 = 1,
    Vp8 = 2,
}

impl VideoCodec {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::H264),
            2 => Some(Self::Vp8),
            _ => None,
        }
    }
    /// The name the signalling tag and the webview use.
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Vp8 => "vp8",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "h264" => Some(Self::H264),
            "vp8" => Some(Self::Vp8),
            _ => None,
        }
    }
}

/// Video frame flags.
pub const VIDEO_KEY: u8 = 1;
pub const VIDEO_SCREEN: u8 = 2;
/// Header on every video frame, on the QUIC stream and on the link alike.
pub const VIDEO_HEADER_LEN: usize = 16;
/// A frame bigger than this is refused: the whole connection window is only 2 MB,
/// and a bigger frame would sit in front of the control stream. A crisp 1440p
/// keyframe at the top of the screen ladder is a few hundred kilobytes.
pub const MAX_VIDEO_FRAME: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub seq: u32,
    /// Capture time in the sender's microseconds.
    pub ts_us: u64,
    pub flags: u8,
    pub codec: VideoCodec,
}

impl VideoHeader {
    pub fn is_key(&self) -> bool {
        self.flags & VIDEO_KEY != 0
    }

    pub fn write(&self, out: &mut [u8]) {
        debug_assert!(out.len() >= VIDEO_HEADER_LEN);
        out[0..4].copy_from_slice(&self.seq.to_be_bytes());
        out[4..12].copy_from_slice(&self.ts_us.to_be_bytes());
        out[12] = self.flags;
        out[13] = self.codec as u8;
        out[14] = 0;
        out[15] = 0;
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < VIDEO_HEADER_LEN {
            return None;
        }
        Some(Self {
            seq: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
            ts_us: u64::from_be_bytes([data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11]]),
            flags: data[12],
            codec: VideoCodec::from_byte(data[13])?,
        })
    }
}

pub const MAX_CONTROL_LEN: usize = 4096;

pub async fn write_control<W: AsyncWrite + Unpin>(send: &mut W, msg: &Control) -> Result<(), String> {
    let body = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    if body.len() > MAX_CONTROL_LEN {
        return Err("control message too large".into());
    }
    let mut buf = Vec::with_capacity(2 + body.len());
    buf.extend_from_slice(&(body.len() as u16).to_be_bytes());
    buf.extend_from_slice(&body);
    send.write_all(&buf).await.map_err(|e| e.to_string())
}

pub async fn read_control<R: AsyncRead + Unpin>(recv: &mut R) -> Result<Control, String> {
    let mut len = [0u8; 2];
    recv.read_exact(&mut len).await.map_err(|e| e.to_string())?;
    let len = u16::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_CONTROL_LEN {
        return Err("control message length out of range".into());
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_header_round_trips_and_refuses_an_unknown_codec() {
        let h = VideoHeader { seq: 70_000, ts_us: 1 << 40, flags: VIDEO_KEY | VIDEO_SCREEN, codec: VideoCodec::Vp8 };
        let mut buf = [0u8; VIDEO_HEADER_LEN + 3];
        h.write(&mut buf);
        assert_eq!(VideoHeader::parse(&buf), Some(h));
        assert!(h.is_key());
        buf[13] = 9;
        assert!(VideoHeader::parse(&buf).is_none());
        assert!(VideoHeader::parse(&buf[..10]).is_none());
    }

    #[test]
    fn an_unknown_control_message_parses_as_unknown() {
        let c: Control = serde_json::from_str(r#"{"t":"hologram","on":true}"#).unwrap();
        assert_eq!(c, Control::Unknown);
        let v: Control = serde_json::from_str(r#"{"t":"video","camera":false,"screen":true}"#).unwrap();
        assert_eq!(v, Control::Video { camera: false, screen: true, audio: false });
        assert_eq!(serde_json::to_string(&Control::KeyframeRequest { kind: VideoKind::Camera }).unwrap(), r#"{"t":"keyframe_request","kind":"camera"}"#);
        assert_eq!(VideoKind::from_flags(VIDEO_KEY | VIDEO_SCREEN), VideoKind::Screen);
        assert!(Tracks::default().with(VideoKind::Screen, true).has(VideoKind::Screen));
    }

    #[test]
    fn a_frame_round_trips() {
        let b = pack(7, 1234, 1, &[9, 8, 7]);
        let f = unpack(&b).unwrap();
        assert_eq!((f.seq, f.flags, f.payload), (7, 1, &[9u8, 8, 7][..]));
        assert!(unpack(&b[..5]).is_none());
    }
}
