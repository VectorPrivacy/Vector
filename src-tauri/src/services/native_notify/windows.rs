//! Windows toasts through WinRT, activated through a COM activator so a toast still
//! works from Action Center after Vector has closed.
//!
//! The app identity (AUMID) and the activator are registered per user under HKCU on
//! every launch, which keeps the activator pointing at whichever executable ran last.
//! The uninstaller removes both (`windows/hooks.nsh`).

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};
use tauri::AppHandle;
use windows::core::{implement, IUnknown, Interface, Ref, BOOL, GUID, HSTRING, PCWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::{DateTime, IReference, PropertyValue};
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_POINTER, ERROR_SUCCESS};
use windows::Win32::System::Com::{
    CoInitializeEx, CoRegisterClassObject, IClassFactory, IClassFactory_Impl, CLSCTX_LOCAL_SERVER,
    COINIT_MULTITHREADED, REGCLS_MULTIPLEUSE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    REG_SZ,
};
use windows::Win32::UI::Notifications::{
    INotificationActivationCallback, INotificationActivationCallback_Impl, NOTIFICATION_USER_INPUT_DATA,
};

use super::{Action, Activation, Backend, Matcher};
use crate::services::notification_service::{NotificationData, NotificationType};

/// The argument Windows launches the activator with when Vector isn't running.
const ACTIVATED_ARG: &str = "-ToastActivated";
/// Windows silently drops a Tag or Group longer than this, and a 64-hex channel id is one longer.
const MAX_KEY_LEN: usize = 63;
/// The toast draws its icon at 48 logical pixels; this stays sharp up to 400% scaling.
const ICON_PX: u32 = 192;
const REPLY_INPUT: &str = "reply";
/// Seconds from 1601-01-01 (Windows' epoch) to 1970-01-01, and DateTime's 100ns ticks per second.
const UNIX_TO_FILETIME_SECS: i64 = 11_644_473_600;
const TICKS_PER_SEC: i64 = 10_000_000;

static AUMID: OnceLock<String> = OnceLock::new();
static WORKER: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();

enum Job {
    Show { xml: String, tag: String, group: String, expires: Option<u64> },
    Retract(Matcher),
}

pub struct Windows;

impl Backend for Windows {
    fn init(app: &AppHandle) -> Result<(), String> {
        let base = app.config().identifier.clone();
        let (aumid, display) = if cfg!(debug_assertions) {
            (format!("{base}.dev"), "Vector (Dev)".to_string())
        } else {
            (base, app.config().product_name.clone().unwrap_or_else(|| "Vector".into()))
        };
        let clsid = format!("{{{:?}}}", clsid_for(&aumid));

        let icon = super::cache_dir().ok_or("no cache dir")?.join("vector.png");
        let bundled: &[u8] = include_bytes!("../../../icons/128x128.png");
        if std::fs::read(&icon).map(|b| b != bundled).unwrap_or(true) {
            std::fs::write(&icon, bundled).map_err(|e| e.to_string())?;
        }

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        set_reg(
            &format!(r"Software\Classes\CLSID\{clsid}\LocalServer32"),
            None,
            &format!("\"{}\" {ACTIVATED_ARG}", exe.display()),
        )?;
        let aumid_key = format!(r"Software\Classes\AppUserModelId\{aumid}");
        set_reg(&aumid_key, Some("DisplayName"), &display)?;
        set_reg(&aumid_key, Some("IconUri"), &icon.display().to_string())?;
        set_reg(&aumid_key, Some("CustomActivator"), &clsid)?;

        // Registered on the calling (UI) thread, so activations arrive through its message loop.
        let factory: IClassFactory = ActivatorFactory.into();
        unsafe { CoRegisterClassObject(&clsid_for(&aumid), &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE) }
            .map_err(|e| format!("CoRegisterClassObject: {e}"))?;

        let _ = AUMID.set(aumid);
        Ok(())
    }

    fn show(data: &NotificationData, account: &str) -> Result<(), String> {
        let open = Activation::new(Action::Open, account, data).ok_or("no chat id")?;
        let xml = toast_xml(data, &open);
        let tag = key(open.message.as_deref().unwrap_or(""));
        let group = key(&open.chat);
        worker().ok_or("no toast worker")?.send(Job::Show { xml, tag, group, expires: data.expires_at }).map_err(|e| e.to_string())
    }

    fn retract(matches: Matcher) {
        if let Some(tx) = worker() {
            let _ = tx.send(Job::Retract(matches));
        }
    }
}

/// Action Center outlives Vector, so its history (not a list kept here) is what gets
/// searched: a toast from before a restart is found by the target in its launch args.
fn retract_now(aumid: &str, matches: &Matcher) -> windows::core::Result<()> {
    let history = ToastNotificationManager::History()?;
    let id = HSTRING::from(aumid);
    let launch = HSTRING::from("launch");
    let delivered = history.GetHistoryWithId(&id)?;
    for i in 0..delivered.Size()? {
        let toast = delivered.GetAt(i)?;
        let args = toast.Content()?.DocumentElement()?.GetAttribute(&launch)?;
        if Activation::decode(&args.to_string_lossy(), None).is_some_and(|a| matches(&a)) {
            history.RemoveGroupedTagWithId(&toast.Tag()?, &toast.Group()?, &id)?;
        }
    }
    Ok(())
}

fn toast_xml(data: &NotificationData, open: &Activation) -> String {
    let community = data.notification_type == NotificationType::CommunityMessage;
    // A community toast names the sender; the community goes on its header.
    let title = if community { data.sender_name.as_deref().unwrap_or(&data.title) } else { &data.title };
    // Already round (the community badge would not survive Windows' own crop).
    let icon = super::notification_icon(
        data.avatar_path.as_deref().map(std::path::Path::new),
        data.group_avatar_path.as_deref().filter(|_| community).map(std::path::Path::new),
        ICON_PX,
    );

    let mut xml = String::with_capacity(1024);
    xml.push_str(&format!(r#"<toast launch="{}"><visual><binding template="ToastGeneric">"#, esc(&open.encode())));
    xml.push_str(&format!(r#"<text hint-maxLines="1">{}</text>"#, esc(title)));
    xml.push_str(&format!("<text>{}</text>", esc(&data.body)));
    if let Some(src) = icon {
        xml.push_str(&format!(
            r#"<image placement="appLogoOverride" src="{}"/>"#,
            esc(&src.display().to_string())
        ));
    }
    xml.push_str("</binding></visual>");
    if let Some(name) = data.group_name.as_deref().filter(|s| community && !s.is_empty()) {
        xml.push_str(&format!(
            r#"<header id="{}" title="{}" arguments="{}"/>"#,
            esc(&key(&open.chat)),
            esc(name),
            esc(&open.with(Action::Open, false).encode())
        ));
    }
    xml.push_str("<actions>");
    // The sender's name can be a fallback ("New Message") or masked, so the placeholder names nobody.
    xml.push_str(&format!(r#"<input id="{REPLY_INPUT}" type="text" placeHolderContent="Type a reply…"/>"#));
    xml.push_str(&format!(
        r#"<action content="Send" arguments="{}" hint-inputId="{REPLY_INPUT}" activationType="background"/>"#,
        esc(&open.with(Action::Reply, true).encode())
    ));
    xml.push_str(&format!(
        r#"<action content="Mark as read" arguments="{}" activationType="background"/>"#,
        esc(&open.with(Action::MarkRead, true).encode())
    ));
    // The right-click menu. Windows renders five actions at most, the two buttons included.
    for (label, ms) in super::MUTE_CHOICES {
        xml.push_str(&format!(
            r#"<action content="{}" arguments="{}" placement="contextMenu" activationType="background"/>"#,
            esc(label),
            esc(&open.mute(ms).encode())
        ));
    }
    // Vector plays its own notification sound.
    xml.push_str(r#"</actions><audio silent="true"/></toast>"#);
    xml
}

/// A stable activator id per app identity, so a dev build and an installed one never share one.
fn clsid_for(aumid: &str) -> GUID {
    let h = Sha256::digest(format!("vector-toast-activator:{aumid}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    GUID::from_u128(u128::from_be_bytes(b))
}

fn set_reg(path: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let mut hkey = HKEY::default();
    let err = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(format!("RegCreateKeyExW {path}: {err:?}"));
    }
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    let err = unsafe { RegSetValueExW(hkey, name_ptr, None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if err != ERROR_SUCCESS {
        return Err(format!("RegSetValueExW {path}: {err:?}"));
    }
    Ok(())
}

/// WinRT needs an initialised apartment, which tokio's threads lack, so one thread owns it.
fn worker() -> Option<Sender<Job>> {
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let _ = std::thread::Builder::new().name("toast".into()).spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            while let Ok(job) = rx.recv() {
                let Some(aumid) = AUMID.get() else { continue };
                let result = match job {
                    Job::Show { xml, tag, group, expires } => show_now(aumid, &xml, &tag, &group, expires),
                    Job::Retract(matches) => retract_now(aumid, &matches),
                };
                if let Err(e) = result {
                    log_warn!("[Notify] Toast: {e}");
                }
            }
        });
        Mutex::new(tx)
    });
    tx.lock().ok().map(|t| t.clone())
}

fn show_now(aumid: &str, xml: &str, tag: &str, group: &str, expires: Option<u64>) -> windows::core::Result<()> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    if !tag.is_empty() {
        toast.SetTag(&HSTRING::from(tag))?;
    }
    toast.SetGroup(&HSTRING::from(group))?;
    // Windows removes the toast at the message's expiry itself, so it goes even if Vector is closed.
    if let Some(at) = expires {
        let when = DateTime { UniversalTime: (at as i64 + UNIX_TO_FILETIME_SECS) * TICKS_PER_SEC };
        toast.SetExpirationTime(&PropertyValue::CreateDateTime(when)?.cast::<IReference<DateTime>>()?)?;
    }
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?.Show(&toast)
}

fn key(id: &str) -> String {
    if id.len() <= MAX_KEY_LEN {
        return id.to_string();
    }
    let digest = Sha256::digest(id.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    vector_core::simd::hex::bytes_to_hex_16(&b)
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids these, and one would sink the whole toast.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

#[implement(INotificationActivationCallback)]
struct Activator;

impl INotificationActivationCallback_Impl for Activator_Impl {
    fn Activate(
        &self,
        _aumid: &PCWSTR,
        invokedargs: &PCWSTR,
        data: *const NOTIFICATION_USER_INPUT_DATA,
        count: u32,
    ) -> windows::core::Result<()> {
        let args = unsafe { invokedargs.to_string() }.unwrap_or_default();
        let mut reply = None;
        if !data.is_null() {
            for item in unsafe { std::slice::from_raw_parts(data, count as usize) } {
                if unsafe { item.Key.to_string() }.is_ok_and(|k| k == REPLY_INPUT) {
                    reply = unsafe { item.Value.to_string() }.ok();
                }
            }
        }
        if let Some(activation) = Activation::decode(&args, reply.as_deref()) {
            super::handle(activation);
        }
        Ok(())
    }
}

#[implement(IClassFactory)]
struct ActivatorFactory;

impl IClassFactory_Impl for ActivatorFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if object.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *object = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let callback: INotificationActivationCallback = Activator.into();
        unsafe { callback.query(iid, object) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_survives_markup_and_control_characters() {
        assert_eq!(esc("a<b>&\"c'\u{0007}"), "a&lt;b&gt;&amp;&quot;c&apos;");
    }

    #[test]
    fn keys_fit_the_windows_limit() {
        let npub = format!("npub1{}", "q".repeat(58));
        assert_eq!(npub.len(), 63);
        assert_eq!(key(&npub), npub);
        let channel = "a".repeat(64);
        assert_eq!(key(&channel).len(), 32);
        assert_eq!(key(&channel), key(&channel));
    }

    #[test]
    fn activator_id_differs_per_identity_and_is_stable() {
        assert_eq!(clsid_for("io.vectorapp"), clsid_for("io.vectorapp"));
        assert_ne!(clsid_for("io.vectorapp"), clsid_for("io.vectorapp.dev"));
    }

    #[test]
    fn toast_carries_every_action_and_parses_as_xml() {
        let data = NotificationData::community_message(
            "Alice <&>".into(),
            "Rust Club".into(),
            "hello \"world\"".into(),
            None,
            None,
            "a".repeat(64),
        )
        .with_message_id("ff00".into())
        .with_sender("npub1alice".into());
        let open = Activation::new(Action::Open, "npub1me", &data).unwrap();
        let xml = toast_xml(&data, &open);
        assert!(xml.contains(r#"title="Rust Club""#));
        assert!(xml.contains("Alice &lt;&amp;&gt;"));
        assert!(xml.contains("a=reply") && xml.contains("a=read") && xml.contains("a=open"));
        assert_eq!(xml.matches(r#"placement="contextMenu""#).count(), MUTE_CHOICES_LEN);
        assert!(xml.matches("<action ").count() <= 5, "Windows renders five actions at most");
        assert_eq!(xml.matches("from=npub1alice").count(), xml.matches("acct=npub1me").count());
        let doc = XmlDocument::new().unwrap();
        doc.LoadXml(&HSTRING::from(xml)).unwrap();
    }

    const MUTE_CHOICES_LEN: usize = super::super::MUTE_CHOICES.len();
}
