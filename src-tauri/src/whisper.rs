use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use futures_util::StreamExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use tauri::{AppHandle, Runtime, Manager, Emitter};
use serde::Serialize;

/// Cancellation flag for in-progress model downloads
static DOWNLOAD_CANCELLED: AtomicBool = AtomicBool::new(false);

/// Cached WhisperContext — avoids reloading model weights + GPU init on every transcription.
/// WhisperContext internally uses Arc<WhisperInnerContext>, and both WhisperContext and
/// WhisperState implement Send + Sync, so this is safe.
struct CachedWhisperCtx {
    model_path: String,
    ctx: WhisperContext,
    /// Desktop keeps the state as well: building one reserves four GPU graphs (~20 ms on Metal).
    /// Not after a beam retry, which grows its KV cache sevenfold; and Android builds one per
    /// transcription, so an idle phone holds no compute buffers.
    state: Option<whisper_rs::WhisperState>,
}

static WHISPER_CTX_CACHE: Mutex<Option<CachedWhisperCtx>> = Mutex::new(None);

/// Weights and GPU buffers stay resident only through a burst of use.
const WHISPER_CTX_TTL: std::time::Duration = std::time::Duration::from_secs(3 * 60);

/// Millis since `WHISPER_EPOCH` when a transcription last released the context.
static WHISPER_LAST_USED_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WHISPER_EPOCH: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);

/// Stamps the last use when dropped, which is while the cache lock is still held.
struct StampLastUse;
impl Drop for StampLastUse {
    fn drop(&mut self) {
        WHISPER_LAST_USED_MS.store(WHISPER_EPOCH.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
}

/// Drop the cached context once it has sat idle past the TTL. Never waits on the
/// lock: a held lock means a transcription is running.
pub fn check_ctx_ttl() -> bool {
    let Ok(mut cache) = WHISPER_CTX_CACHE.try_lock() else { return false };
    if cache.is_none() {
        return false;
    }
    let idle_ms = (WHISPER_EPOCH.elapsed().as_millis() as u64)
        .saturating_sub(WHISPER_LAST_USED_MS.load(Ordering::Relaxed));
    if idle_ms < WHISPER_CTX_TTL.as_millis() as u64 {
        return false;
    }
    *cache = None;
    println!("[Maintenance] Whisper context idle {}s - unloaded", idle_ms / 1000);
    true
}

#[derive(Serialize, Clone)]
pub struct TranscriptionSection {
    pub text: String,
    pub at: i64, // millisecond timestamp
    pub confidence: f32, // average token probability (0.0-1.0)
}

#[derive(Serialize, Clone)]
pub struct TranscriptionResult {
    pub sections: Vec<TranscriptionSection>,
    /// A country for the flag: several languages share one, so it can't name the language.
    pub lang: String,
    /// The detected language itself (ISO 639-1, e.g. "ru"), which can be named.
    pub language: String,
    pub confidence: f32, // overall average confidence across all sections
}

/// Whisper model information
#[derive(Serialize, Clone)]
pub struct WhisperModel {
    /// Model name (used in API requests and UI identification)
    pub name: &'static str,
    /// Display name (used in the UI as a simplified name)
    pub display_name: &'static str,
    /// Quantized GGML filename for this platform
    #[serde(skip)]
    pub filename: &'static str,
    /// Primary download URL for this model
    #[serde(skip)]
    pub url: &'static str,
    /// Whether this model supports ACFT (dynamic audio_ctx for faster short-audio inference)
    #[serde(skip)]
    pub acft: bool,
    /// Approximate file size in MB for this platform's quantized model
    pub size: usize,
    /// Minimum system RAM required in MB (model + inference buffers + whisper overhead)
    pub ram_required: usize,
    /// Whether this model produces reliable translations (tiny/base are too small)
    pub supports_translate: bool,
}

/// Android: FUTO ACFT models (Q8_0) with dynamic audio_ctx support for 3-5x faster short-audio inference
#[cfg(target_os = "android")]
pub const MODELS: [WhisperModel; 2] = [
    WhisperModel { name: "base", display_name: "Good Quality - Fast", filename: "base_acft_q8_0.bin", url: "https://voiceinput.futo.org/VoiceInput/base_acft_q8_0.bin", acft: true, size: 74, ram_required: 400, supports_translate: false },
    WhisperModel { name: "small", display_name: "Best Quality - Moderate", filename: "small_acft_q8_0.bin", url: "https://voiceinput.futo.org/VoiceInput/small_acft_q8_0.bin", acft: true, size: 244, ram_required: 770, supports_translate: true },
];

/// Desktop: stock Whisper Q8_0 models with GPU acceleration (Metal/Vulkan)
#[cfg(not(target_os = "android"))]
pub const MODELS: [WhisperModel; 5] = [
    WhisperModel { name: "tiny", display_name: "Lowest Quality - Fastest", filename: "ggml-tiny-q8_0.bin", url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny-q8_0.bin", acft: false, size: 44, ram_required: 310, supports_translate: false },
    WhisperModel { name: "base", display_name: "Base Quality - Fast", filename: "ggml-base-q8_0.bin", url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base-q8_0.bin", acft: false, size: 82, ram_required: 360, supports_translate: false },
    WhisperModel { name: "small", display_name: "Good Quality - Fast", filename: "ggml-small-q8_0.bin", url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q8_0.bin", acft: false, size: 264, ram_required: 620, supports_translate: true },
    WhisperModel { name: "medium", display_name: "High Quality - Moderate", filename: "ggml-medium-q8_0.bin", url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium-q8_0.bin", acft: false, size: 823, ram_required: 1320, supports_translate: true },
    WhisperModel { name: "large-v3", display_name: "Highest Quality - Slowest", filename: "ggml-large-v3-q5_0.bin", url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin", acft: false, size: 1080, ram_required: 2400, supports_translate: true },
];

/// Threads for whisper.cpp. Phones skip their efficiency cluster: ggml splits most work evenly,
/// so every op waits on the slowest core.
fn whisper_threads() -> i32 {
    let all = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
    #[cfg(target_os = "android")]
    {
        // cpu_capacity is the kernel's own performance rating; cpufreq where it is missing
        let read = |file: &str| -> Option<Vec<u64>> {
            (0..all)
                .map(|cpu| std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/{file}")).ok()?.trim().parse().ok())
                .collect()
        };
        if let Some(n) = read("cpu_capacity").and_then(|c| performance_cores(&c, 0.6))
            .or_else(|| read("cpufreq/cpuinfo_max_freq").and_then(|f| performance_cores(&f, 0.8)))
        {
            return n as i32;
        }
    }
    all
}

/// The cores rated above the slowest, when the slowest are below `ratio` of the fastest; None
/// for a phone with no efficiency cores.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn performance_cores(ratings: &[u64], ratio: f64) -> Option<usize> {
    let fastest = *ratings.iter().max()?;
    let slowest = *ratings.iter().min()?;
    if (slowest as f64) >= fastest as f64 * ratio {
        return None;
    }
    Some(ratings.iter().filter(|&&r| r > slowest).count())
}

/// ggml-cpu is built for x86-64-v3 (see whisper-rs-sys): a CPU without those instructions gets an
/// error here rather than an illegal-instruction crash.
fn whisper_cpu_supported() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(all(target_arch = "x86_64", not(target_os = "macos")))]
    {
        let ok = std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("fma")
            && std::arch::is_x86_feature_detected!("f16c")
            && std::arch::is_x86_feature_detected!("bmi2");
        if !ok {
            return Err("Voice transcription needs a processor with AVX2".into());
        }
    }
    Ok(())
}

/// Detect repetition loops in transcription output.
/// Two-layer check:
///   1. Word-level: repeated 3-word trigrams (catches "het verbanden van het verbanden van...")
///   2. Char-level: repeated 4-char substrings (catches "waardwaardwaardwaard..." with no spaces)
fn has_repetition_loop(sections: &[TranscriptionSection]) -> bool {
    use std::collections::HashMap;
    let full_text: String = sections.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");

    // Check 1: word-level trigram repetition
    // Requires BOTH a minimum count AND that the repeated phrase dominates the text.
    // This avoids false positives on rhetorical repetition (anaphora) where a phrase
    // appears 4-5 times in otherwise varied text (e.g. political speeches).
    let words: Vec<&str> = full_text.split_whitespace().collect();
    if words.len() >= 12 {
        let mut counts: HashMap<(&str, &str, &str), u32> = HashMap::new();
        for w in words.windows(3) {
            *counts.entry((w[0], w[1], w[2])).or_insert(0) += 1;
        }
        let max_count = counts.values().max().copied().unwrap_or(0);
        // ratio = fraction of all words covered by the most-repeated trigram
        // > 0.4 means 40%+ of the text is one phrase on repeat = genuine loop
        // Rhetorical speech (5 reps in 55 words = 0.27) stays safely below
        let ratio = (max_count * 3) as f32 / words.len() as f32;
        if max_count >= 4 && ratio > 0.4 {
            return true;
        }
    }

    // Check 2: character-level substring repetition
    // Catches single-token loops like "waardwaardwaard" where whisper fuses
    // repeated subwords into one giant spaceless token.
    let chars: Vec<char> = full_text.chars().collect();
    if chars.len() >= 32 {
        let mut char_counts: HashMap<&[char], u32> = HashMap::new();
        for w in chars.windows(4) {
            *char_counts.entry(w).or_insert(0) += 1;
        }
        let max_count = *char_counts.values().max().unwrap_or(&0);
        // A 4-char window appears in (len - 3) positions; if any single pattern
        // accounts for >30% of all windows, it's a hallucination loop.
        let total_windows = chars.len().saturating_sub(3) as f32;
        if max_count >= 8 && (max_count as f32 / total_windows) > 0.3 {
            return true;
        }
    }

    false
}

pub async fn transcribe<R: Runtime>(handle: &AppHandle<R>, model_name: &str, translate: bool, audio: Vec<f32>) -> Result<TranscriptionResult, Box<dyn std::error::Error + Send + Sync>> {
    use std::time::Instant;
    let t_total = Instant::now();

    whisper_cpu_supported()?;

    let model_def = MODELS.iter().find(|m| m.name == model_name);
    // Safety net: low-quality models (tiny/base) produce unreliable translations
    let translate = translate && model_def.is_some_and(|m| m.supports_translate);
    let audio_duration_ms = (audio.len() as f64 / 16.0) as u64; // 16kHz -> ms

    // --- Phase 1: Model path resolution (downloads if needed) ---
    let t0 = Instant::now();
    let model_path = download_whisper_model(handle, model_name).await?;
    let t_model_load = t0.elapsed();

    // --- Phase 2: Context acquisition (cached or fresh) ---
    // Lock the cache for the entire synchronous inference block.
    // This is safe: no await points while held, and concurrent transcriptions
    // would contend for GPU/CPU anyway.
    let mut cache_guard = WHISPER_CTX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let _stamp = StampLastUse;

    let t0 = Instant::now();
    let cache_hit = matches!(cache_guard.as_ref(), Some(cached) if cached.model_path == model_path);

    if !cache_hit {
        // Free the old model first: two resident at once can fail a GPU's allocation.
        *cache_guard = None;

        // Different model or first run — create new context
        let mut ctx_params = WhisperContextParameters::default();
        ctx_params.flash_attn(true);

        // Android: CPU-only (Vulkan GPU hangs freeze the entire device via compositor stall)
        // Desktop: try GPU first, fall back to CPU
        let use_gpu = !cfg!(target_os = "android");
        ctx_params.use_gpu(use_gpu);

        let ctx = if use_gpu {
            match WhisperContext::new_with_params(&model_path, ctx_params) {
                Ok(ctx) => {
                    println!("[Whisper] GPU context initialized");
                    ctx
                }
                Err(gpu_err) => {
                    println!("[Whisper] GPU init failed ({}), falling back to CPU", gpu_err);
                    let mut cpu_params = WhisperContextParameters::default();
                    cpu_params.flash_attn(true);
                    cpu_params.use_gpu(false);
                    WhisperContext::new_with_params(&model_path, cpu_params)?
                }
            }
        } else {
            println!("[Whisper] CPU context initialized (GPU disabled on this platform)");
            WhisperContext::new_with_params(&model_path, ctx_params)?
        };

        *cache_guard = Some(CachedWhisperCtx {
            model_path: model_path.clone(),
            ctx,
            state: None,
        });
    }
    let t_ctx_init = t0.elapsed();

    let cached = cache_guard.as_mut().unwrap();

    // --- Phase 3: Parameter setup ---
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_realtime(false);
    params.set_print_progress(false);
    params.set_print_timestamps(true);
    params.set_language(Some("auto"));
    params.set_token_timestamps(true);
    params.set_max_len(30);
    params.set_split_on_word(true);
    params.set_translate(translate);
    params.set_suppress_nst(true);
    params.set_no_context(true);
    // Skip segmentation overhead for very short audio (under 5s) where
    // there's only one meaningful segment — longer clips need segments
    // for the frontend's "active segment" highlight during playback
    if audio.len() < 16000 * 5 {
        params.set_single_segment(true);
    }

    let n_threads = whisper_threads();
    params.set_n_threads(n_threads);

    // ACFT models support dynamic audio_ctx: encoder processes only actual audio
    // frames instead of the full 30s window, giving 3-5x speedup on short audio.
    // suppress_blank must be disabled for ACFT — truncated audio_ctx can produce
    // legitimate blanks, and suppressing them causes decoder hallucination loops.
    if model_def.is_some_and(|m| m.acft) {
        let audio_ctx = std::cmp::min(1500, (audio.len() as f64 / 320.0).ceil() as i32 + 32);
        params.set_audio_ctx(audio_ctx);
        params.set_suppress_blank(false);
    }

    // --- Phase 4: State (kept on desktop, else built from the cached context) ---
    let t0 = Instant::now();
    let mut state = match cached.state.take() {
        Some(state) => state,
        None => cached.ctx.create_state()?,
    };
    let t_state_create = t0.elapsed();

    // Minimum confidence threshold — below this, retry with different strategy
    const MIN_CONFIDENCE: f32 = 0.40;

    // Helper: extract results from a completed whisper state
    let extract_results = |state: &whisper_rs::WhisperState| -> (Vec<TranscriptionSection>, f32, &'static str, &'static str) {
        let num_segments = state.full_n_segments();
        let detected_lang = state.full_lang_id_from_state();
        let language = whisper_rs::get_lang_str(detected_lang).unwrap_or("");
        let lang_str = match detected_lang {
            0 => "GB",  1 => "CN",  2 => "DE",  3 => "ES",  4 => "RU",
            5 => "KR",  6 => "FR",  7 => "JP",  8 => "PT",  9 => "TR",
            10 => "PL", 11 => "ES", 12 => "NL", 13 => "SA", 14 => "SE",
            15 => "IT", 16 => "ID", 17 => "IN", 18 => "FI", 19 => "VN",
            20 => "IL", 21 => "UA", 22 => "GR", 23 => "MY", 24 => "CZ",
            25 => "RO", 26 => "DK", 27 => "HU", 28 => "IN", 29 => "NO",
            30 => "TH", 31 => "PK", 32 => "HR", 33 => "BG", 34 => "LT",
            35 => "VA", 36 => "NZ", 37 => "IN", 38 => "GB", 39 => "SK",
            40 => "IN", 41 => "IR", 42 => "LV", 43 => "BD", 44 => "RS",
            45 => "AZ", 46 => "SI", 47 => "IN", 48 => "EE", 49 => "MK",
            50 => "FR", 51 => "ES", 52 => "IS", 53 => "AM", 54 => "NP",
            55 => "MN", 56 => "BA", 57 => "KZ", 58 => "AL", 59 => "TZ",
            60 => "ES", 61 => "IN", 62 => "IN", 63 => "LK", 64 => "KH",
            65 => "ZW", 66 => "NG", 67 => "SO", 68 => "ZA", 69 => "FR",
            70 => "GE", 71 => "BY", 72 => "TJ", 73 => "PK", 74 => "IN",
            75 => "ET", 76 => "IL", 77 => "LA", 78 => "UZ", 79 => "FO",
            80 => "HT", 81 => "AF", 82 => "TM", 83 => "NO", 84 => "MT",
            85 => "IN", 86 => "LU", 87 => "MM", 88 => "CN", 89 => "PH",
            90 => "MG", 91 => "IN", 92 => "RU", 93 => "US", 94 => "CD",
            95 => "NG", 96 => "RU", 97 => "ID", 98 => "ID", 99 => "HK",
            _ => "auto",
        };

        let mut sections = Vec::new();
        for i in 0..num_segments {
            let seg = match state.get_segment(i) {
                Some(s) => s,
                None => continue,
            };
            let segment = seg.to_str_lossy().unwrap_or_default().to_string();
            let start_time = seg.start_timestamp();

            let n_tokens = seg.n_tokens();
            let avg_prob = if n_tokens > 0 {
                let sum: f32 = (0..n_tokens)
                    .filter_map(|t| seg.get_token(t))
                    .map(|tok| tok.token_probability())
                    .sum();
                sum / n_tokens as f32
            } else {
                0.0
            };

            let trimmed = segment.trim();
            if !trimmed.is_empty() &&
               !trimmed.eq(",") &&
               !trimmed.eq(".") &&
               !trimmed.eq("[BLANK_AUDIO]") {
                sections.push(TranscriptionSection {
                    text: segment,
                    at: start_time * 10,
                    confidence: avg_prob,
                });
            }
        }

        let mut overall = if sections.is_empty() {
            0.0
        } else {
            sections.iter().map(|s| s.confidence).sum::<f32>() / sections.len() as f32
        };

        // Repetition loops can have HIGH token probabilities (the model is
        // "confidently" repeating itself). Override confidence to zero so
        // the retry logic kicks in with beam search.
        if has_repetition_loop(&sections) {
            println!("[Whisper]   repetition loop detected — overriding confidence to 0%");
            overall = 0.0;
        }

        (sections, overall, lang_str, language)
    };

    // --- Phase 5: Inference with confidence-gated retry ---
    let t0 = Instant::now();
    state.full(params, &audio)?;
    let t_first_pass = t0.elapsed();

    let (mut sections, mut overall_confidence, mut lang_str, mut language) = extract_results(&state);
    let mut t_inference = t_first_pass;
    let mut retries_used = 0;

    // Retry if confidence is too low (includes repetition-detected = 0%)
    if overall_confidence < MIN_CONFIDENCE && !sections.is_empty() {
        // Retry 1: beam search (explores multiple hypotheses, breaks repetition loops)
        // Retry 2: beam search + elevated temperature (more sampling diversity)
        let retry_configs: [(i32, f32); 2] = [(5, 0.0), (5, 0.6)];

        for &(beam_size, temp) in &retry_configs {
            let strategy_name = if temp > 0.0 {
                format!("beam({})+temp={:.1}", beam_size, temp)
            } else {
                format!("beam({})", beam_size)
            };
            println!("[Whisper]   pass {} conf={:.1}% — retrying with {}",
                retries_used + 1, overall_confidence * 100.0, strategy_name);

            let mut retry_params = FullParams::new(
                SamplingStrategy::BeamSearch { beam_size, patience: 1.0 }
            );
            retry_params.set_print_realtime(false);
            retry_params.set_print_progress(false);
            retry_params.set_print_timestamps(true);
            retry_params.set_language(Some("auto"));
            retry_params.set_token_timestamps(true);
            retry_params.set_max_len(30);
            retry_params.set_split_on_word(true);
            retry_params.set_translate(translate);
            retry_params.set_suppress_nst(true);
            retry_params.set_no_context(true);
            retry_params.set_n_threads(n_threads);
            if temp > 0.0 {
                retry_params.set_temperature(temp);
            }
            retry_params.set_temperature_inc(0.0); // no further internal retries
            if audio.len() < 16000 * 5 {
                retry_params.set_single_segment(true);
            }
            if model_def.is_some_and(|m| m.acft) {
                let audio_ctx = std::cmp::min(1500, (audio.len() as f64 / 320.0).ceil() as i32 + 32);
                retry_params.set_audio_ctx(audio_ctx);
                retry_params.set_suppress_blank(false);
            }

            // The same state and samples: whisper.cpp keeps the spectrogram (and, up to 30 s, the
            // encoding), so a retry mostly decodes again.
            let t_retry = Instant::now();
            let retry = state.full(retry_params, &audio);
            let retry_elapsed = t_retry.elapsed();
            t_inference += retry_elapsed;
            retries_used += 1;
            if let Err(e) = retry {
                // a beam search can fail to grow its cache; what the earlier pass found still stands
                println!("[Whisper]   {} failed ({}), keeping the earlier result", strategy_name, e);
                break;
            }

            let (new_sections, new_confidence, new_lang, new_language) = extract_results(&state);

            // Keep the retry result if it improved confidence
            if new_confidence > overall_confidence {
                sections = new_sections;
                overall_confidence = new_confidence;
                lang_str = new_lang;
                language = new_language;
            }

            // Good enough — stop retrying
            if overall_confidence >= MIN_CONFIDENCE {
                break;
            }
        }
    }

    if !cfg!(target_os = "android") && retries_used == 0 {
        cached.state = Some(state);
    }

    // Release the cache lock — inference is done
    drop(cache_guard);

    // --- Timing summary ---
    let total_time = t_total.elapsed();
    let rtf = if audio_duration_ms > 0 {
        total_time.as_millis() as f64 / audio_duration_ms as f64
    } else {
        0.0
    };

    println!("[Whisper] ---- {:.1}s audio | model={} | {} threads ----", audio_duration_ms as f64 / 1000.0, model_name, n_threads);
    println!("[Whisper]   model path:    {} ({})", model_name, if cache_hit { "cached" } else { "cold load" });
    println!("[Whisper]   model resolve: {:>10?}", t_model_load);
    println!("[Whisper]   ctx init:      {:>10?}  ({})", t_ctx_init, if cache_hit { "cache hit" } else { "new context" });
    println!("[Whisper]   state create:  {:>10?}", t_state_create);
    println!("[Whisper]   inference:     {:>10?}  ({} retries)", t_inference, retries_used);
    println!("[Whisper]   TOTAL:         {:>10?}  (RTF={:.2}x)", total_time, rtf);
    println!("[Whisper]   confidence: {:.1}% | lang: {}", overall_confidence * 100.0, lang_str);
    println!("[Whisper] ----------------------------------------");

    Ok(TranscriptionResult {
        sections,
        lang: lang_str.to_string(),
        language: language.to_string(),
        confidence: overall_confidence,
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
    let model = match MODELS.iter().find(|m| m.name == model_name) {
        Some(m) => m,
        None => return false,
    };
    // Note: Whisper models use app_local_data_dir (AppData\Local on Windows)
    // while user data uses app_data_dir (AppData\Roaming). This is intentional
    // as large AI models are better suited to Local (cache-like) storage.
    let models_dir = match handle.path().app_local_data_dir() {
        Ok(dir) => dir.join("whisper"),
        Err(_) => return false,
    };
    models_dir.join(model.filename).exists()
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
    let model = match MODELS.iter().find(|m| m.name == model_name.as_str()) {
        Some(m) => m,
        None => return false,
    };
    let models_dir = match handle.path().app_local_data_dir() {
        Ok(dir) => dir.join("whisper"),
        Err(_) => return false,
    };
    let model_path = models_dir.join(model.filename);

    if model_path.exists() {
        // Evict cached context if it references this model (frees GPU/RAM before file delete)
        {
            let mut cache = WHISPER_CTX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.as_ref() {
                if cached.model_path == model_path.to_string_lossy() {
                    *cache = None;
                    println!("Evicted cached Whisper context for: {}", model_name);
                }
            }
        }

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
        println!("Model not found: {}", model_path.display());
        false
    }
}

/// Cancel an in-progress model download
#[tauri::command]
pub async fn cancel_whisper_download() {
    DOWNLOAD_CANCELLED.store(true, Ordering::SeqCst);
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
    let model_def = MODELS
        .iter()
        .find(|m| m.name == model_name)
        .ok_or_else(|| format!("Unknown model: {}", model_name))?;

    let models_dir = handle.path().app_local_data_dir()
        .map_err(|e| format!("Failed to resolve data directory: {e}"))?
        .join("whisper");
    std::fs::create_dir_all(&models_dir)?;

    let model_filename = model_def.filename;
    let model_path = models_dir.join(model_filename);

    if model_path.exists() {
        println!("Using cached model: {}", model_path.display());
        return Ok(model_path.to_string_lossy().to_string());
    }

    let model_size = model_def.size;
    println!("Downloading {} model ({}, ~{}MB), please wait...", model_name, model_filename, model_size);
    
    // Bounded by progress, not a deadline: a 1 GB model over an anonymity network takes hours,
    // and a network switch resumes it from the bytes on disk.
    let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    
    // Use the model's configured URL (FUTO CDN for ACFT models, HuggingFace for stock models)
    let urls = [
        model_def.url.to_string(),
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
            return Err(Box::new(std::io::Error::other(
                error_msg)));
        }
    };
    
    // Get content length
    let total_size = response.content_length().unwrap_or(0);
    let mut response = response;

    // Bytes land beside the model and take its name only once whole: an existing model file is
    // read as complete.
    let part_path = model_path.with_extension("part");
    let mut file = std::fs::File::create(&part_path)?;
    let mut downloaded: u64 = 0;
    let mut network_waits = 0u8;

    // Reset cancellation flag before starting
    DOWNLOAD_CANCELLED.store(false, Ordering::SeqCst);

    // Progress tracking
    let start_time = std::time::Instant::now();
    let mut last_percent: u64 = 0;

    // Stream chunks and write to file
    println!("Starting to download chunks...");
    'resume: loop {
        let mut stream = response.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            // Check for cancellation
            if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
                DOWNLOAD_CANCELLED.store(false, Ordering::SeqCst);
                drop(file);
                let _ = std::fs::remove_file(&part_path);
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Interrupted, "Download cancelled")));
            }

            match chunk_result {
                Ok(chunk) => {
                    std::io::Write::write_all(&mut file, &chunk)?;
                    downloaded += chunk.len() as u64;

                    // Emit progress on every percentage change
                    if let Some(percent) = (downloaded * 100).checked_div(total_size) {
                        if percent != last_percent {
                            last_percent = percent;
                            let elapsed = start_time.elapsed().as_secs_f64();
                            let speed_bps = if elapsed > 0.0 { downloaded as f64 / elapsed } else { 0.0 };

                            let _ = handle.emit("whisper_download_progress", serde_json::json!({
                                "progress": percent,
                                "downloaded_bytes": downloaded,
                                "total_bytes": total_size,
                                "speed_bps": speed_bps as u64
                            }));

                            print!("\rDownloading: {}% ({:.1}/{:.1} MB)",
                                   percent,
                                   downloaded as f64 / (1024.0 * 1024.0),
                                   total_size as f64 / (1024.0 * 1024.0));
                            io::stdout().flush()?;
                        }
                    }
                },
                Err(e) => {
                    println!("\nError downloading chunk: {}", e);
                    // A network switch: wait for the new network, then ask for the rest.
                    if e.is_transient() && network_waits < 8 && vector_core::net::wait_after(&e).await.is_ok() {
                        network_waits += 1;
                        let resumed = client
                            .get(&urls[0])
                            .header(reqwest::header::RANGE, format!("bytes={downloaded}-"))
                            .send()
                            .await;
                        if let Ok(r) = resumed {
                            if r.status().as_u16() == 206 {
                                response = r;
                                continue 'resume;
                            }
                        }
                    }
                    drop(file);
                    let _ = std::fs::remove_file(&part_path);
                    return Err(Box::new(std::io::Error::other(format!("Failed to download chunk: {}", e))));
                }
            }
        }
        break;
    }
    drop(file);
    std::fs::rename(&part_path, &model_path)?;

    println!("\nModel downloaded to: {}", model_path.display());

    Ok(model_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::performance_cores;

    #[test]
    fn efficiency_clusters_are_left_out() {
        // capacity: Snapdragon 695, 2x A78 + 6x A55, whose clocks (2.2 / 1.7 GHz) are close
        assert_eq!(performance_cores(&[1024, 1024, 439, 439, 439, 439, 439, 439], 0.6), Some(2));
        // capacity: 1x X1 + 3x A78 + 4x A55
        assert_eq!(performance_cores(&[1024, 870, 870, 870, 325, 325, 325, 325], 0.6), Some(4));
        // frequency: 4x A76 at 2.4 GHz + 4x A55 at 1.8 GHz
        assert_eq!(performance_cores(&[2400000, 2400000, 2400000, 2400000, 1800000, 1800000, 1800000, 1800000], 0.8), Some(4));
    }

    #[test]
    fn all_big_or_uniform_keeps_every_core() {
        // capacity: Snapdragon 8 Elite, 2x 4.32 GHz + 6x 3.53 GHz Oryon
        assert_eq!(performance_cores(&[1024, 1024, 836, 836, 836, 836, 836, 836], 0.6), None);
        assert_eq!(performance_cores(&[1024; 8], 0.6), None);
        assert_eq!(performance_cores(&[], 0.6), None);
    }
}
