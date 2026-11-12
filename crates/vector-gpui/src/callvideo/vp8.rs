//! VP8 pictures from peers whose encoder offers nothing else, decoded by FFmpeg's own VP8
//! decoder. Named in our caps only where the FFmpeg we run with carries one.

#[cfg(feature = "video")]
mod imp {
    use ffmpeg_next as ffmpeg;

    pub struct Vp8 {
        dec: ffmpeg::decoder::Video,
        frame: ffmpeg::frame::Video,
    }

    fn codec() -> Option<ffmpeg::Codec> {
        static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*OK.get_or_init(|| ffmpeg::init().is_ok()) {
            return None;
        }
        ffmpeg::decoder::find(ffmpeg::codec::Id::VP8)
    }

    pub fn available() -> bool {
        codec().is_some()
    }

    impl Vp8 {
        pub fn new() -> Result<Self, String> {
            let codec = codec().ok_or("no VP8 decoder in this FFmpeg")?;
            let dec = ffmpeg::codec::context::Context::new_with_codec(codec)
                .decoder()
                .video()
                .map_err(|e| e.to_string())?;
            Ok(Self { dec, frame: ffmpeg::frame::Video::empty() })
        }

        /// One frame in, its picture as (width, height, BGRA) once the decoder has one.
        pub fn decode(&mut self, data: &[u8], max_px: usize) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
            self.dec.send_packet(&ffmpeg::Packet::copy(data)).map_err(|e| e.to_string())?;
            match self.dec.receive_frame(&mut self.frame) {
                Ok(()) => {}
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => return Ok(None),
                Err(e) => return Err(e.to_string()),
            }
            let f = &self.frame;
            if f.format() != ffmpeg::format::Pixel::YUV420P {
                return Err(format!("unexpected VP8 pixel format {:?}", f.format()));
            }
            let (w, h) = (f.width() as usize, f.height() as usize);
            if w == 0 || h == 0 || w * h > max_px {
                return Ok(None);
            }
            let bgra = super::super::yuv::to_bgra(f.data(0), f.data(1), f.data(2), (f.stride(0), f.stride(1)), w, h);
            Ok(Some((w as u32, h as u32, bgra)))
        }
    }
}

#[cfg(not(feature = "video"))]
mod imp {
    pub struct Vp8;

    pub fn available() -> bool {
        false
    }

    impl Vp8 {
        pub fn new() -> Result<Self, String> {
            Err("built without FFmpeg".into())
        }

        pub fn decode(&mut self, _: &[u8], _: usize) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
            Err("built without FFmpeg".into())
        }
    }
}

pub use imp::{Vp8, available};

/// The codecs we take, H.264 always.
pub fn decode_caps() -> Vec<String> {
    let mut caps = vec!["h264".to_string()];
    if available() {
        caps.push("vp8".into());
    }
    caps
}

#[cfg(all(test, feature = "video"))]
mod tests {
    use ffmpeg_next as ffmpeg;
    use ffmpeg::format::Pixel;

    #[test]
    fn decodes_raw_vp8_frames_to_bgra() {
        if !super::available() {
            eprintln!("no VP8 decoder in this FFmpeg; skipped");
            return;
        }
        let Some(codec) = ffmpeg::encoder::find(ffmpeg::codec::Id::VP8) else {
            eprintln!("no VP8 encoder to make frames with; skipped");
            return;
        };
        let (w, h) = (64u32, 48u32);
        let mut enc = ffmpeg::codec::context::Context::new_with_codec(codec).encoder().video().unwrap();
        enc.set_width(w);
        enc.set_height(h);
        enc.set_format(Pixel::YUV420P);
        enc.set_time_base((1, 30));
        enc.set_bit_rate(400_000);
        let mut enc = enc.open().unwrap();
        let mut dec = super::Vp8::new().unwrap();
        let mut seen = Vec::new();
        let drain = |enc: &mut ffmpeg::encoder::Video, dec: &mut super::Vp8, seen: &mut Vec<(u32, u32, Vec<u8>)>| {
            let mut pkt = ffmpeg::Packet::empty();
            while enc.receive_packet(&mut pkt).is_ok() {
                if let Some(pic) = dec.decode(pkt.data().unwrap(), 1 << 24).unwrap() {
                    seen.push(pic);
                }
            }
        };
        for i in 0..6 {
            let mut f = ffmpeg::frame::Video::new(Pixel::YUV420P, w, h);
            let ys = f.stride(0);
            for (n, b) in f.data_mut(0).iter_mut().enumerate() {
                *b = if (n % ys) < (w as usize) / 2 { 40 } else { 220 };
            }
            f.data_mut(1).fill(128);
            f.data_mut(2).fill(128);
            f.set_pts(Some(i));
            enc.send_frame(&f).unwrap();
            drain(&mut enc, &mut dec, &mut seen);
        }
        enc.send_eof().unwrap();
        drain(&mut enc, &mut dec, &mut seen);
        assert!(seen.len() >= 3, "only {} pictures came back", seen.len());
        let (pw, ph, bgra) = seen.last().unwrap();
        assert_eq!((*pw, *ph), (w, h));
        let px = |x: usize, y: usize| bgra[(y * w as usize + x) * 4 + 1];
        assert!(px(8, 24) < 80 && px(56, 24) > 180, "left {} right {}", px(8, 24), px(56, 24));
    }
}
