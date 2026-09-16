//! The call's wire format over one Iroh connection: a reliable control stream for
//! the few messages that must arrive, and unreliable QUIC datagrams for audio,
//! where a late frame is worth less than nothing.

use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

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
}

pub const MAX_CONTROL_LEN: usize = 4096;

pub async fn write_control(
    send: &mut iroh::endpoint::SendStream,
    msg: &Control,
) -> Result<(), String> {
    let body = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    if body.len() > MAX_CONTROL_LEN {
        return Err("control message too large".into());
    }
    let mut buf = Vec::with_capacity(2 + body.len());
    buf.extend_from_slice(&(body.len() as u16).to_be_bytes());
    buf.extend_from_slice(&body);
    send.write_all(&buf).await.map_err(|e| e.to_string())
}

pub async fn read_control(recv: &mut iroh::endpoint::RecvStream) -> Result<Control, String> {
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
    fn a_frame_round_trips() {
        let b = pack(7, 1234, 1, &[9, 8, 7]);
        let f = unpack(&b).unwrap();
        assert_eq!((f.seq, f.flags, f.payload), (7, 1, &[9u8, 8, 7][..]));
        assert!(unpack(&b[..5]).is_none());
    }
}
