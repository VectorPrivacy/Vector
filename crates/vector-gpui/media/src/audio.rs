//! What the shared audio engine and call modules read from the desktop client's `audio.rs`.

include!(concat!(env!("OUT_DIR"), "/wav.rs"));

/// Nothing caches the device rate here: notification sounds ask the device each time.
pub fn invalidate_device_sample_rate_cache() {}

pub fn resample_mono_f32(
    samples: Vec<f32>,
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<f32>, String> {
    if from_rate == to_rate {
        return Ok(samples);
    }
    Ok(crate::simd::audio::linear_resample_mono(
        &samples, from_rate, to_rate,
    ))
}

pub fn resample_mono_i16(samples: &[i16], from_rate: u32, to_rate: u32) -> Result<Vec<i16>, String> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let f = crate::simd::audio::i16_le_bytes_to_f32_mono(&bytes);
    let resampled = crate::simd::audio::linear_resample_mono(&f, from_rate, to_rate);
    Ok(crate::simd::audio::f32_to_i16(&resampled))
}
