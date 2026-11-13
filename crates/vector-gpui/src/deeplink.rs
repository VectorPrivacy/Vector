//! Links from outside: the launch arguments, the platform's open-URL event, and a second launch,
//! which hands its own over to the running one and exits. An empty line only raises the window.
//! On the default data folder the instance is the Tauri client's own (tauri-plugin-single-instance,
//! identifier `io.vectorapp`), so the two clients never run on one set of databases at once.

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
    if data_dir == crate::backend::default_data_dir() {
        if let Some(own) = shared::claim(tx.clone()) {
            return own;
        }
    }
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

/// Another launch's whole argv: its Vector links, or a bare raise.
fn deliver(argv: impl IntoIterator<Item = String>, tx: &UnboundedSender<String>) {
    let links: Vec<String> = argv
        .into_iter()
        .skip(1)
        .filter(|a| vector_core::deep_link::parse_deep_link(a).is_some())
        .collect();
    if links.is_empty() {
        let _ = tx.unbounded_send(String::new());
    }
    for l in links {
        let _ = tx.unbounded_send(l);
    }
}

fn cwd() -> String {
    std::env::current_dir().unwrap_or_default().to_string_lossy().into_owned()
}

/// tauri-plugin-single-instance's wire, per OS. `Some(false)`: forwarded to the running client.
/// `None`: the mechanism is unavailable, so fall back to the folder's own socket.
#[cfg(target_os = "linux")]
mod shared {
    use super::UnboundedSender;
    use std::sync::OnceLock;

    const NAME: &str = "io.vectorapp.SingleInstance";
    const PATH: &str = "/io/vectorapp/SingleInstance";
    const IFACE: &str = "org.SingleInstance.DBus";
    static BUS: OnceLock<zbus::blocking::Connection> = OnceLock::new();

    struct Instance(UnboundedSender<String>);

    #[zbus::interface(name = "org.SingleInstance.DBus")]
    impl Instance {
        fn execute_callback(&mut self, argv: Vec<String>, _cwd: String) {
            super::deliver(argv, &self.0);
        }
    }

    pub fn claim(tx: UnboundedSender<String>) -> Option<bool> {
        let built = zbus::blocking::connection::Builder::session()
            .and_then(|b| b.name(NAME))
            .map(|b| b.replace_existing_names(false).allow_name_replacements(false))
            .and_then(|b| b.serve_at(PATH, Instance(tx)))
            .and_then(|b| b.build());
        match built {
            Ok(conn) => {
                let _ = BUS.set(conn);
                Some(true)
            }
            Err(zbus::Error::NameTaken) => {
                let argv: Vec<String> = std::env::args().collect();
                let sent = zbus::blocking::Connection::session().and_then(|c| {
                    c.call_method(Some(NAME), PATH, Some(IFACE), "ExecuteCallback", &(argv, super::cwd()))
                });
                if let Err(e) = sent {
                    vector_core::log_warn!("[Instance] couldn't reach the running Vector: {e}");
                }
                Some(false)
            }
            Err(e) => {
                vector_core::log_warn!("[Instance] no session bus: {e}");
                None
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod shared {
    use super::UnboundedSender;
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    const SOCKET: &str = "/tmp/io_vectorapp_si.sock";

    pub fn claim(tx: UnboundedSender<String>) -> Option<bool> {
        match UnixStream::connect(SOCKET) {
            Ok(mut s) => {
                let argv: Vec<String> = std::env::args().collect();
                let _ = s.write_all(format!("{}\0\0{}", super::cwd(), argv.join("\0")).as_bytes());
                Some(false)
            }
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
                let _ = std::fs::remove_file(SOCKET);
                let listener = UnixListener::bind(SOCKET).ok()?;
                // spawn-detached: process-lifetime listener, no account state.
                std::thread::spawn(move || {
                    for mut stream in listener.incoming().map_while(Result::ok) {
                        let mut s = String::new();
                        if stream.read_to_string(&mut s).is_ok() {
                            let args = s.split_once("\0\0").map(|(_, a)| a).unwrap_or_default();
                            super::deliver(args.split('\0').map(String::from), &tx);
                        }
                    }
                });
                Some(true)
            }
            Err(_) => None,
        }
    }
}

#[cfg(windows)]
mod shared {
    use super::UnboundedSender;
    use std::sync::OnceLock;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    const CLASS: PCWSTR = w!("io.vectorapp-sic");
    const WINDOW: PCWSTR = w!("io.vectorapp-siw");
    const DATA: usize = 1542;
    static TX: OnceLock<UnboundedSender<String>> = OnceLock::new();

    pub fn claim(tx: UnboundedSender<String>) -> Option<bool> {
        let mutex = unsafe { CreateMutexW(None, true, w!("io.vectorapp-sim")) }.ok()?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            if let Ok(hwnd) = unsafe { FindWindowW(CLASS, WINDOW) } {
                let argv: Vec<String> = std::env::args().collect();
                let data = format!("{}|{}\0", super::cwd(), argv.join("|"));
                let cds = COPYDATASTRUCT { dwData: DATA, cbData: data.len() as u32, lpData: data.as_ptr() as _ };
                unsafe { SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize))) };
            }
            return Some(false);
        }
        // Held for the process's life: the mutex is what a later launch finds.
        std::mem::forget(mutex);
        let _ = TX.set(tx);
        // spawn-detached: the hidden window's message loop, no account state.
        std::thread::spawn(|| unsafe {
            let Ok(module) = GetModuleHandleW(None) else { return };
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: module.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
                CLASS,
                WINDOW,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module.into()),
                None,
            ) else {
                return;
            };
            SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_VISIBLE | WS_POPUP).0 as isize);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
        Some(true)
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_COPYDATA {
            let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            if cds.dwData == DATA && !cds.lpData.is_null() {
                let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
                let text = String::from_utf8_lossy(bytes);
                let text = text.trim_end_matches('\0');
                if let Some(tx) = TX.get() {
                    super::deliver(text.split('|').skip(1).map(String::from), tx);
                }
            }
            return LRESULT(1);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod shared {
    pub fn claim(_: super::UnboundedSender<String>) -> Option<bool> {
        None
    }
}

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
