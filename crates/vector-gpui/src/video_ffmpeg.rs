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

/// Whether FFmpeg is up, for decoders outside this module.
pub fn ready() -> bool {
    init()
}

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
        let index = src.input.streams().best(ffmpeg::media::Type::Video)?.index();
        let audio = src.input.streams().best(ffmpeg::media::Type::Audio).map(|s| s.index());
        Some((Video::of(src, index, threads)?, audio))
    }

    fn of(src: Source, index: usize, threads: usize) -> Option<Video> {
        let stream = src.input.stream(index)?;
        let tb = f64::from(stream.time_base());
        let rot = rotation(&stream);
        let params = stream.parameters();
        // SAFETY: the stream's codec parameters, read while `src` owns them.
        let pixels = unsafe { (*params.as_ptr()).width.max(0) as u64 * (*params.as_ptr()).height.max(0) as u64 };
        let fps = f64::from(stream.avg_frame_rate());
        let threads = if threads > 0 { threads } else { playback_threads(pixels, fps) };
        let mut ctx = ffmpeg::codec::context::Context::from_parameters(params).ok()?;
        ctx.set_threading(ffmpeg::threading::Config { kind: ffmpeg::threading::Type::Frame, count: threads });
        let decoder = ctx.decoder().video().ok()?;
        Some(Video { src, index, decoder, tb, rot })
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

/// A HEIF sequence's track a frame at a time, for the animation path: its delay first, its
/// pixels only for a frame that is kept.
pub struct Sequence {
    video: Video,
    eof: bool,
    at: ffmpeg::frame::Video,
    ahead: Option<ffmpeg::frame::Video>,
    /// A frame's length when the next one gives none: the track's own rate.
    each: u32,
}

impl Sequence {
    /// The track with the most frames, its size as shown, and how many frames it says it has.
    pub fn open(bytes: impl AsRef<[u8]> + Send + 'static) -> Option<(Sequence, u32, u32, usize)> {
        let src = Source::open(Box::new(std::io::Cursor::new(bytes)))?;
        let (index, count, rate) = src
            .input
            .streams()
            .filter(|s| s.parameters().medium() == ffmpeg::media::Type::Video)
            .map(|s| (s.index(), s.frames().max(0) as usize, f64::from(s.avg_frame_rate())))
            .max_by_key(|&(_, n, _)| n)?;
        if count < 2 {
            return None;
        }
        let video = Video::of(src, index, 2)?;
        let (w, h) = video.size();
        let each = if rate > 0. { (1000. / rate).round().max(1.) as u32 } else { 100 };
        Some((Sequence { video, eof: false, at: ffmpeg::frame::Video::empty(), ahead: None, each }, w, h, count))
    }

    fn decoded(&mut self) -> Option<ffmpeg::frame::Video> {
        let mut f = ffmpeg::frame::Video::empty();
        self.video.next(&mut f, &mut self.eof)?;
        Some(f)
    }

    /// The next frame's length in ms; None at the end.
    pub fn advance(&mut self) -> Option<u32> {
        let at = match self.ahead.take() {
            Some(f) => f,
            None => self.decoded()?,
        };
        self.ahead = self.decoded();
        let delay = match &self.ahead {
            Some(next) => self.video.pts(next).saturating_sub(self.video.pts(&at)) as u32,
            None => 0,
        };
        self.at = at;
        Some(if delay == 0 { self.each } else { delay })
    }

    /// The frame `advance` reached, as straight BGRA at its shown size or `to`.
    pub fn take(&mut self, to: Option<(u32, u32)>) -> image::RgbaImage {
        let size = to.unwrap_or_else(|| self.video.size());
        convert(&self.at, size, self.video.rot).unwrap_or_else(|| image::RgbaImage::new(size.0.max(1), size.1.max(1)))
    }
}

/// Each frame thread holds frames of its own: FFmpeg's default of one per core costs ~80 MB a
/// 1080p clip, and two keep up with 1080p30.
fn playback_threads(pixels: u64, fps: f64) -> usize {
    if pixels > 1920 * 1088 || fps > 31. { 4 } else { 2 }
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
    let mut out = super::frame_buffer(ow * oh * 4);
    yuv::convert(&y, ow, oh, rot, &mut out);
    image::RgbaImage::from_raw(target.0, target.1, out)
}

fn target(size: (u32, u32), scale: f32) -> (u32, u32) {
    let (w, h) = fit(size.0, size.1);
    (((w * scale) as u32).clamp(1, size.0.max(1)), ((h * scale) as u32).clamp(1, size.1.max(1)))
}

pub fn probe(input: AudioInput, scale: f32) -> Option<Info> {
    let _trim = crate::trim::OnDrop;
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

/// A track FFmpeg reads that the engine's own decoders do not (Opus in WebM): whole, mono, up
/// to `max` samples.
pub fn decode_audio(input: &AudioInput, max: usize) -> Result<(Vec<f32>, u32), String> {
    let reader: Box<dyn ReadSeek> = Box::new(Reader(input.open()?));
    let mut src = Source::open(reader).ok_or("unreadable")?;
    let stream = src.input.streams().best(ffmpeg::media::Type::Audio).ok_or("no audio")?; // i18n-exempt: developer error
    let index = stream.index();
    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters()).map_err(|e| e.to_string())?;
    let mut dec = ctx.decoder().audio().map_err(|e| e.to_string())?;
    let mut samples = Vec::new();
    let mut frame = ffmpeg::frame::Audio::empty();
    let mut rate = 0;
    let mut drain = |dec: &mut ffmpeg::decoder::Audio, samples: &mut Vec<f32>, rate: &mut u32| {
        while dec.receive_frame(&mut frame).is_ok() {
            *rate = frame.rate();
            mono(&frame, samples);
        }
    };
    let mut packet = ffmpeg::Packet::empty();
    while samples.len() < max {
        match packet.read(&mut src.input) {
            Ok(()) if packet.stream() == index => {
                let _ = dec.send_packet(&packet);
                drain(&mut dec, &mut samples, &mut rate);
            }
            Ok(()) => {}
            Err(_) => break,
        }
    }
    if samples.len() < max {
        let _ = dec.send_eof();
        drain(&mut dec, &mut samples, &mut rate);
    }
    samples.truncate(max);
    if samples.is_empty() || rate == 0 {
        return Err("no audio".into()); // i18n-exempt: developer error
    }
    Ok((samples, rate))
}

/// A decoded frame's channels averaged, onto `out`.
fn mono(frame: &ffmpeg::frame::Audio, out: &mut Vec<f32>) {
    use ffmpeg::format::Sample;
    let n = frame.samples();
    let ch = (frame.channels() as usize).max(1);
    let planar = frame.is_planar();
    let get = |c: usize, i: usize| -> f32 {
        let (plane, at) = if planar { (c, i) } else { (0, i * ch + c) };
        let d = frame.data(plane);
        match frame.format() {
            Sample::F32(_) => f32::from_le_bytes(d[at * 4..at * 4 + 4].try_into().unwrap_or([0; 4])),
            Sample::I16(_) => i16::from_le_bytes([d[at * 2], d[at * 2 + 1]]) as f32 / 32768.,
            Sample::I32(_) => i32::from_le_bytes(d[at * 4..at * 4 + 4].try_into().unwrap_or([0; 4])) as f32 / 2147483648.,
            Sample::F64(_) => f64::from_le_bytes(d[at * 8..at * 8 + 8].try_into().unwrap_or([0; 8])) as f32,
            _ => 0.,
        }
    };
    out.reserve(n);
    for i in 0..n {
        let mut s = 0.;
        for c in 0..ch {
            s += get(c, i);
        }
        out.push(s / ch as f32);
    }
}

/// A clip's sound from any moment on, mono at its own rate, its other tracks never read.
pub struct AudioReader {
    src: Source,
    index: usize,
    dec: ffmpeg::decoder::Audio,
    frame: ffmpeg::frame::Audio,
    tb: f64,
    rate: u32,
    eof: bool,
}

impl AudioReader {
    pub fn open(input: &AudioInput) -> Result<Self, String> {
        let reader: Box<dyn ReadSeek> = Box::new(Reader(input.open()?));
        let mut src = Source::open(reader).ok_or("unreadable")?;
        let stream = src.input.streams().best(ffmpeg::media::Type::Audio).ok_or("no audio")?; // i18n-exempt: developer error
        let (index, tb) = (stream.index(), f64::from(stream.time_base()));
        let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters()).map_err(|e| e.to_string())?;
        let dec = ctx.decoder().audio().map_err(|e| e.to_string())?;
        // A web video's picture would be downloaded only to be dropped.
        // SAFETY: the format context `src` owns, its streams only flagged.
        unsafe {
            let ctx = src.input.as_mut_ptr();
            for i in 0..(*ctx).nb_streams as usize {
                if i != index {
                    (**(*ctx).streams.add(i)).discard = ffmpeg::ffi::AVDiscard::AVDISCARD_ALL;
                }
            }
        }
        let mut r = AudioReader { src, index, dec, frame: ffmpeg::frame::Audio::empty(), tb, rate: 0, eof: false };
        // The decoder's own rate can be the core's (HE-AAC doubles it): the first frame's is the one heard.
        let mut first = Vec::new();
        r.rate = r.next(&mut first).and(Some(r.frame.rate())).filter(|&x| x > 0).ok_or("no audio")?; // i18n-exempt: developer error
        Ok(r)
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn seek(&mut self, ms: u64) {
        let ts = (ms as i64) * 1000;
        let _ = self.src.input.seek(ts, ..ts + 1);
        self.dec.flush();
        self.eof = false;
    }

    /// The next frame's samples onto `out`, with its time in seconds where it has one; None at the end.
    pub fn next(&mut self, out: &mut Vec<f32>) -> Option<Option<f64>> {
        loop {
            if self.dec.receive_frame(&mut self.frame).is_ok() {
                mono(&self.frame, out);
                return Some(self.frame.timestamp().map(|t| t as f64 * self.tb));
            }
            if self.eof {
                return None;
            }
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.src.input) {
                Ok(()) if packet.stream() == self.index => {
                    let _ = self.dec.send_packet(&packet);
                }
                Ok(()) => {}
                Err(_) => {
                    self.eof = true;
                    let _ = self.dec.send_eof();
                }
            }
        }
    }
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
    fn reads_the_fixture_clips() {
        for name in ["clip.mp4", "clip.mkv"] {
            let i = info(&format!("{}/reference/media/{name}", env!("CARGO_MANIFEST_DIR"))).expect(name);
            assert_eq!((i.w, i.h), (640, 360), "{name}");
            assert!(i.audio && (4900..=5100).contains(&i.duration), "{name}: {}", i.duration);
            let p = i.poster.expect("poster");
            assert_eq!((p.size(0).width.0, p.size(0).height.0), (640, 360), "{name}");
        }
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

    /// Videos as other people send them: the fixture clips with random bytes changed, through the
    /// poster, the player's decoder and both sound readers. `VIDFUZZ_DIR` adds seeds,
    /// `VIDFUZZ_ITERS` and `VIDFUZZ_SEED` run more, `VIDFUZZ_OUT` keeps what failed.
    #[test]
    fn no_damaged_video_panics() {
        let mut seeds: Vec<(Vec<u8>, String)> = ["clip.mp4", "turned.mov", "clip.mkv"]
            .iter()
            .map(|n| (std::fs::read(format!("{}/reference/media/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap(), n.rsplit('.').next().unwrap().to_string()))
            .collect();
        if let Ok(dir) = std::env::var("VIDFUZZ_DIR") {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or_default().to_ascii_lowercase();
                seeds.push((std::fs::read(e.path()).unwrap(), ext));
            }
        }
        let iters: usize = std::env::var("VIDFUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
        let mut seed: u64 = std::env::var("VIDFUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5eed_f00d_cafe_d00d);
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let dir = std::env::temp_dir().join(format!("vidfuzz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut failed = Vec::new();
        for i in 0..iters {
            let (mut b, ext) = seeds[i % seeds.len()].clone();
            for _ in 0..1 + next() % 8 {
                let at = (next() % b.len() as u64) as usize;
                match next() % 6 {
                    0 => b[at] = next() as u8,
                    1 => b[at] = 0xff,
                    2 => b[at] = 0,
                    3 if next() % 4 == 0 => b.truncate(at.max(16)),
                    4 => b[at..].iter_mut().take(4).for_each(|x| *x = 0xff),
                    _ => b[at] ^= 1 << (next() % 8),
                }
            }
            let path = dir.join(format!("case.{ext}"));
            std::fs::write(&path, &b).unwrap();
            let input = AudioInput::File(path);
            let t = std::time::Instant::now();
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = probe(input.clone(), 1.);
                let (cmd, cmds) = std::sync::mpsc::channel();
                let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
                let feed = input.clone();
                let player = std::thread::spawn(move || decode(feed, (48, 32), 0, cmds, tx));
                for n in 0..8 {
                    match rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(Out::Frame { .. }) if n == 3 => {
                            let _ = cmd.send(Cmd::Seek(200, 1));
                        }
                        Ok(Out::Frame { .. }) => {}
                        _ => break,
                    }
                }
                let _ = cmd.send(Cmd::Stop);
                drop(rx);
                let played = player.join().is_ok();
                let _ = decode_audio(&input, 48_000 * 4);
                if let Ok(mut r) = AudioReader::open(&input) {
                    let mut out = Vec::new();
                    for n in 0..24 {
                        if n == 6 {
                            r.seek(150);
                        }
                        if r.next(&mut out).is_none() {
                            break;
                        }
                    }
                }
                played
            }))
            .unwrap_or(false);
            if !ok || t.elapsed() > Duration::from_secs(20) {
                eprintln!("case {i} ({ext}, {} bytes): {}", b.len(), if ok { "stalled" } else { "panicked" });
                failed.push((ext, b));
                if failed.len() >= 3 {
                    break;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        if let Ok(out) = std::env::var("VIDFUZZ_OUT") {
            for (i, (ext, b)) in failed.iter().enumerate() {
                std::fs::write(format!("{out}/bad-{i}.{ext}"), b).unwrap();
            }
        }
        assert!(failed.is_empty(), "{} damaged videos panicked or stalled", failed.len());
    }

    /// `VIDEO_PROBE=a.webm:b.mov cargo test probe_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_files() {
        for p in std::env::var("VIDEO_PROBE").unwrap_or_default().split(':').filter(|p| !p.is_empty()) {
            let i = info(p);
            println!("{p}: {:?}", i.map(|i| (i.w, i.h, i.duration, i.audio)));
            if let Ok((s, rate)) = decode_audio(&AudioInput::File(p.into()), usize::MAX) {
                println!("  audio {} samples at {rate}", s.len());
            }
        }
    }
}
