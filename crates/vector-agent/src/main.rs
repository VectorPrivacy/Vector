mod handler;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use rmcp::ServiceExt;
#[cfg(not(unix))]
use rmcp::transport::stdio;
use vector_core::{VectorCore, CoreConfig};

use handler::AgentEventHandler;
use tools::VectorAgent;

/// The MCP stream owns stdout: a stray `println!` anywhere in the process
/// (a dependency's included) would corrupt it. File descriptor 1 is pointed at
/// stderr, and the protocol gets the original stdout to itself.
#[cfg(unix)]
fn claim_stdout() -> std::io::Result<tokio::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: plain descriptor calls on fds 1 and 2, before any other thread
    // writes to them; the duplicate is owned by the returned File alone.
    unsafe {
        let mcp = libc::dup(libc::STDOUT_FILENO);
        if mcp < 0 || libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(tokio::fs::File::from_std(std::fs::File::from_raw_fd(mcp)))
    }
}

#[tokio::main]
async fn main() {
    #[cfg(unix)]
    let mcp_out = claim_stdout().unwrap_or_else(|e| {
        eprintln!("Failed to take stdout for MCP: {}", e);
        std::process::exit(1);
    });

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
    vector_core::db::spawn_bound(async move {
        if let Err(e) = listen_core.listen(Arc::new(event_handler)).await {
            eprintln!("[vector-agent] Listen error: {}", e);
        }
    });

    eprintln!("[vector-agent] MCP server ready (stdio)");

    let agent = VectorAgent::new(core, message_buffer);
    #[cfg(unix)]
    let transport = (tokio::io::stdin(), mcp_out);
    #[cfg(not(unix))]
    let transport = stdio();
    let service = agent.serve(transport).await.unwrap_or_else(|e| {
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
            return PathBuf::from(home).join("Library/Application Support/io.vectorapp/agent");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(data) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(data).join("io.vectorapp/agent");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".local/share/io.vectorapp/agent");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("io.vectorapp/agent");
        }
    }
    PathBuf::from("/tmp/vector-data")
}

#[cfg(test)]
mod spawn_binding_tests {
    /// This agent swaps accounts on demand (`swap_account`), so it carries the
    /// same hazard as the app: a listener or buffer task outliving the swap and
    /// writing the previous account's data into the new one's store.
    #[test]
    fn per_account_tasks_are_spawned_bound_to_their_account() {
        vector_core::spawn_audit::assert_all_spawns_bound(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            &[],
        );
    }
}
