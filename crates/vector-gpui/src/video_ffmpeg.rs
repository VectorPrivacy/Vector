//! The FFmpeg side of video: a demuxer over any reader, frames decoded and converted on their
//! own thread, and the tracks the audio engine cannot read.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::time::Duration;

use ffmpeg_next as ffmpeg;
use gpui::RenderImage;
use vector_media::audio_engine::AudioInput;

use super::{Cmd, Info, Out, fit};
use crate::yuv;

fn init() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        let ok = ffmpeg::init().is_ok();
        ffmpeg::log::set_level(ffmpeg::log::Level::Fatal);
        ok
    })
}

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// A demuxer over any reader: files and held media alike, through FFmpeg's custom IO.
struct Source {
    input: std::mem::ManuallyDrop<ffmpeg::format::context::Input>,
    io: *mut ffmpeg::ffi::AVIOContext,
    opaque: *mut Box<dyn ReadSeek>,
}

// SAFETY: the context, its IO and the reader move together and are used by one thread at a time.
unsafe impl Send for Source {}

impl Drop for Source {
    fn drop(&mut self) {
        // SAFETY: the input closes first (it does not own a custom pb), then the pb and its
        // buffer, then the reader the pb pointed at.
        unsafe {
            std::mem::ManuallyDrop::drop(&mut self.input);
            if !self.io.is_null() {
                ffmpeg::ffi::av_freep(&mut (*self.io).buffer as *mut *mut u8 as *mut std::ffi::c_void);
                ffmpeg::ffi::avio_context_free(&mut self.io);
            }
            drop(Box::from_raw(self.opaque));
        }
    }
}

unsafe extern "C" fn io_read(opaque: *mut std::ffi::c_void, buf: *mut u8, size: i32) -> i32 {
    // SAFETY: FFmpeg hands back the reader it was given and a buffer of `size` bytes.
    let r = unsafe { &mut *(opaque as *mut Box<dyn ReadSeek>) };
    let out = unsafe { std::slice::from_raw_parts_mut(buf, size.max(0) as usize) };
    match r.read(out) {
        Ok(0) => ffmpeg::ffi::AVERROR_EOF,
        Ok(n) => n as i32,
        Err(_) => -5,
    }
}

unsafe extern "C" fn io_seek(opaque: *mut std::ffi::c_void, offset: i64, whence: i32) -> i64 {
    // SAFETY: as io_read.
    let r = unsafe { &mut *(opaque as *mut Box<dyn ReadSeek>) };
    if whence & ffmpeg::ffi::AVSEEK_SIZE as i32 != 0 {
        let Ok(here) = r.stream_position() else { return -1 };
        let size = r.seek(SeekFrom::End(0)).map_or(-1, |s| s as i64);
        let _ = r.seek(SeekFrom::Start(here));
        return size;
    }
    let to = match whence & !(ffmpeg::ffi::AVSEEK_FORCE as i32) {
        0 => SeekFrom::Start(offset.max(0) as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return -1,
    };
    r.seek(to).map_or(-1, |p| p as i64)
}

impl Source {
    fn open(reader: Box<dyn ReadSeek>) -> Option<Source> {
        if !init() {
            return None;
        }
        const BUF: usize = 64 * 1024;
        // SAFETY: each FFmpeg allocation is checked, and freed on the failure paths below or by
        // Drop once the Source owns it.
        unsafe {
            let opaque = Box::into_raw(Box::new(reader));
            let buf = ffmpeg::ffi::av_malloc(BUF) as *mut u8;
            if buf.is_null() {
                drop(Box::from_raw(opaque));
                return None;
            }
            let mut io = ffmpeg::ffi::avio_alloc_context(
                buf,
                BUF as i32,
                0,
                opaque as *mut std::ffi::c_void,
                Some(io_read),
                None,
                Some(io_seek),
            );
            if io.is_null() {
                ffmpeg::ffi::av_free(buf as *mut std::ffi::c_void);
                drop(Box::from_raw(opaque));
                return None;
            }
            let fail = |mut io: *mut ffmpeg::ffi::AVIOContext| {
                ffmpeg::ffi::av_freep(&mut (*io).buffer as *mut *mut u8 as *mut std::ffi::c_void);
                ffmpeg::ffi::avio_context_free(&mut io);
                drop(Box::from_raw(opaque));
            };
            let mut ctx = ffmpeg::ffi::avformat_alloc_context();
            if ctx.is_null() {
                fail(io);
                return None;
            }
            (*ctx).pb = io;
            (*ctx).flags |= ffmpeg::ffi::AVFMT_FLAG_CUSTOM_IO as i32;
            if ffmpeg::ffi::avformat_open_input(&mut ctx, std::ptr::null(), std::ptr::null(), std::ptr::null_mut()) < 0 {
                fail(io);
                return None;
            }
            if ffmpeg::ffi::avformat_find_stream_info(ctx, std::ptr::null_mut()) < 0 {
                ffmpeg::ffi::avformat_close_input(&mut ctx);
                fail(io);
                return None;
            }
            let input = ffmpeg::format::context::Input::wrap(ctx);
            io = (*ctx).pb;
            Some(Source { input: std::mem::ManuallyDrop::new(input), io, opaque })
        }
    }
}

/// Degrees clockwise the file asks the picture be turned.
fn rotation(stream: &ffmpeg::format::stream::Stream) -> u16 {
    // SAFETY: reads the stream's own side-data array.
    let deg = unsafe {
        let par = stream.parameters().as_ptr();
        let sd = ffmpeg::ffi::av_packet_side_data_get(
            (*par).coded_side_data,
            (*par).nb_coded_side_data,
            ffmpeg::ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        );
        if sd.is_null() || (*sd).size < 36 {
            return 0;
        }
        ffmpeg::ffi::av_display_rotation_get((*sd).data as *const i32)
    };
    if !deg.is_finite() {
        return 0;
    }
    let cw = (-deg).rem_euclid(360.);
    (((cw / 90.).round() as u16) % 4) * 90
}

struct Video {
    src: Source,
    index: usize,
    decoder: ffmpeg::decoder::Video,
    tb: f64,
    rot: u16,
}

impl Video {
    /// `threads`: 0 for FFmpeg's choice. A poster wants one: each frame thread holds its own buffers.
    fn open(reader: Box<dyn ReadSeek>, threads: usize) -> Option<(Video, Option<usize>)> {
        let src = Source::open(reader)?;
        let stream = src.input.streams().best(ffmpeg::media::Type::Video)?;
        let index = stream.index();
        let audio = src.input.streams().best(ffmpeg::media::Type::Audio).map(|s| s.index());
        let tb = f64::from(stream.time_base());
        let rot = rotation(&stream);
        let mut ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters()).ok()?;
        ctx.set_threading(ffmpeg::threading::Config { kind: ffmpeg::threading::Type::Frame, count: threads });
        let decoder = ctx.decoder().video().ok()?;
        Some((Video { src, index, decoder, tb, rot }, audio))
    }

    fn size(&self) -> (u32, u32) {
        let (w, h) = (self.decoder.width(), self.decoder.height());
        if self.rot % 180 == 90 { (h, w) } else { (w, h) }
    }

    fn pts(&self, f: &ffmpeg::frame::Video) -> u64 {
        f.timestamp().map_or(0, |t| (t as f64 * self.tb * 1000.).max(0.) as u64)
    }

    /// Reads on until a frame is out, or None at the end.
    fn next(&mut self, frame: &mut ffmpeg::frame::Video, eof: &mut bool) -> Option<()> {
        loop {
            if self.decoder.receive_frame(frame).is_ok() {
                return Some(());
            }
            if *eof {
                return None;
            }
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.src.input) {
                Ok(()) if packet.stream() == self.index => {
                    let _ = self.decoder.send_packet(&packet);
                }
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => {
                    *eof = true;
                    let _ = self.decoder.send_eof();
                }
                Err(_) => {
                    *eof = true;
                    let _ = self.decoder.send_eof();
                }
            }
        }
    }

    fn seek(&mut self, ms: u64) {
        let ts = (ms as i64) * 1000;
        let _ = self.src.input.seek(ts, ..ts + 1);
        self.decoder.flush();
    }
}

fn convert(f: &ffmpeg::frame::Video, target: (u32, u32), rot: u16) -> Option<image::RgbaImage> {
    use ffmpeg::format::Pixel as P;
    let (shift, depth, nv, full) = match f.format() {
        P::YUV420P => ((1, 1), yuv::Depth::U8, false, false),
        P::YUVJ420P => ((1, 1), yuv::Depth::U8, false, true),
        P::YUV422P => ((1, 0), yuv::Depth::U8, false, false),
        P::YUVJ422P => ((1, 0), yuv::Depth::U8, false, true),
        P::YUV444P => ((0, 0), yuv::Depth::U8, false, false),
        P::YUVJ444P => ((0, 0), yuv::Depth::U8, false, true),
        P::NV12 => ((1, 1), yuv::Depth::U8, true, false),
        P::YUV420P10LE => ((1, 1), yuv::Depth::Low(10), false, false),
        P::YUV422P10LE => ((1, 0), yuv::Depth::Low(10), false, false),
        P::YUV444P10LE => ((0, 0), yuv::Depth::Low(10), false, false),
        P::YUV420P12LE => ((1, 1), yuv::Depth::Low(12), false, false),
        P::P010LE => ((1, 1), yuv::Depth::High, true, false),
        _ => return None,
    };
    let full = full || f.color_range() == ffmpeg::color::Range::JPEG;
    let bt709 = match f.color_space() {
        ffmpeg::color::Space::BT709 | ffmpeg::color::Space::BT2020NCL | ffmpeg::color::Space::BT2020CL => true,
        ffmpeg::color::Space::BT470BG | ffmpeg::color::Space::SMPTE170M => false,
        _ => f.height() >= 720,
    };
    let plane = |i: usize| yuv::Plane { data: f.data(i), stride: f.stride(i) };
    let y = yuv::Yuv {
        w: f.width() as usize,
        h: f.height() as usize,
        y: plane(0),
        u: plane(1),
        v: (!nv).then(|| plane(2)),
        shift,
        depth,
        full_range: full,
        bt709,
    };
    let (ow, oh) = (target.0 as usize, target.1 as usize);
    let mut out = vec![0u8; ow * oh * 4];
    yuv::convert(&y, ow, oh, rot, &mut out);
    image::RgbaImage::from_raw(target.0, target.1, out)
}

fn target(size: (u32, u32), scale: f32) -> (u32, u32) {
    let (w, h) = fit(size.0, size.1);
    (((w * scale) as u32).clamp(1, size.0.max(1)), ((h * scale) as u32).clamp(1, size.1.max(1)))
}

pub fn probe(input: AudioInput, scale: f32) -> Option<Info> {
    let reader: Box<dyn ReadSeek> = Box::new(Reader(input.open().ok()?));
    let (mut v, audio) = Video::open(reader, 1)?;
    let (w, h) = v.size();
    if w == 0 || h == 0 {
        return None;
    }
    let mut duration = v.src.input.duration().max(0) as u64 / 1000;
    if duration == 0
        && let Some(s) = v.src.input.stream(v.index)
    {
        duration = (s.duration().max(0) as f64 * v.tb * 1000.) as u64;
    }
    let mut frame = ffmpeg::frame::Video::empty();
    let mut eof = false;
    v.next(&mut frame, &mut eof)?;
    let image = convert(&frame, target((w, h), scale), v.rot)?;
    Some(Info {
        w,
        h,
        duration,
        audio: audio.is_some(),
        poster: Some(Arc::new(RenderImage::new(vec![image::Frame::new(image)]))),
    })
}

/// The decoder thread: frames ahead of the clock until the queue is full, a seek starting a new
/// generation so stale ones are known on arrival.
pub fn decode(input: AudioInput, size: (u32, u32), from: u64, cmds: Receiver<Cmd>, out: SyncSender<Out>) -> Option<()> {
    let reader: Box<dyn ReadSeek> = Box::new(Reader(input.open().ok()?));
    let (mut v, _) = Video::open(reader, 0)?;
    let rot = v.rot;
    let mut epoch = 0u32;
    let mut skip = from;
    let mut eof = false;
    if from > 0 {
        v.seek(from);
    }
    let mut frame = ffmpeg::frame::Video::empty();
    loop {
        let mut wanted = None;
        loop {
            match cmds.try_recv() {
                Ok(Cmd::Stop) | Err(TryRecvError::Disconnected) => return Some(()),
                Ok(Cmd::Seek(ms, g)) => wanted = Some((ms, g)),
                Err(TryRecvError::Empty) => break,
            }
        }
        if let Some((ms, g)) = wanted {
            epoch = g;
            skip = ms;
            eof = false;
            v.seek(ms);
        }
        if v.next(&mut frame, &mut eof).is_none() {
            if out.send(Out::End { epoch }).is_err() {
                return Some(());
            }
            match cmds.recv() {
                Ok(Cmd::Seek(ms, g)) => {
                    epoch = g;
                    skip = ms;
                    eof = false;
                    v.seek(ms);
                    continue;
                }
                _ => return Some(()),
            }
        }
        let pts = v.pts(&frame);
        if pts + 34 < skip {
            continue;
        }
        let Some(image) = convert(&frame, size, rot) else { return None };
        let mut msg = Out::Frame { epoch, pts, image };
        loop {
            match out.try_send(msg) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return Some(()),
                Err(TrySendError::Full(m)) => {
                    msg = m;
                    match cmds.recv_timeout(Duration::from_millis(5)) {
                        Ok(Cmd::Seek(ms, g)) => {
                            epoch = g;
                            skip = ms;
                            eof = false;
                            v.seek(ms);
                            break;
                        }
                        Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return Some(()),
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            }
        }
    }
}

/// A track FFmpeg reads that the engine's own decoders do not (Opus in WebM): whole, mono.
pub fn decode_audio(input: &AudioInput) -> Result<(Vec<f32>, u32), String> {
    use ffmpeg::format::Sample;
    let reader: Box<dyn ReadSeek> = Box::new(Reader(input.open()?));
    let mut src = Source::open(reader).ok_or("unreadable")?;
    let stream = src.input.streams().best(ffmpeg::media::Type::Audio).ok_or("no audio")?;
    let index = stream.index();
    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters()).map_err(|e| e.to_string())?;
    let mut dec = ctx.decoder().audio().map_err(|e| e.to_string())?;
    let mut samples = Vec::new();
    let mut frame = ffmpeg::frame::Audio::empty();
    let mut rate = 0;
    let mut drain = |dec: &mut ffmpeg::decoder::Audio, samples: &mut Vec<f32>, rate: &mut u32| {
        while dec.receive_frame(&mut frame).is_ok() {
            *rate = frame.rate();
            let n = frame.samples();
            let ch = (frame.channels() as usize).max(1);
            let planar = frame.is_planar();
            let get = |c: usize, i: usize| -> f32 {
                let (plane, at) = if planar { (c, i) } else { (0, i * ch + c) };
                let d = frame.data(plane);
                match frame.format() {
                    Sample::F32(_) => f32::from_le_bytes(d[at * 4..at * 4 + 4].try_into().unwrap_or([0; 4])),
                    Sample::I16(_) => i16::from_le_bytes([d[at * 2], d[at * 2 + 1]]) as f32 / 32768.,
                    Sample::I32(_) => {
                        i32::from_le_bytes(d[at * 4..at * 4 + 4].try_into().unwrap_or([0; 4])) as f32 / 2147483648.
                    }
                    Sample::F64(_) => f64::from_le_bytes(d[at * 8..at * 8 + 8].try_into().unwrap_or([0; 8])) as f32,
                    _ => 0.,
                }
            };
            samples.reserve(n);
            for i in 0..n {
                let mut s = 0.;
                for c in 0..ch {
                    s += get(c, i);
                }
                samples.push(s / ch as f32);
            }
        }
    };
    let mut packet = ffmpeg::Packet::empty();
    loop {
        match packet.read(&mut src.input) {
            Ok(()) if packet.stream() == index => {
                let _ = dec.send_packet(&packet);
                drain(&mut dec, &mut samples, &mut rate);
            }
            Ok(()) => {}
            Err(_) => break,
        }
    }
    let _ = dec.send_eof();
    drain(&mut dec, &mut samples, &mut rate);
    if samples.is_empty() || rate == 0 {
        return Err("no audio".into());
    }
    Ok((samples, rate))
}

struct Reader(Box<dyn symphonia::core::io::MediaSource>);

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for Reader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Cmd, Out, QUEUE};

    fn info(path: &str) -> Option<Info> {
        probe(AudioInput::File(path.into()), 2.)
    }

    #[test]
    fn reads_the_fixture_clip() {
        let i = info(concat!(env!("CARGO_MANIFEST_DIR"), "/reference/media/clip.mp4")).expect("probe");
        assert_eq!((i.w, i.h), (640, 360));
        assert!(i.audio && (4900..=5100).contains(&i.duration), "{}", i.duration);
        let p = i.poster.expect("poster");
        assert_eq!((p.size(0).width.0, p.size(0).height.0), (640, 360));
    }

    #[test]
    fn decodes_ahead_and_seeks() {
        let input = AudioInput::File(concat!(env!("CARGO_MANIFEST_DIR"), "/reference/media/clip.mp4").into());
        let (cmd, cmds) = std::sync::mpsc::channel();
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
        let t = std::thread::spawn(move || decode(input, (320, 180), 0, cmds, tx));
        let first = |rx: &Receiver<Out>, want: u32| loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("frame") {
                Out::Frame { epoch, pts, image } if epoch == want => break (pts, image.dimensions()),
                _ => {}
            }
        };
        let (pts, dims) = first(&rx, 0);
        assert_eq!((pts, dims), (0, (320, 180)));
        cmd.send(Cmd::Seek(3000, 1)).unwrap();
        let (pts, _) = first(&rx, 1);
        assert!((2960..=3040).contains(&pts), "{pts}");
        cmd.send(Cmd::Stop).unwrap();
        drop(rx);
        t.join().unwrap();
    }

    /// `VIDEO_PROBE=a.webm:b.mov cargo test probe_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_files() {
        for p in std::env::var("VIDEO_PROBE").unwrap_or_default().split(':').filter(|p| !p.is_empty()) {
            let i = info(p);
            println!("{p}: {:?}", i.map(|i| (i.w, i.h, i.duration, i.audio)));
            if let Ok((s, rate)) = decode_audio(&AudioInput::File(p.into())) {
                println!("  audio {} samples at {rate}", s.len());
            }
        }
    }
}
