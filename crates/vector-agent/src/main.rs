mod handler;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use rmcp::{ServiceExt, transport::stdio};
use vector_core::{VectorCore, CoreConfig};

use handler::AgentEventHandler;
use tools::VectorAgent;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let nsec = match std::env::var("VECTOR_NSEC") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("Error: VECTOR_NSEC environment variable is required");
            eprintln!("Usage: VECTOR_NSEC=nsec1... vector-agent");
            std::process::exit(1);
        }
    };

    let data_dir = std::env::var("VECTOR_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_or_default());

    std::fs::create_dir_all(&data_dir).ok();

    let core = VectorCore::init(CoreConfig {
        data_dir,
        event_emitter: None,
    }).unwrap_or_else(|e| {
        eprintln!("Failed to initialize Vector Core: {}", e);
        std::process::exit(1);
    });

    let password = std::env::var("VECTOR_PASSWORD").ok();
    match core.login(&nsec, password.as_deref()).await {
        Ok(result) => {
            eprintln!("[vector-agent] Logged in as {}", result.npub);
        }
        Err(e) => {
            eprintln!("Login failed: {}", e);
            std::process::exit(1);
        }
    }

    // Wait for relay connections
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Start background listener with event handler
    let (event_handler, message_buffer) = AgentEventHandler::new();
    let listen_core = VectorCore;
    tokio::spawn(async move {
        if let Err(e) = listen_core.listen(Arc::new(event_handler)).await {
            eprintln!("[vector-agent] Listen error: {}", e);
        }
    });

    eprintln!("[vector-agent] MCP server ready (stdio)");

    let agent = VectorAgent::new(core, message_buffer);
    let service = agent.serve(stdio()).await.unwrap_or_else(|e| {
        eprintln!("Failed to start MCP server: {}", e);
        std::process::exit(1);
    });

    service.waiting().await.unwrap_or_else(|e| {
        eprintln!("MCP server error: {}", e);
        std::process::exit(1);
    });
}

fn dirs_or_default() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Application Support/io.vectorapp/data");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(data) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(data).join("io.vectorapp/data");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".local/share/io.vectorapp/data");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("io.vectorapp/data");
        }
    }
    PathBuf::from("/tmp/vector-data")
}
