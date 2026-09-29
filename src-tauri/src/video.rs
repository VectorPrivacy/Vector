//! Video compression for sending: any common phone or web video in, H.264 + AAC MP4 out,
//! at most 720p, through the platform's hardware H.264 encoder. FFmpeg is a selective static
//! build (scripts/build-ffmpeg.sh); rare containers and codecs are not compiled in.

use ffmpeg_next as ffmpeg;
use ffmpeg::{codec, encoder, format, frame, media, software, Dictionary, Packet, Rational};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Longest side of a compressed video.
pub const MAX_LONG_SIDE: u32 = 1280;
/// Bits per pixel per frame the video encoder is given: ~2.2 Mbit/s at 720p30.
const BITS_PER_PIXEL: f64 = 0.08;
const AUDIO_BIT_RATE: usize = 128_000;
const AUDIO_RATE: i32 = 48_000;

/// Hardware H.264 encoders, the platform's first. `mpeg4` is FFmpeg's own encoder, compiled
/// in only by a test build (FFMPEG_TEST_ENCODER=1) so the pipeline can run without hardware.
const ENCODERS: &[&str] = &["h264_videotoolbox", "h264_mediacodec", "h264_mf", "mpeg4"];

#[derive(Debug, Clone, serde::Serialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}

fn err(context: &str) -> impl Fn(ffmpeg::Error) -> String + '_ {
    move |e| format!("{context}: {e}")
}

fn init() -> Result<(), String> {
    static INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        ffmpeg::init().map_err(err("ffmpeg init"))?;
        ffmpeg::log::set_level(ffmpeg::log::Level::Error);
        // MediaCodec is a Java API: FFmpeg reaches it through the app's JavaVM.
        #[cfg(target_os = "android")]
        {
            if !crate::android::utils::context_registered() {
                return Err("video: Android context not registered".into());
            }
            let vm = ndk_context::android_context().vm();
            // SAFETY: the JavaVM outlives the process's use of FFmpeg.
            if unsafe { ffmpeg::ffi::av_jni_set_java_vm(vm, std::ptr::null_mut()) } < 0 {
                return Err("video: FFmpeg refused the JavaVM".into());
            }
        }
        Ok(())
    })
    .clone()
}

/// The first encoder this build and device provide.
fn video_encoder() -> Option<ffmpeg::Codec> {
    ENCODERS.iter().find_map(|name| encoder::find_by_name(name))
}

/// Whether this build can compress video at all.
pub fn available() -> bool {
    init().is_ok() && video_encoder().is_some()
}

/// Fit within MAX_LONG_SIDE keeping the aspect ratio, never upscaling, even sides for 4:2:0.
fn output_size(w: u32, h: u32) -> (u32, u32) {
    let scale = (f64::from(MAX_LONG_SIDE) / f64::from(w.max(h))).min(1.0);
    let even = |v: u32| ((f64::from(v) * scale).round() as u32 & !1).max(2);
    (even(w), even(h))
}

/// Compress `input` to an MP4 at `output`. `progress` gets the fraction done; setting
/// `cancel` stops at the next packet with an error.
pub fn compress(input: &Path, output: &Path, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<VideoInfo, String> {
    init()?;
    let mut ictx = format::input(&input).map_err(err("open video"))?;
    let vin = ictx.streams().best(media::Type::Video).ok_or("no video stream")?;
    let vin_index = vin.index();
    let vin_tb = vin.time_base();
    let fps = [vin.avg_frame_rate(), vin.rate()].into_iter().find(|r| r.numerator() > 0 && r.denominator() > 0).unwrap_or(Rational(30, 1));
    let mut decoder = codec::context::Context::from_parameters(vin.parameters()).map_err(err("video decoder"))?.decoder().video().map_err(err("video decoder"))?;
    let display_matrix = display_matrix(&vin);
    let ain = ictx.streams().best(media::Type::Audio).map(|s| (s.index(), s.time_base(), s.parameters()));
    let total_us = ictx.duration().max(1) as f64;

    let (w, h) = output_size(decoder.width(), decoder.height());
    let mut octx = format::output(&output).map_err(err("create output"))?;
    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    // Video: decode, scale, encode.
    let vcodec = video_encoder().ok_or("no video encoder on this device")?;
    let pix = vcodec
        .video()
        .ok()
        .and_then(|v| v.formats())
        .and_then(|mut f| f.find(|p| matches!(p, format::Pixel::YUV420P | format::Pixel::NV12)))
        .unwrap_or(format::Pixel::YUV420P);
    let mut venc = codec::context::Context::new_with_codec(vcodec).encoder().video().map_err(err("video encoder"))?;
    venc.set_width(w);
    venc.set_height(h);
    venc.set_format(pix);
    venc.set_time_base(vin_tb);
    venc.set_frame_rate(Some(fps));
    venc.set_aspect_ratio(decoder.aspect_ratio());
    let fps_f = f64::from(fps.numerator()) / f64::from(fps.denominator());
    venc.set_bit_rate((f64::from(w * h) * fps_f.min(60.0) * BITS_PER_PIXEL) as usize);
    venc.set_gop((fps_f * 2.0).round().max(1.0) as u32);
    if global_header {
        venc.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut venc = venc.open_as_with(vcodec, Dictionary::new()).map_err(err("open video encoder"))?;
    let vout_index = {
        let mut ost = octx.add_stream(vcodec).map_err(err("video stream"))?;
        ost.set_parameters(&venc);
        if let Some(m) = &display_matrix {
            set_display_matrix(&mut ost, m);
        }
        ost.index()
    };
    let mut scaler: Option<software::scaling::Context> = None;

    // Audio: AAC is copied as it is, anything else re-encoded to AAC.
    let mut audio = match ain {
        Some((index, tb, params)) if params.id() == codec::Id::AAC => {
            let mut ost = octx.add_stream(encoder::find(codec::Id::None)).map_err(err("audio stream"))?;
            ost.set_parameters(params);
            // SAFETY: a zero tag lets the MP4 muxer choose the right one for AAC.
            unsafe { (*ost.parameters().as_mut_ptr()).codec_tag = 0 };
            Some(Audio::Copy { index, tb, out: ost.index() })
        }
        Some((index, tb, params)) => Some(Audio::encoder(&mut octx, index, tb, params, global_header)?),
        None => None,
    };

    let mut mux_opts = Dictionary::new();
    // Index up front: the receiver can start playing before the whole file arrives.
    mux_opts.set("movflags", "+faststart");
    octx.write_header_with(mux_opts).map_err(err("write header"))?;
    let vout_tb = octx.stream(vout_index).ok_or("video stream")?.time_base();

    let mut last_progress = 0.0f32;
    let mut decoded = frame::Video::empty();
    let mut scaled = frame::Video::empty();
    for (stream, mut packet) in ictx.packets() {
        if cancel.load(Ordering::Relaxed) {
            return Err("video compression cancelled".into());
        }
        let si = stream.index();
        if si == vin_index {
            decoder.send_packet(&packet).map_err(err("decode video"))?;
            drain_video(&mut decoder, &mut venc, &mut scaler, &mut decoded, &mut scaled, (w, h, pix), vin_tb, vout_index, vout_tb, &mut octx)?;
            if let Some(ts) = packet.pts() {
                let done = (ts as f64 * f64::from(vin_tb) * 1e6 / total_us).clamp(0.0, 1.0) as f32;
                if done - last_progress >= 0.01 {
                    last_progress = done;
                    progress(done);
                }
            }
        } else if let Some(a) = audio.as_mut().filter(|a| a.index() == si) {
            match a {
                Audio::Copy { tb, out, .. } => {
                    let out_tb = octx.stream(*out).ok_or("audio stream")?.time_base();
                    packet.rescale_ts(*tb, out_tb);
                    packet.set_position(-1);
                    packet.set_stream(*out);
                    packet.write_interleaved(&mut octx).map_err(err("write audio"))?;
                }
                Audio::Encode(t) => t.packet(&packet, &mut octx)?,
            }
        }
    }
    decoder.send_eof().map_err(err("decode video"))?;
    drain_video(&mut decoder, &mut venc, &mut scaler, &mut decoded, &mut scaled, (w, h, pix), vin_tb, vout_index, vout_tb, &mut octx)?;
    venc.send_eof().map_err(err("encode video"))?;
    write_encoded(&mut venc, vin_tb, vout_index, vout_tb, &mut octx)?;
    if let Some(Audio::Encode(t)) = audio.as_mut() {
        t.finish(&mut octx)?;
    }
    octx.write_trailer().map_err(err("write trailer"))?;
    progress(1.0);
    Ok(VideoInfo { width: w, height: h, duration_ms: (total_us / 1000.0) as u64 })
}

#[allow(clippy::too_many_arguments)]
fn drain_video(
    decoder: &mut ffmpeg::decoder::Video,
    venc: &mut encoder::Video,
    scaler: &mut Option<software::scaling::Context>,
    decoded: &mut frame::Video,
    scaled: &mut frame::Video,
    (w, h, pix): (u32, u32, format::Pixel),
    in_tb: Rational,
    out_index: usize,
    out_tb: Rational,
    octx: &mut format::context::Output,
) -> Result<(), String> {
    while decoder.receive_frame(decoded).is_ok() {
        // The scaler follows the stream: some files change size or format mid-stream.
        let fits = scaler.as_ref().is_some_and(|s| {
            let i = s.input();
            (i.format, i.width, i.height) == (decoded.format(), decoded.width(), decoded.height())
        });
        if !fits {
            *scaler = Some(
                software::scaling::Context::get(decoded.format(), decoded.width(), decoded.height(), pix, w, h, software::scaling::Flags::BICUBIC)
                    .map_err(err("scaler"))?,
            );
        }
        if let Some(s) = scaler.as_mut() {
            s.run(decoded, scaled).map_err(err("scale"))?;
        }
        scaled.set_pts(decoded.timestamp());
        scaled.set_kind(ffmpeg::picture::Type::None);
        venc.send_frame(scaled).map_err(err("encode video"))?;
        write_encoded(venc, in_tb, out_index, out_tb, octx)?;
    }
    Ok(())
}

fn write_encoded(venc: &mut encoder::Video, in_tb: Rational, out_index: usize, out_tb: Rational, octx: &mut format::context::Output) -> Result<(), String> {
    let mut packet = Packet::empty();
    while venc.receive_packet(&mut packet).is_ok() {
        packet.set_stream(out_index);
        packet.rescale_ts(in_tb, out_tb);
        packet.write_interleaved(octx).map_err(err("write video"))?;
    }
    Ok(())
}

/// The rotation a phone records as stream side data rather than baking into the pixels.
fn display_matrix(stream: &format::stream::Stream) -> Option<Vec<u8>> {
    // SAFETY: reads the stream's own side-data array, copying the entry out.
    unsafe {
        let par = stream.parameters().as_ptr();
        let sd = ffmpeg::ffi::av_packet_side_data_get(
            (*par).coded_side_data,
            (*par).nb_coded_side_data,
            ffmpeg::ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        );
        if sd.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts((*sd).data, (*sd).size).to_vec())
    }
}

fn set_display_matrix(stream: &mut format::stream::StreamMut, matrix: &[u8]) {
    // SAFETY: av_packet_side_data_new allocates `size` bytes owned by the codec parameters,
    // filled here before anything reads them.
    unsafe {
        let par = stream.parameters().as_mut_ptr();
        let sd = ffmpeg::ffi::av_packet_side_data_new(
            &mut (*par).coded_side_data,
            &mut (*par).nb_coded_side_data,
            ffmpeg::ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
            matrix.len(),
            0,
        );
        if !sd.is_null() {
            std::ptr::copy_nonoverlapping(matrix.as_ptr(), (*sd).data, matrix.len());
        }
    }
}

enum Audio {
    Copy { index: usize, tb: Rational, out: usize },
    Encode(Box<AudioTranscode>),
}

impl Audio {
    fn index(&self) -> usize {
        match self {
            Audio::Copy { index, .. } => *index,
            Audio::Encode(t) => t.index,
        }
    }

    fn encoder(octx: &mut format::context::Output, index: usize, tb: Rational, params: codec::Parameters, global_header: bool) -> Result<Self, String> {
        let decoder = codec::context::Context::from_parameters(params).map_err(err("audio decoder"))?.decoder().audio().map_err(err("audio decoder"))?;
        let codec = encoder::find(codec::Id::AAC).ok_or("no AAC encoder")?;
        let mut enc = codec::context::Context::new_with_codec(codec).encoder().audio().map_err(err("audio encoder"))?;
        let layout = ffmpeg::ChannelLayout::STEREO;
        enc.set_rate(AUDIO_RATE);
        enc.set_channel_layout(layout);
        enc.set_format(format::Sample::F32(format::sample::Type::Planar));
        enc.set_bit_rate(AUDIO_BIT_RATE);
        enc.set_time_base(Rational(1, AUDIO_RATE));
        if global_header {
            enc.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let enc = enc.open_as(codec).map_err(err("open audio encoder"))?;
        let out = {
            let mut ost = octx.add_stream(codec).map_err(err("audio stream"))?;
            ost.set_parameters(&enc);
            ost.index()
        };
        let frame_size = (enc.frame_size() as usize).max(1024);
        Ok(Audio::Encode(Box::new(AudioTranscode {
            index,
            in_tb: tb,
            out,
            decoder,
            enc,
            resampler: None,
            fifo: [Vec::new(), Vec::new()],
            frame_size,
            next_pts: 0,
        })))
    }
}

/// Decode, resample to 48 kHz stereo float, and cut into the encoder's frame size.
struct AudioTranscode {
    index: usize,
    in_tb: Rational,
    out: usize,
    decoder: ffmpeg::decoder::Audio,
    enc: encoder::Audio,
    resampler: Option<software::resampling::Context>,
    fifo: [Vec<f32>; 2],
    frame_size: usize,
    next_pts: i64,
}

impl AudioTranscode {
    fn packet(&mut self, packet: &Packet, octx: &mut format::context::Output) -> Result<(), String> {
        let _ = self.in_tb;
        self.decoder.send_packet(packet).map_err(err("decode audio"))?;
        self.drain(octx, false)
    }

    fn finish(&mut self, octx: &mut format::context::Output) -> Result<(), String> {
        self.decoder.send_eof().map_err(err("decode audio"))?;
        self.drain(octx, true)?;
        self.enc.send_eof().map_err(err("encode audio"))?;
        self.write(octx)
    }

    fn drain(&mut self, octx: &mut format::context::Output, last: bool) -> Result<(), String> {
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            if self.resampler.is_none() {
                self.resampler = Some(
                    software::resampling::Context::get(
                        decoded.format(),
                        // Some files leave the layout unset; take the default for the count.
                        Some(decoded.channel_layout()).filter(|l| !l.is_empty()).unwrap_or_else(|| ffmpeg::ChannelLayout::default(i32::from(decoded.channels()))),
                        decoded.rate(),
                        format::Sample::F32(format::sample::Type::Planar),
                        ffmpeg::ChannelLayout::STEREO,
                        AUDIO_RATE as u32,
                    )
                    .map_err(err("resampler"))?,
                );
            }
            let mut out = frame::Audio::empty();
            if let Some(r) = self.resampler.as_mut() {
                r.run(&decoded, &mut out).map_err(err("resample"))?;
            }
            self.push(&out);
        }
        if last {
            if let Some(r) = self.resampler.as_mut() {
                let mut out = frame::Audio::empty();
                if r.flush(&mut out).is_ok() && out.samples() > 0 {
                    self.push(&out);
                }
            }
        }
        while self.fifo[0].len() >= self.frame_size || (last && !self.fifo[0].is_empty()) {
            let n = self.fifo[0].len().min(self.frame_size);
            let mut f = frame::Audio::new(format::Sample::F32(format::sample::Type::Planar), n, ffmpeg::ChannelLayout::STEREO);
            f.set_rate(AUDIO_RATE as u32);
            for (c, ch) in self.fifo.iter_mut().enumerate() {
                f.plane_mut::<f32>(c)[..n].copy_from_slice(&ch[..n]);
                ch.drain(..n);
            }
            f.set_pts(Some(self.next_pts));
            self.next_pts += n as i64;
            self.enc.send_frame(&f).map_err(err("encode audio"))?;
            self.write(octx)?;
        }
        Ok(())
    }

    fn push(&mut self, f: &frame::Audio) {
        let n = f.samples();
        for (c, ch) in self.fifo.iter_mut().enumerate() {
            ch.extend_from_slice(&f.plane::<f32>(c)[..n]);
        }
    }

    fn write(&mut self, octx: &mut format::context::Output) -> Result<(), String> {
        let out_tb = octx.stream(self.out).ok_or("audio stream")?.time_base();
        let mut packet = Packet::empty();
        while self.enc.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.out);
            packet.rescale_ts(Rational(1, AUDIO_RATE), out_tb);
            packet.write_interleaved(octx).map_err(err("write audio"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_fits_720p_keeps_aspect_and_even_sides() {
        assert_eq!(output_size(1920, 1080), (1280, 720));
        assert_eq!(output_size(1080, 1920), (720, 1280));
        assert_eq!(output_size(640, 360), (640, 360));
        assert_eq!(output_size(1281, 721), (1280, 720));
        assert_eq!(output_size(3, 3), (2, 2));
    }

    struct Probe {
        dims: (u32, u32),
        duration_s: f64,
        audio: Option<codec::Id>,
        rotated: bool,
    }

    fn probe(path: &Path) -> Probe {
        let ictx = format::input(&path).unwrap();
        let v = ictx.streams().best(media::Type::Video).unwrap();
        let dec = codec::context::Context::from_parameters(v.parameters()).unwrap().decoder().video().unwrap();
        Probe {
            dims: (dec.width(), dec.height()),
            duration_s: ictx.duration() as f64 / 1e6,
            audio: ictx.streams().best(media::Type::Audio).map(|a| a.parameters().id()),
            rotated: display_matrix(&v).is_some(),
        }
    }

    /// Every file in VIDEO_FIXTURES compresses to a playable MP4 that keeps its length,
    /// orientation and sound. Needs an encoder: a hardware one, or a test build of FFmpeg.
    #[test]
    #[ignore = "needs VIDEO_FIXTURES and an FFmpeg with an encoder"]
    fn fixtures_compress_and_keep_their_length_rotation_and_sound() {
        let dir = std::path::PathBuf::from(std::env::var("VIDEO_FIXTURES").unwrap());
        let out_dir = tempfile::tempdir().unwrap();
        let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let src = probe(&p);
            let out = out_dir.path().join(format!("{}.mp4", p.file_stem().unwrap().to_string_lossy()));
            let t = std::time::Instant::now();
            let mut steps = Vec::new();
            let info = compress(&p, &out, &AtomicBool::new(false), |f| steps.push(f)).unwrap();
            let got = probe(&out);
            let (a, b) = (std::fs::metadata(&p).unwrap().len(), std::fs::metadata(&out).unwrap().len());
            println!("{}: {}x{} -> {}x{}, {} KB -> {} KB, {:.0} ms", p.display(), src.dims.0, src.dims.1, got.dims.0, got.dims.1, a / 1024, b / 1024, t.elapsed().as_secs_f64() * 1e3);
            assert_eq!(got.dims, output_size(src.dims.0, src.dims.1));
            assert_eq!((info.width, info.height), got.dims);
            assert!((got.duration_s - src.duration_s).abs() <= src.duration_s * 0.05 + 0.1, "{} s vs {} s", got.duration_s, src.duration_s);
            assert_eq!(got.audio.is_some(), src.audio.is_some(), "sound kept");
            assert!(got.audio.is_none_or(|id| id == codec::Id::AAC));
            assert_eq!(got.rotated, src.rotated, "orientation kept");
            assert!(steps.windows(2).all(|w| w[0] <= w[1]) && steps.last() == Some(&1.0), "progress runs to 1");
        }
    }

    #[test]
    #[ignore = "needs VIDEO_FIXTURES and an FFmpeg with an encoder"]
    fn a_cancelled_compression_stops_with_an_error() {
        let dir = std::path::PathBuf::from(std::env::var("VIDEO_FIXTURES").unwrap());
        let p = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).next().unwrap();
        let out = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        assert!(compress(&p, &out.path().join("x.mp4"), &cancel, |_| {}).is_err());
    }
}
