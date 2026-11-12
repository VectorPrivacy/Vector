//! Whisper on the desktop client's module: the account's model choice, the download, and a
//! transcription of an attachment off the UI thread.

use vector_media::whisper;

pub const DEFAULT_MODEL: &str = "small";

fn app() -> Result<tauri::AppHandle, String> {
    vector_media::TAURI_APP.get().copied().ok_or_else(|| "Audio engine not started".to_string())
}

fn setting(key: &str) -> Option<String> {
    vector_core::db::settings::get_sql_setting(key.to_string()).ok().flatten().filter(|v| !v.is_empty())
}

/// The account's chosen model and whether it translates to English.
pub fn choice() -> (String, bool) {
    let model = setting("whisper_model_name").unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let translate = setting("whisper_auto_translate").as_deref() == Some("true");
    (model, translate)
}

pub fn auto_transcribe() -> bool {
    setting("whisper_auto_transcribe").as_deref() == Some("true")
}

pub fn downloaded(model: &str) -> bool {
    fixture().is_some() || app().is_ok_and(|a| whisper::is_model_downloaded(&a, model))
}

/// A model as the Voice page lists it.
#[derive(Clone)]
pub struct Model {
    pub name: &'static str,
    pub display: &'static str,
    /// Megabytes on disk, and of memory it needs to run.
    pub size: usize,
    pub ram: usize,
    pub translate: bool,
    pub downloaded: bool,
}

pub fn models() -> Vec<Model> {
    let fixture = fixture().is_some();
    let app = app().ok();
    whisper::MODELS
        .iter()
        .map(|m| Model {
            name: m.name,
            display: m.display_name,
            size: m.size,
            ram: m.ram_required,
            translate: m.supports_translate,
            downloaded: if fixture {
                m.name == DEFAULT_MODEL && std::env::var("VECTOR_TRANSCRIPT").is_ok_and(|v| v != "missing")
            } else {
                app.as_ref().is_some_and(|a| whisper::is_model_downloaded(a, m.name))
            },
        })
        .collect()
}

/// This device's memory in megabytes, which bounds the models it can run.
pub fn memory_mb() -> u64 {
    if fixture().is_some() {
        return 2048;
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory() / (1024 * 1024)
}

pub fn set(key: &'static str, value: impl ToString) {
    let _ = vector_core::db::settings::set_sql_setting(key.to_string(), value.to_string());
}

pub async fn download(model: String) -> Result<(), String> {
    let app = app()?;
    whisper::download_whisper_model(&app, &model).await.map(|_| ()).map_err(|e| e.to_string())
}

pub async fn delete(model: String) -> bool {
    match app() {
        Ok(app) => whisper::delete_whisper_model(app, model).await,
        Err(_) => false,
    }
}

pub fn cancel_download() {
    tauri::async_runtime::spawn(whisper::cancel_whisper_download());
}

/// Transcribe a file, downloading the model first if it isn't here.
pub async fn transcribe(input: vector_media::audio_engine::AudioInput) -> Result<whisper::TranscriptionResult, String> {
    if let Some(r) = fixture() {
        return Ok(r);
    }
    let app = app()?;
    let (model, translate) = choice();
    let audio = tokio::task::spawn_blocking(move || vector_media::audio::decode_for_whisper(&input))
        .await
        .map_err(|e| e.to_string())??;
    whisper::transcribe(&app, &model, translate, audio).await.map_err(|e| e.to_string())
}

/// Dictation: the recording held for the preview, as one line of text.
pub async fn transcribe_recording() -> Result<String, String> {
    let r = match fixture() {
        Some(r) => r,
        None => {
            let app = app()?;
            let (model, _) = choice();
            let audio = vector_media::voice::AudioRecorder::global().pending_for_whisper()?;
            whisper::transcribe(&app, &model, false, audio).await.map_err(|e| e.to_string())?
        }
    };
    Ok(r.sections.iter().map(|s| s.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" "))
}

/// `VECTOR_TRANSCRIPT`: a canned transcript for reference renders, which have no model.
fn fixture() -> Option<whisper::TranscriptionResult> {
    std::env::var_os("VECTOR_TRANSCRIPT")?;
    let section = |at: i64, text: &str| whisper::TranscriptionSection { text: text.into(), at, confidence: 0.9 };
    Some(whisper::TranscriptionResult {
        sections: vec![
            section(0, "Привет, это Ана."),
            section(1800, "Я буду дома около семи,"),
            section(4200, "купи, пожалуйста, хлеб по дороге."),
        ],
        lang: "ru".into(),
        language: "ru".into(),
        confidence: 0.9,
    })
}
