//! The codecs Vector decodes audio files with: symphonia's, plus Opus through libopus (as calls
//! use), which symphonia demuxes from Ogg but cannot decode.

use std::sync::{LazyLock, Mutex};

use symphonia::core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia::core::codecs::{
    CodecDescriptor, CodecParameters, CodecRegistry, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_OPUS,
};
use symphonia::core::errors::{decode_error, unsupported_error, Result};
use symphonia::core::formats::Packet;
use symphonia::core::support_codec;

static REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_all::<OpusDecoder>();
    registry
});

/// symphonia's codecs and Opus: every audio file decode goes through this, not `symphonia::default::get_codecs()`.
pub fn codecs() -> &'static CodecRegistry {
    &REGISTRY
}

/// 120 ms at 48 kHz: the longest an Opus packet decodes to.
const MAX_FRAME: usize = 5760;

struct OpusDecoder {
    params: CodecParameters,
    // libopus's decoder is Send but not Sync; symphonia's trait asks for both.
    decoder: Mutex<opus::Decoder>,
    channels: usize,
    interleaved: Vec<f32>,
    buf: AudioBuffer<f32>,
}

impl Decoder for OpusDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> Result<Self> {
        let layout = params.channels.unwrap_or(Channels::FRONT_LEFT);
        let channels = layout.count();
        let mode = match channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => return unsupported_error("opus: more than two channels"),
        };
        let Ok(decoder) = opus::Decoder::new(48_000, mode) else {
            return unsupported_error("opus: decoder unavailable");
        };
        Ok(Self {
            params: params.clone(),
            decoder: Mutex::new(decoder),
            channels,
            interleaved: vec![0.0; MAX_FRAME * channels],
            buf: AudioBuffer::new(MAX_FRAME as u64, SignalSpec::new(48_000, layout)),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_OPUS, "opus", "Opus")]
    }

    fn reset(&mut self) {
        if let Ok(decoder) = self.decoder.get_mut() {
            let _ = decoder.reset_state();
        }
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        self.buf.clear();
        let decoded = match self.decoder.get_mut() {
            Ok(decoder) => decoder.decode_float(&packet.data, &mut self.interleaved, false),
            Err(_) => return decode_error("opus: decoder poisoned"),
        };
        let Ok(frames) = decoded else {
            return decode_error("opus: undecodable packet");
        };
        self.buf.render_reserved(Some(frames));
        for ch in 0..self.channels {
            for (i, sample) in self.buf.chan_mut(ch).iter_mut().enumerate() {
                *sample = self.interleaved[i * self.channels + ch];
            }
        }
        Ok(self.buf.as_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}
