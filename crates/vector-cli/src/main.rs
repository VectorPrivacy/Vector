use std::path::PathBuf;
use std::sync::Arc;
use vector_core::{VectorCore, CoreConfig, SendCallback, SendConfig};
use vector_core::types::Message;

// ============================================================================
// CLI Send Callback — real-time terminal feedback
// ============================================================================

struct CliSendCallback;

impl SendCallback for CliSendCallback {
    fn on_upload_progress(
        &self,
        _pending_id: &str,
        percentage: u8,
        bytes_sent: u64,
    ) -> Result<(), String> {
        print!("\r  [upload] {}% ({} bytes)", percentage, bytes_sent);
        if percentage >= 100 {
            println!();
        }
        Ok(())
    }

    fn on_upload_complete(&self, _chat_id: &str, _pending_id: &str, _att_id: &str, url: &str) {
        println!("  [uploaded] {}", url);
    }

    fn on_sent(&self, _chat_id: &str, _old_id: &str, msg: &Message) {
        println!("  [sent] {}", &msg.id);
    }

    fn on_failed(&self, _chat_id: &str, _old_id: &str, _msg: &Message) {
        eprintln!("  [FAILED]");
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    let data_dir = dirs_or_default();
    std::fs::create_dir_all(&data_dir).ok();

    let core = VectorCore::init(CoreConfig {
        data_dir,
        event_emitter: None,
    }).expect("Failed to initialize Vector Core");

    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        return;
    }

    match args[1].as_str() {
        "--login" | "login" => {
            if args.len() < 3 {
                eprintln!("Usage: vector-cli login <nsec|seed>");
                std::process::exit(1);
            }
            match core.login(&args[2], None).await {
                Ok(result) => {
                    println!("Logged in as {}", result.npub);
                    println!("Encryption: {}", if result.has_encryption { "enabled" } else { "none" });
                    println!("Connecting to relays...");
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    println!("Ready.");
                }
                Err(e) => {
                    eprintln!("Login failed: {}", e);
                    std::process::exit(1);
                }
            }
        }

        "--send" | "send" => {
            if args.len() < 4 {
                eprintln!("Usage: vector-cli send <npub> <message>");
                std::process::exit(1);
            }
            let npub = &args[2];
            let content = args[3..].join(" ");

            auto_login(&core).await;

            println!("Sending DM to {}...", &npub[..20.min(npub.len())]);
            let config = SendConfig { self_send: true, ..Default::default() };
            match vector_core::sending::send_dm(npub, &content, None, &config, Arc::new(CliSendCallback)).await {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Send failed: {}", e);
                    std::process::exit(1);
                }
            }
        }

        "--send-file" | "send-file" => {
            if args.len() < 4 {
                eprintln!("Usage: vector-cli send-file <npub> <filepath>");
                std::process::exit(1);
            }
            let npub = &args[2];
            let filepath = &args[3];

            auto_login(&core).await;

            let path = std::path::Path::new(filepath);
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Failed to read file: {}", e);
                    std::process::exit(1);
                }
            };
            let filename = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            let extension = path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("bin");

            println!("Sending {} to {}...", filename, &npub[..20.min(npub.len())]);
            let config = SendConfig { self_send: true, ..Default::default() };
            match vector_core::sending::send_file_dm(
                npub, Arc::new(bytes), filename, extension, None,
                &config, Arc::new(CliSendCallback),
            ).await {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("File send failed: {}", e);
                    std::process::exit(1);
                }
            }
        }

        "--accounts" | "accounts" => {
            match core.accounts() {
                Ok(accounts) if !accounts.is_empty() => {
                    println!("Accounts:");
                    for (i, acc) in accounts.iter().enumerate() {
                        println!("  {}. {}", i + 1, acc);
                    }
                }
                Ok(_) => println!("No accounts. Use: vector-cli login <nsec>"),
                Err(e) => eprintln!("Error: {}", e),
            }
        }

        "--whoami" | "whoami" => {
            auto_login(&core).await;
            match core.my_npub() {
                Some(npub) => println!("{}", npub),
                None => eprintln!("Not logged in"),
            }
        }

        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_usage();
            std::process::exit(1);
        }
    }

    core.logout().await;
}

async fn auto_login(core: &VectorCore) {
    let accounts = core.accounts().unwrap_or_default();
    if accounts.is_empty() {
        eprintln!("No account found. Run: vector-cli login <nsec>");
        std::process::exit(1);
    }

    let npub = &accounts[0];
    vector_core::db::set_current_account(npub.clone()).ok();
    vector_core::db::init_database(npub).ok();

    let pkey = match vector_core::db::get_pkey() {
        Ok(Some(k)) => k,
        _ => {
            eprintln!("No stored key for {}. Run: vector-cli login <nsec>", npub);
            std::process::exit(1);
        }
    };

    match core.login(&pkey, None).await {
        Ok(_) => {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Err(e) => {
            eprintln!("Auto-login failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    println!("Vector CLI v0.1.0");
    println!();
    println!("Usage:");
    println!("  vector-cli login <nsec|seed>       Login / create account");
    println!("  vector-cli send <npub> <message>    Send a text DM");
    println!("  vector-cli send-file <npub> <path>  Send a file DM");
    println!("  vector-cli accounts                 List stored accounts");
    println!("  vector-cli whoami                   Show current npub");
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
