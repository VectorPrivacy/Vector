//! The link between the media engine and whatever captures, encodes, decodes and
//! paints video (a webview over a loopback socket, or an in-process port). Binary
//! messages, the first byte says what follows.

use serde::{Deserialize, Serialize};

use super::wire::{VideoCodec, VideoHeader, VideoKind};

/// A video frame: the 16-byte wire header, then the encoded bytes. The same bytes
/// go on the QUIC stream, so nothing is re-framed in between.
pub const KIND_FRAME: u8 = 1;
/// A JSON control message.
pub const KIND_CONTROL: u8 = 2;

/// Sent to the encoder side of the link.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToLink {
    /// Encode at this rate, size and frame rate from now on.
    Rate { kbps: u32, width: u32, height: u32, fps: u32 },
    /// Too many frames in flight: drop the next capture.
    Skip,
    /// Make the next frame a keyframe.
    Keyframe,
    /// What the peer is sending now, so the stage can show or hide their picture.
    Peer { kind: VideoKind },
    /// The peer cannot see us; stop capturing (or resume).
    Pause { on: bool },
    /// Send with this codec: the one the peer decodes.
    Codec { codec: VideoCodec },
}

/// Received from the encoder side of the link.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromLink {
    /// What this device can encode and decode, from its boot probe.
    Caps { encode: Vec<String>, decode: Vec<String> },
    /// The encoder's own numbers, for the stats line.
    Stats { fps: u32, kbps: u32, width: u32, height: u32 },
    /// My decoder lost the chain: ask the peer for a keyframe.
    Lost,
    #[serde(other)]
    Unknown,
}

/// What one link message is.
pub enum LinkMsg<'a> {
    Frame { header: VideoHeader, frame: &'a [u8] },
    Control(FromLink),
}

/// Parses a message from the encoder side. `frame` is the header plus payload, the
/// exact bytes to put on the wire.
pub fn parse(msg: &[u8]) -> Option<LinkMsg<'_>> {
    match msg.first()? {
        &KIND_FRAME => {
            let frame = &msg[1..];
            let header = VideoHeader::parse(frame)?;
            Some(LinkMsg::Frame { header, frame })
        }
        &KIND_CONTROL => serde_json::from_slice(&msg[1..]).ok().map(LinkMsg::Control),
        _ => None,
    }
}

/// A control message ready to send to the encoder side.
pub fn control(msg: &ToLink) -> Vec<u8> {
    let mut out = vec![KIND_CONTROL];
    if let Ok(body) = serde_json::to_vec(msg) {
        out.extend_from_slice(&body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{VIDEO_HEADER_LEN, VIDEO_KEY};

    #[test]
    fn frames_and_controls_parse_and_garbage_does_not() {
        let mut msg = vec![KIND_FRAME];
        let mut hdr = [0u8; VIDEO_HEADER_LEN];
        VideoHeader { seq: 3, ts_us: 9, flags: VIDEO_KEY, codec: VideoCodec::H264 }.write(&mut hdr);
        msg.extend_from_slice(&hdr);
        msg.extend_from_slice(&[7, 7, 7]);
        match parse(&msg) {
            Some(LinkMsg::Frame { header, frame }) => {
                assert_eq!(header.seq, 3);
                assert_eq!(frame.len(), VIDEO_HEADER_LEN + 3);
            }
            _ => panic!("frame expected"),
        }
        let c = control(&ToLink::Rate { kbps: 800, width: 640, height: 360, fps: 30 });
        assert_eq!(c[0], KIND_CONTROL);
        let mut lost = vec![KIND_CONTROL];
        lost.extend_from_slice(br#"{"t":"lost"}"#);
        assert!(matches!(parse(&lost), Some(LinkMsg::Control(FromLink::Lost))));
        let mut odd = vec![KIND_CONTROL];
        odd.extend_from_slice(br#"{"t":"teleport"}"#);
        assert!(matches!(parse(&odd), Some(LinkMsg::Control(FromLink::Unknown))));
        assert!(parse(&[9, 9]).is_none());
        assert!(parse(&[]).is_none());
    }
}
