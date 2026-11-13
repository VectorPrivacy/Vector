//! Mini App windows on Linux, hosted by `vector-gpui-miniapp` beside the app: WebKitGTK and
//! GTK load there, on first open, and never here. Its `webxdc://` requests are answered here,
//! where the packages live.

use std::collections::HashMap;
use std::io::BufWriter;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Mutex, OnceLock};

use super::wire::{self, FromHost, ToHost};
use super::{Event, PLAYER, PlayerSpec, Spec};

struct Host {
    stdin: BufWriter<ChildStdin>,
    /// Open windows, with the loopback host an isolated one holds.
    open: HashMap<String, Option<u64>>,
    generation: u64,
}

static HOST: Mutex<Option<Host>> = Mutex::new(None);
static EVENTS: OnceLock<Box<dyn Fn(Event) + Send + Sync>> = OnceLock::new();

pub fn start(events: impl Fn(Event) + Send + Sync + 'static) {
    let _ = EVENTS.set(Box::new(events));
}

fn emit(e: Event) {
    if let Some(f) = EVENTS.get() {
        f(e);
    }
}

/// Sends to the host. A window being opened is registered first, starting the host if need
/// be; anything else is dropped while no host runs.
fn post(msg: &ToHost, body: &[u8], opening: Option<(&str, Option<u64>)>) -> Result<(), String> {
    let mut host = HOST.lock().map_err(|_| "Mini App host lock poisoned".to_string())?;
    if host.is_none() {
        if opening.is_none() {
            return Ok(());
        }
        *host = Some(launch()?);
    }
    let h = host.as_mut().expect("host set above");
    if let Some((label, isolated)) = opening {
        h.open.insert(label.to_string(), isolated);
    }
    wire::write(&mut h.stdin, msg, body).map_err(|e| format!("Mini App host unreachable: {e}"))
}

fn launch() -> Result<Host, String> {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = exe.with_file_name("vector-gpui-miniapp");
    let mut child = Command::new(&helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Mini Apps need {}: {e}", helper.display()))?;
    let stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    std::thread::Builder::new()
        .name("miniapp-host".into())
        .spawn(move || {
            while let Ok((msg, body)) = wire::read::<FromHost>(&mut stdout) {
                on_message(msg, body);
            }
            let _ = child.wait();
            gone(generation);
        })
        .map_err(|e| e.to_string())?;
    Ok(Host { stdin: BufWriter::new(stdin), open: HashMap::new(), generation })
}

/// The host exited: every window it held is closed.
fn gone(generation: u64) {
    let labels: Vec<_> = match HOST.lock() {
        Ok(mut h) if h.as_ref().is_some_and(|h| h.generation == generation) => {
            h.take().map(|h| h.open.into_iter().collect()).unwrap_or_default()
        }
        _ => return,
    };
    for (label, isolated) in labels {
        closed(label, isolated);
    }
}

fn closed(label: String, isolated: Option<u64>) {
    if let Some(id) = isolated {
        vector_media::miniapps::isolated::stop_if(&label, id);
    }
    emit(Event::Closed(label));
}

fn on_message(msg: FromHost, body: Vec<u8>) {
    match msg {
        FromHost::Ipc { label, body } => emit(Event::Ipc(label, body)),
        FromHost::Closed { label } => {
            let isolated = HOST.lock().ok().and_then(|mut h| h.as_mut()?.open.remove(&label)).flatten();
            closed(label, isolated);
        }
        FromHost::Failed { label, error } => {
            let isolated = HOST.lock().ok().and_then(|mut h| h.as_mut()?.open.remove(&label)).flatten();
            if let Some(id) = isolated {
                vector_media::miniapps::isolated::stop_if(&label, id);
            }
            emit(Event::Failed(label, error));
        }
        FromHost::Player { body } => emit(Event::Player(body)),
        FromHost::Browse { url } => emit(Event::Browse(url)),
        FromHost::Serve { id, label, method, uri, headers } => serve(id, &label, method, uri, headers, body),
    }
}

fn serve(id: u64, label: &str, method: String, uri: String, headers: Vec<(String, String)>, body: Vec<u8>) {
    let mut req = http::Request::builder().method(method.as_str()).uri(uri.as_str());
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let Ok(req) = req.body(body) else {
        let _ = post(&ToHost::Reply { id, status: 400, headers: Vec::new() }, &[], None);
        return;
    };
    vector_media::miniapps::serve(label, req, move |response| {
        let (parts, body) = response.into_parts();
        let headers = parts
            .headers
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let _ = post(&ToHost::Reply { id, status: parts.status.as_u16(), headers }, &body, None);
    });
}

pub fn open(spec: Spec) -> Result<(), String> {
    let isolated_id = spec.isolated.as_ref().map(|i| i.id);
    let label = spec.label.clone();
    let isolated = spec.isolated.map(|i| {
        let mut bypass = vec![format!("localhost:{}", i.port)];
        if let Some(rt) = vector_media::miniapps::isolated::rt_ws_port() {
            bypass.push(format!("127.0.0.1:{rt}"));
        }
        wire::Isolated { data_dir: i.data_dir, boot_url: i.boot_url, sink: i.sink, bypass }
    });
    let msg = ToHost::Open(wire::Open {
        label: spec.label,
        title: spec.title,
        url: spec.url,
        init: spec.init,
        farewell: spec.farewell,
        proxy: spec.proxy,
        isolated,
    });
    post(&msg, &[], Some((&label, isolated_id)))
}

pub fn player(spec: PlayerSpec) -> Result<(), String> {
    let msg = ToHost::Player { title: spec.title, url: spec.url, size: spec.size, init: super::PLAYER_SHIM.into() };
    post(&msg, &[], Some((PLAYER, None)))
}

pub fn focus(label: &str, navigate: Option<String>) {
    let _ = post(&ToHost::Focus { label: label.into(), navigate }, &[], None);
}

pub fn eval(label: &str, js: String) {
    let _ = post(&ToHost::Eval { label: label.into(), js }, &[], None);
}

/// Gone at once, with no farewell page: an account swap.
pub fn destroy(label: &str) {
    let _ = post(&ToHost::Destroy { label: label.into() }, &[], None);
}

pub fn close(label: &str) {
    let _ = post(&ToHost::Close { label: label.into() }, &[], None);
}

/// Runs `f` on a GLib main loop thread of the app's own, started on first use: D-Bus signal
/// subscriptions deliver to the context that was current when they subscribed.
pub fn on_glib(f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    use gtk::glib;
    static CONTEXT: OnceLock<Option<glib::MainContext>> = OnceLock::new();
    let ctx = CONTEXT.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new().name("glib".into()).spawn(move || {
            let ctx = glib::MainContext::new();
            let main = glib::MainLoop::new(Some(&ctx), false);
            let _ = ctx.with_thread_default(|| {
                let _ = tx.send(ctx.clone());
                main.run();
            });
        });
        spawned.ok().and_then(|_| rx.recv().ok())
    });
    ctx.as_ref().ok_or_else(|| "no GLib thread".to_string())?.invoke(f);
    Ok(())
}
