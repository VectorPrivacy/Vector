use std::io::{self, Write};
use std::path::Path;
use hound::{WavReader, SampleFormat};
use rubato::*;
use futures_util::StreamExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use tauri::{AppHandle, Runtime, Manager, Emitter};
use serde::Serialize;

/// Whisper model information
#[derive(Serialize, Clone)]
pub struct WhisperModel {
    /// Model name (used in filenames and API requests)
    pub name: &'static str,
    /// Display name (used in the UI as a simplified name)
    pub display_name: &'static str,
    /// Approximate size in MB
    pub size: usize,
}

/// List of supported Whisper models with their details
pub const MODELS: [WhisperModel; 5] = [
    WhisperModel { name: "tiny", display_name: "Lowest Quality - Fastest", size: 75 },
    WhisperModel { name: "base", display_name: "Low Quality - Faster", size: 142 },
    WhisperModel { name: "small", display_name: "Base Quality - Fast", size: 466 },
    WhisperModel { name: "medium", display_name: "High Quality - Slow", size: 1500 },
    WhisperModel { name: "large-v3", display_name: "Highest Quality - Slowest", size: 2900 },
];

pub async fn transcribe<R: Runtime>(handle: &AppHandle<R>, model_name: &str, translate: bool, audio: Vec<f32>) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // Download or get cached whisper model
    println!("Initializing Whisper...");
    let model_path = download_whisper_model(handle, model_name).await?;
    
    let ctx_params = WhisperContextParameters::default();
    let ctx = WhisperContext::new_with_params(&model_path, ctx_params)?;

    // Configure the parameters
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_realtime(false);
    params.set_print_progress(false);
    params.set_print_timestamps(true);
    params.set_language(Some("auto"));
    params.set_translate(translate);
    params.set_suppress_non_speech_tokens(true);

    // Create state and run inference
    println!("Transcribing audio...");
    let mut state = ctx.create_state()?;
    state.full(params, &audio)?;

    // Get the number of segments
    let num_segments = state.full_n_segments()?;

    // Collect and print the transcription
    println!("\n----- Transcription -----");
    let mut full_text = String::new();
    for i in 0..num_segments {
        let segment = state.full_get_segment_text(i)?;
        let start_time = state.full_get_segment_t0(i)?;
        let end_time = state.full_get_segment_t1(i)?;

        let start_mins = start_time / 60;
        let start_secs = start_time % 60;
        let end_mins = end_time / 60;
        let end_secs = end_time % 60;

    println!("[{:02}:{:05.2}-{:02}:{:05.2}] {}", 
        start_mins, start_secs, end_mins, end_secs, segment);

    // Skip empty segments, single punctuation, or [BLANK_AUDIO] markers
    let trimmed = segment.trim();
    if !trimmed.is_empty() && 
       !trimmed.eq(",") && 
       !trimmed.eq(".") && 
       !trimmed.eq("[BLANK_AUDIO]") {
        full_text.push_str(&segment);
        full_text.push_str("\n");
    }
    }
    println!("------------------------");
    
    Ok(full_text)
}

pub fn resample_audio(path: &Path, target_rate: u32) -> Result<Vec<f32>, Box<dyn std::error::Error + Send + Sync>> {
    println!("Resampling to {} Hz", target_rate);
    
    // Read WAV file
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    
    // Read all samples into a buffer
    let mut samples = Vec::new();
    
    // Handle different bit depths and formats
    if spec.sample_format == SampleFormat::Int {
        match spec.bits_per_sample {
            16 => {
                for sample in reader.samples::<i16>() {
                    samples.push(sample? as f32 / i16::MAX as f32);
                }
            },
            24 | 32 => {
                for sample in reader.samples::<i32>() {
                    samples.push(sample? as f32 / i32::MAX as f32);
                }
            },
            _ => {
                return Err(format!("Unsupported bits per sample: {}", spec.bits_per_sample).into());
            }
        }
    } else {
        for sample in reader.samples::<f32>() {
            samples.push(sample?);
        }
    }
    
    println!("Read {} samples from input file", samples.len());
    
    // Organize samples by channel
    let channels = spec.channels as usize;
    let mut channels_data = vec![Vec::new(); channels];
    
    for (i, sample) in samples.iter().enumerate() {
        channels_data[i % channels].push(*sample);
    }
    
    println!("Channel 0 has {} samples", channels_data[0].len());
    
    // Calculate the resample ratio
    let resample_ratio = target_rate as f64 / spec.sample_rate as f64;
    
    // Set up the resampler with SincFixedIn
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    
    // Use correct constructor for SincFixedIn
    let mut resampler = SincFixedIn::<f32>::new(
        resample_ratio,        // Ratio of target/source sample rates
        1.0,                   // Max resample ratio relative
        params,                // Sinc parameters
        channels_data[0].len(),// Chunk size (input length)
        channels,              // Number of channels
    )
    .map_err(|e| format!("Failed to create resampler: {}", e))?;
    
    // Resample
    println!("Resampling...");
    let resampled_data = Resampler::process(&mut resampler, &channels_data, None)
        .map_err(|e| format!("Failed to resample audio: {}", e))?;
    
    println!("After resampling, channel 0 has {} samples", resampled_data[0].len());
    
    // Interleave the resampled channel data into a single Vec<f32>
    println!("Preparing audio data...");
    let mut result = Vec::with_capacity(resampled_data[0].len() * channels);
    
    for i in 0..resampled_data[0].len() {
        for channel in 0..channels {
            result.push(resampled_data[channel][i]);
        }
    }
    
    println!("Resampling complete! Got {} samples", result.len());
    
    Ok(result)
}

/// Checks if a Whisper model is already downloaded
/// 
/// # Arguments
/// * `handle` - The Tauri app handle for accessing app paths
/// * `model_name` - The name of the model to check (e.g., "tiny", "base", "small", etc.)
/// 
/// # Returns
/// * `bool` - true if the model is already downloaded, false otherwise
pub fn is_model_downloaded<R: Runtime>(handle: &AppHandle<R>, model_name: &str) -> bool {
    // Get models directory in app data directory
    let models_dir = handle.path().app_local_data_dir().unwrap().join("whisper");
    
    // Construct model path
    let model_filename = format!("ggml-{}.bin", model_name);
    let model_path = models_dir.join(&model_filename);
    
    // Check if model exists
    model_path.exists()
}

/// Information about a Whisper model and its download status
#[derive(Serialize, Clone)]
pub struct WhisperModelStatus {
    /// The name of the model
    pub model: WhisperModel,
    /// Whether the model is already downloaded
    pub downloaded: bool,
}

/// Deletes a Whisper model from local storage
/// 
/// # Arguments
/// * `handle` - The Tauri app handle for accessing app paths
/// * `model_name` - The name of the model to delete (e.g., "tiny", "base", "small", etc.)
/// 
/// # Returns
/// * `bool` - true if the model was successfully deleted, false if it doesn't exist
#[tauri::command]
pub async fn delete_whisper_model<R: Runtime>(handle: AppHandle<R>, model_name: String) -> bool {
    // Get models directory in app data directory
    let models_dir = handle.path().app_local_data_dir().unwrap().join("whisper");
    
    // Construct model path
    let model_filename = format!("ggml-{}.bin", model_name);
    let model_path = models_dir.join(&model_filename);
    
    // Check if model exists
    if model_path.exists() {
        // Delete the model file
        match std::fs::remove_file(&model_path) {
            Ok(_) => {
                println!("Successfully deleted model: {}", model_path.display());
                true
            },
            Err(e) => {
                eprintln!("Failed to delete model {}: {}", model_path.display(), e);
                false
            }
        }
    } else {
        // Model doesn't exist
        println!("Model not found: {}", model_path.display());
        false
    }
}

/// Lists all available Whisper models and their download status
/// 
/// # Returns
/// * `Vec<WhisperModelStatus>` - A vector of model status objects containing names and download status
#[tauri::command]
pub async fn list_models(app_handle: tauri::AppHandle) -> Vec<WhisperModelStatus> {
    // Check each model's download status and create a list
    MODELS
        .iter()
        .map(|model| {
            let is_downloaded = is_model_downloaded(&app_handle, model.name);
            WhisperModelStatus {
                model: model.clone(),
                downloaded: is_downloaded,
            }
        })
        .collect()
}

pub async fn download_whisper_model<R: Runtime>(handle: &AppHandle<R>, model_name: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // Get models directory in app data directory
    let models_dir = handle.path().app_local_data_dir().unwrap().join("whisper");
    
    // Create directory if it doesn't exist
    std::fs::create_dir_all(&models_dir)?;
    
    // Construct model path
    let model_filename = format!("ggml-{}.bin", model_name);
    let model_path = models_dir.join(&model_filename);
    
    // Check if model already exists
    if model_path.exists() {
        println!("Using cached model: {}", model_path.display());
        return Ok(model_path.to_string_lossy().to_string());
    }
    
    // Get model size for progress estimation (in MB)
    let model_size = MODELS
        .iter()
        .find(|model| model.name == model_name)
        .map(|model| model.size)
        .unwrap_or(100);
    
    // Model needs to be downloaded
    println!("Downloading {} model (~{}MB), please wait...", model_name, model_size);
    
    // Create client with longer timeout
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600)) // 60 minute timeout
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
        .build()?;
    
    // Try multiple sources for the model
    let urls = [
        format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", model_filename),
        format!("https://github.com/ggerganov/whisper.cpp/raw/master/models/{}", model_filename)
    ];
    
    // Try each URL until one works
    let mut response = None;
    let mut last_error = None;
    
    for url in &urls {
        println!("Trying to download from: {}", url);
        match client.get(url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    println!("Successfully connected to {}", url);
                    println!("Got response status: {}", resp.status());
                    
                    // Check if we can get content length
                    let size = resp.content_length();
                    if let Some(length) = size {
                        println!("Content length: {} bytes ({:.2} MB)", 
                               length, (length as f64) / (1024.0 * 1024.0));
                    } else {
                        println!("Content length unknown");
                    }
                    
                    response = Some(resp);
                    break;
                } else {
                    println!("Failed with status: {}", resp.status());
                    last_error = Some(format!("HTTP error: {}", resp.status()));
                }
            },
            Err(e) => {
                println!("Error connecting to {}: {}", url, e);
                last_error = Some(format!("Connection error: {}", e));
            }
        }
    }
    
    // If no URL worked, return the last error
    let response = match response {
        Some(r) => r,
        None => {
            let error_msg = last_error.unwrap_or_else(|| 
                "Failed to download model from all sources".to_string());
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other, error_msg)));
        }
    };
    
    // Get content length
    let total_size = response.content_length().unwrap_or(0);
    
    let mut file = std::fs::File::create(&model_path)?;
    let mut downloaded: u64 = 0;
    
    // Stream chunks and write to file
    println!("Starting to download chunks...");
    let mut stream = response.bytes_stream();
    let mut chunk_count = 0;
    while let Some(chunk_result) = stream.next().await {
        chunk_count += 1;
        match chunk_result {
            Ok(chunk) => {
                let chunk_size = chunk.len() as u64;
                std::io::Write::write_all(&mut file, &chunk)?;
                
                downloaded += chunk_size;
                
                if total_size > 0 {
                    let percent = (downloaded * 100) / total_size;
                    print!("\rDownloading: {}% ({}/{} bytes, {} chunks)", 
                           percent, downloaded, total_size, chunk_count);
                    io::stdout().flush()?;
                } else {
                    print!("\rDownloaded: {} MB ({} chunks)", 
                           downloaded / (1024 * 1024), chunk_count);
                    io::stdout().flush()?;
                }
                
                // Print a progress message every 5MB
                if chunk_count % 50 == 0 {
                    handle.emit("whisper_download_progress", serde_json::json!({
                        "progress": (downloaded * 100) / total_size
                    })).unwrap();
                    println!("\nProgress update: Downloaded {} MB so far...", 
                             downloaded / (1024 * 1024));
                }
            },
            Err(e) => {
                println!("\nError downloading chunk {}: {}", chunk_count, e);
                return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, 
                           format!("Failed to download chunk: {}", e))));
            }
        }
    }
    
    println!("\nModel downloaded to: {}", model_path.display());
    
    Ok(model_path.to_string_lossy().to_string())
}
