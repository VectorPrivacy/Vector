//! Links from outside: the launch arguments, the platform's open-URL event, and a second launch,
//! which hands its own over to the running one and exits. An empty line only raises the window.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use futures::channel::mpsc::UnboundedSender;

/// The launch arguments that are Vector links.
pub fn from_args() -> Vec<String> {
    std::env::args()
        .skip(1)
        .filter(|a| vector_core::deep_link::parse_deep_link(a).is_some())
        .collect()
}

/// Becomes the one instance for `data_dir`, or forwards `urls` to the one running and says so.
pub fn claim(data_dir: &Path, urls: &[String], tx: UnboundedSender<String>) -> bool {
    let _ = std::fs::create_dir_all(data_dir);
    if forward(data_dir, urls) {
        return false;
    }
    listen(data_dir, tx);
    true
}

fn send(mut stream: impl Write, urls: &[String]) -> bool {
    let mut body = String::new();
    for u in urls {
        body.push_str(u.trim());
        body.push('\n');
    }
    if urls.is_empty() {
        body.push('\n');
    }
    stream.write_all(body.as_bytes()).is_ok()
}

fn serve(stream: impl std::io::Read, tx: &UnboundedSender<String>) {
    for line in BufReader::new(stream)
        .lines()
        .map_while(Result::ok)
        .take(16)
    {
        if tx.unbounded_send(line).is_err() {
            return;
        }
    }
}

#[cfg(unix)]
fn forward(data_dir: &Path, urls: &[String]) -> bool {
    std::os::unix::net::UnixStream::connect(data_dir.join("gpui.sock")).is_ok_and(|s| send(s, urls))
}

#[cfg(unix)]
fn listen(data_dir: &Path, tx: UnboundedSender<String>) {
    use std::os::unix::fs::PermissionsExt;
    let path = data_dir.join("gpui.sock");
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = std::os::unix::net::UnixListener::bind(&path) else {
        return;
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            serve(stream, &tx);
        }
    });
}

#[cfg(windows)]
fn forward(data_dir: &Path, urls: &[String]) -> bool {
    let Some(port) = std::fs::read_to_string(data_dir.join("gpui.port"))
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
    else {
        return false;
    };
    std::net::TcpStream::connect_timeout(
        &(std::net::Ipv4Addr::LOCALHOST, port).into(),
        std::time::Duration::from_millis(300),
    )
    .is_ok_and(|mut s| s.write_all(b"vector-gpui\n").is_ok() && send(s, urls))
}

#[cfg(windows)]
fn listen(data_dir: &Path, tx: UnboundedSender<String>) {
    let Ok(listener) = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)) else {
        return;
    };
    let Ok(addr) = listener.local_addr() else {
        return;
    };
    let _ = std::fs::write(data_dir.join("gpui.port"), addr.port().to_string());
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut lines = BufReader::new(stream);
            let mut magic = String::new();
            if lines.read_line(&mut magic).is_ok() && magic.trim() == "vector-gpui" {
                serve(lines, &tx);
            }
        }
    });
}

#[cfg(not(any(unix, windows)))]
fn forward(_: &Path, _: &[String]) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
fn listen(_: &Path, _: UnboundedSender<String>) {}

#[cfg(test)]
mod tests {
    #[test]
    fn a_second_launch_hands_its_link_over() {
        let dir = std::env::temp_dir().join(format!("vector-gpui-dl-{}", std::process::id()));
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        assert!(super::claim(&dir, &[], tx));
        let (tx2, _) = futures::channel::mpsc::unbounded();
        assert!(!super::claim(
            &dir,
            &["vector://profile/npub1x".into()],
            tx2
        ));
        let got = futures::executor::block_on(futures::StreamExt::next(&mut rx));
        assert_eq!(got.as_deref(), Some("vector://profile/npub1x"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
