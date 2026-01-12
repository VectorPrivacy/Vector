use std::io::{self, Write};
use futures_util::StreamExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use tauri::{AppHandle, Runtime, Manager, Emitter};
use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct TranscriptionSection {
    pub text: String,
    pub at: i64, // millisecond timestamp
}

#[derive(Serialize, Clone)]
pub struct TranscriptionResult {
    pub sections: Vec<TranscriptionSection>,
    pub lang: String,
}

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

pub async fn transcribe<R: Runtime>(handle: &AppHandle<R>, model_name: &str, translate: bool, audio: Vec<f32>) -> Result<TranscriptionResult, Box<dyn std::error::Error + Send + Sync>> {
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
    params.set_token_timestamps(true);
    params.set_max_len(30);
    params.set_split_on_word(true);
    params.set_translate(translate);
    params.set_suppress_non_speech_tokens(true);

    // Create state and run inference
    println!("Transcribing audio...");
    let mut state = ctx.create_state()?;
    state.full(params, &audio)?;

    // Get the number of segments and detected language (returned as Unicode-compatible ISO codes)
    let num_segments = state.full_n_segments()?;
    let detected_lang = state.full_lang_id_from_state().unwrap_or(0);
    let lang_str = match detected_lang {
        0 => "GB",  // English -> United Kingdom
        1 => "CN",  // Chinese -> China
        2 => "DE",  // German -> Germany
        3 => "ES",  // Spanish -> Spain
        4 => "RU",  // Russian -> Russia
        5 => "KR",  // Korean -> South Korea
        6 => "FR",  // French -> France
        7 => "JP",  // Japanese -> Japan
        8 => "PT",  // Portuguese -> Portugal
        9 => "TR",  // Turkish -> Turkey
        10 => "PL", // Polish -> Poland
        11 => "ES", // Catalan -> Spain
        12 => "NL", // Dutch -> Netherlands
        13 => "SA", // Arabic -> Saudi Arabia
        14 => "SE", // Swedish -> Sweden
        15 => "IT", // Italian -> Italy
        16 => "ID", // Indonesian -> Indonesia
        17 => "IN", // Hindi -> India
        18 => "FI", // Finnish -> Finland
        19 => "VN", // Vietnamese -> Vietnam
        20 => "IL", // Hebrew -> Israel
        21 => "UA", // Ukrainian -> Ukraine
        22 => "GR", // Greek -> Greece
        23 => "MY", // Malay -> Malaysia
        24 => "CZ", // Czech -> Czech Republic
        25 => "RO", // Romanian -> Romania
        26 => "DK", // Danish -> Denmark
        27 => "HU", // Hungarian -> Hungary
        28 => "IN", // Tamil -> India
        29 => "NO", // Norwegian -> Norway
        30 => "TH", // Thai -> Thailand
        31 => "PK", // Urdu -> Pakistan
        32 => "HR", // Croatian -> Croatia
        33 => "BG", // Bulgarian -> Bulgaria
        34 => "LT", // Lithuanian -> Lithuania
        35 => "VA", // Latin -> Vatican
        36 => "NZ", // Maori -> New Zealand
        37 => "IN", // Malayalam -> India
        38 => "GB", // Welsh -> United Kingdom
        39 => "SK", // Slovak -> Slovakia
        40 => "IN", // Telugu -> India
        41 => "IR", // Persian -> Iran
        42 => "LV", // Latvian -> Latvia
        43 => "BD", // Bengali -> Bangladesh
        44 => "RS", // Serbian -> Serbia
        45 => "AZ", // Azerbaijani -> Azerbaijan
        46 => "SI", // Slovenian -> Slovenia
        47 => "IN", // Kannada -> India
        48 => "EE", // Estonian -> Estonia
        49 => "MK", // Macedonian -> North Macedonia
        50 => "FR", // Breton -> France
        51 => "ES", // Basque -> Spain
        52 => "IS", // Icelandic -> Iceland
        53 => "AM", // Armenian -> Armenia
        54 => "NP", // Nepali -> Nepal
        55 => "MN", // Mongolian -> Mongolia
        56 => "BA", // Bosnian -> Bosnia and Herzegovina
        57 => "KZ", // Kazakh -> Kazakhstan
        58 => "AL", // Albanian -> Albania
        59 => "TZ", // Swahili -> Tanzania
        60 => "ES", // Galician -> Spain
        61 => "IN", // Marathi -> India
        62 => "IN", // Punjabi -> India
        63 => "LK", // Sinhala -> Sri Lanka
        64 => "KH", // Khmer -> Cambodia
        65 => "ZW", // Shona -> Zimbabwe
        66 => "NG", // Yoruba -> Nigeria
        67 => "SO", // Somali -> Somalia
        68 => "ZA", // Afrikaans -> South Africa
        69 => "FR", // Occitan -> France
        70 => "GE", // Georgian -> Georgia
        71 => "BY", // Belarusian -> Belarus
        72 => "TJ", // Tajik -> Tajikistan
        73 => "PK", // Sindhi -> Pakistan
        74 => "IN", // Gujarati -> India
        75 => "ET", // Amharic -> Ethiopia
        76 => "IL", // Yiddish -> Israel
        77 => "LA", // Lao -> Laos
        78 => "UZ", // Uzbek -> Uzbekistan
        79 => "FO", // Faroese -> Faroe Islands
        80 => "HT", // Haitian Creole -> Haiti
        81 => "AF", // Pashto -> Afghanistan
        82 => "TM", // Turkmen -> Turkmenistan
        83 => "NO", // Nynorsk -> Norway
        84 => "MT", // Maltese -> Malta
        85 => "IN", // Sanskrit -> India
        86 => "LU", // Luxembourgish -> Luxembourg
        87 => "MM", // Myanmar -> Myanmar
        88 => "CN", // Tibetan -> China
        89 => "PH", // Tagalog -> Philippines
        90 => "MG", // Malagasy -> Madagascar
        91 => "IN", // Assamese -> India
        92 => "RU", // Tatar -> Russia
        93 => "US", // Hawaiian -> United States
        94 => "CD", // Lingala -> Democratic Republic of Congo
        95 => "NG", // Hausa -> Nigeria
        96 => "RU", // Bashkir -> Russia
        97 => "ID", // Javanese -> Indonesia
        98 => "ID", // Sundanese -> Indonesia
        99 => "HK", // Cantonese -> Hong Kong
        _ => "auto", // Unknown/auto
    };

    // Collect sections with timestamps
    println!("\n----- Transcription -----");
    let mut sections = Vec::new();
    for i in 0..num_segments {
        let segment = state.full_get_segment_text(i)?;
        let start_time = state.full_get_segment_t0(i)?;
        let _end_time = state.full_get_segment_t1(i)?;

        let start_mins = start_time / 60;
        let start_secs = start_time % 60;
        let _end_mins = _end_time / 60;
        let _end_secs = _end_time % 60;

        println!("[{:02}:{:05.2}-{:02}:{:05.2}] {}", 
            start_mins, start_secs, _end_mins, _end_secs, segment);

        // Skip empty segments, single punctuation, or [BLANK_AUDIO] markers
        let trimmed = segment.trim();
        if !trimmed.is_empty() && 
           !trimmed.eq(",") && 
           !trimmed.eq(".") && 
           !trimmed.eq("[BLANK_AUDIO]") {
            sections.push(TranscriptionSection {
                text: segment,
                at: (start_time as i64) * 10, // Convert to milliseconds (whisper uses centiseconds)
            });
        }
    }
    println!("------------------------");
    
    Ok(TranscriptionResult {
        sections,
        lang: lang_str.to_string(),
    })
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
    // Note: Whisper models use app_local_data_dir (AppData\Local on Windows)
    // while user data uses app_data_dir (AppData\Roaming). This is intentional
    // as large AI models are better suited to Local (cache-like) storage.
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
