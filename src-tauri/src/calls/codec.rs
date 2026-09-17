//! Opus for calls: one 20 ms mono frame per packet at the engine rate.

use super::rate::START_KBPS;
use super::{ENGINE_RATE, FRAME};

pub struct Encoder(opus::Encoder);

impl Encoder {
    pub fn new() -> Result<Self, String> {
        let mut enc = opus::Encoder::new(ENGINE_RATE, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|e| format!("opus encoder: {e}"))?;
        enc.set_vbr(true).map_err(|e| e.to_string())?;
        // In-band FEC lets the decoder rebuild a lost frame from its successor.
        enc.set_inband_fec(true).map_err(|e| e.to_string())?;
        let mut enc = Self(enc);
        enc.set_rate(START_KBPS, 10)?;
        Ok(enc)
    }

    /// The bitrate and the loss the encoder should expect: more expected loss and
    /// Opus spends more of the bitrate on FEC, less on the frame itself.
    pub fn set_rate(&mut self, kbps: u32, loss_pct: u32) -> Result<(), String> {
        self.0.set_bitrate(opus::Bitrate::Bits((kbps * 1000) as i32)).map_err(|e| e.to_string())?;
        self.0.set_packet_loss_perc(loss_pct as i32).map_err(|e| e.to_string())
    }

    /// Encodes one frame into `out`, returning the packet length.
    pub fn encode(&mut self, frame: &[i16], out: &mut [u8]) -> Result<usize, String> {
        debug_assert_eq!(frame.len(), FRAME);
        self.0.encode(frame, out).map_err(|e| format!("opus encode: {e}"))
    }
}

pub struct Decoder(opus::Decoder);

impl Decoder {
    pub fn new() -> Result<Self, String> {
        opus::Decoder::new(ENGINE_RATE, opus::Channels::Mono)
            .map(Self)
            .map_err(|e| format!("opus decoder: {e}"))
    }

    /// Decodes one packet into one frame.
    pub fn decode(&mut self, packet: &[u8], out: &mut [i16]) -> Result<usize, String> {
        self.0.decode(packet, out, false).map_err(|e| format!("opus decode: {e}"))
    }

    /// Rebuilds the frame BEFORE `next` from the FEC data `next` carries.
    pub fn decode_fec(&mut self, next: &[u8], out: &mut [i16]) -> Result<usize, String> {
        self.0.decode(next, out, true).map_err(|e| format!("opus fec: {e}"))
    }

    /// Packet loss concealment: extrapolates one frame from the decoder's state.
    pub fn conceal(&mut self, out: &mut [i16]) -> Result<usize, String> {
        self.0.decode(&[], out, false).map_err(|e| format!("opus plc: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_survives_a_round_trip() {
        let mut enc = Encoder::new().unwrap();
        let mut dec = Decoder::new().unwrap();
        let frame: Vec<i16> = (0..FRAME).map(|i| ((i as f32 * 0.2).sin() * 8000.0) as i16).collect();
        let mut pkt = vec![0u8; 400];
        let n = enc.encode(&frame, &mut pkt).unwrap();
        assert!(n > 0 && n < 200, "packet {n} bytes");
        let mut out = vec![0i16; FRAME];
        assert_eq!(dec.decode(&pkt[..n], &mut out).unwrap(), FRAME);
        let mut plc = vec![0i16; FRAME];
        assert_eq!(dec.conceal(&mut plc).unwrap(), FRAME);
    }
}
