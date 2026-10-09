//! macOS notifications through UserNotifications: a reply field, Mark as Read and the mutes
//! on every message, the round avatar as the notification's image, and one thread per chat
//! in Notification Center.
//!
//! Notification Center keeps what Vector delivered across restarts, so retraction searches
//! its delivered list rather than one kept here. A click that launches Vector reaches the
//! delegate once `init` sets it, which happens while the app finishes launching.
//!
//! UserNotifications needs a real app bundle: an unbundled binary (`tauri dev`) has no
//! notification identity, so `init` declines it and the plugin's plain notifications stay.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread};
use objc2_foundation::{ns_string, NSArray, NSBundle, NSDictionary, NSError, NSSet, NSString, NSURL};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationAttachment, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNTextInputNotificationAction, UNTextInputNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use tauri::AppHandle;

use super::{Action, Activation, Backend, Matcher};
use crate::services::notification_service::{NotificationData, NotificationType};

const CATEGORY: &str = "vector.message";
const REPLY: &str = "reply";
const READ: &str = "read";
const MUTE_PREFIX: &str = "mute:";
/// `userInfo` keys: the encoded [`Activation`], and a self-destructing message's expiry.
const KEY_ACTIVATION: &str = "v";
const KEY_EXPIRES: &str = "exp";
/// The image shows at up to 80 points on a Retina screen.
const ICON_PX: u32 = 192;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[name = "VectorNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        // Vector skips notifying while its window has focus, so one that arrives while it's
        // frontmost (another window, or just activated) still shows. Vector plays its own sound.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        #[allow(non_snake_case)]
        unsafe fn userNotificationCenter_willPresentNotification_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        #[allow(non_snake_case)]
        unsafe fn userNotificationCenter_didReceiveNotificationResponse_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &block2::DynBlock<dyn Fn()>,
        ) {
            if let Some(activation) = activation_of(response) {
                super::handle(activation);
            }
            handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

pub struct MacOS;

impl Backend for MacOS {
    fn init(_app: &AppHandle) -> Result<(), String> {
        let bundle = NSBundle::mainBundle();
        let bundled = bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app");
        if !bundled {
            return Err("not running from an app bundle".into());
        }
        let center = unsafe { UNUserNotificationCenter::currentNotificationCenter() };

        // The center holds its delegate weakly; this one lives as long as the process.
        let delegate = Delegate::new();
        unsafe { center.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
        std::mem::forget(delegate);

        unsafe { center.setNotificationCategories(&NSSet::from_retained_slice(&[category()])) };
        let asked = RcBlock::new(|granted: Bool, error: *mut NSError| {
            if !granted.as_bool() {
                let why = unsafe { error.as_ref() }.map(|e| e.to_string()).unwrap_or_else(|| "declined".into());
                log_warn!("[Notify] Notifications not allowed: {why}");
            }
        });
        unsafe {
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge,
                &asked,
            )
        };

        // A self-destructing message's notification can't expire on its own here; whatever
        // expired while Vector was closed goes now.
        retract_where(|_, expires| expires.is_some_and(|at| at <= now_secs()));
        Ok(())
    }

    fn show(data: &NotificationData, account: &str) -> Result<(), String> {
        let open = Activation::new(Action::Open, account, data).ok_or("no chat id")?;
        let community = data.notification_type == NotificationType::CommunityMessage;
        // A community notification names the sender, with the community beneath.
        let title = if community { data.sender_name.as_deref().unwrap_or(&data.title) } else { &data.title };

        let content = unsafe { UNMutableNotificationContent::new() };
        unsafe {
            content.setTitle(&NSString::from_str(title));
            if let Some(name) = data.group_name.as_deref().filter(|s| community && !s.is_empty()) {
                content.setSubtitle(&NSString::from_str(name));
            }
            content.setBody(&NSString::from_str(&data.body));
            content.setThreadIdentifier(&NSString::from_str(&open.chat));
            content.setCategoryIdentifier(&NSString::from_str(CATEGORY));
            content.setUserInfo(&user_info(&open, data.expires_at));
            if let Some(attachment) = icon_attachment(data, community) {
                content.setAttachments(&NSArray::from_retained_slice(&[attachment]));
            }
        }

        // One notification per message: a re-delivery replaces rather than duplicates it.
        let id = format!("{}|{}|{}", open.account, open.chat, open.message.as_deref().unwrap_or(""));
        let request = unsafe {
            UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&id), &content, None)
        };
        let done = RcBlock::new(|error: *mut NSError| {
            if let Some(e) = unsafe { error.as_ref() } {
                log_warn!("[Notify] Notification not delivered: {e}");
            }
        });
        unsafe {
            UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, Some(&done))
        };
        Ok(())
    }

    fn retract(matches: Matcher) {
        retract_where(move |activation, _| activation.is_some_and(|a| matches(a)));
    }
}

/// Remove every delivered notification for which `matches(activation, expires_at)` holds.
fn retract_where(matches: impl Fn(Option<&Activation>, Option<u64>) -> bool + 'static) {
    let found = RcBlock::new(move |delivered: std::ptr::NonNull<NSArray<UNNotification>>| {
        let delivered = unsafe { delivered.as_ref() };
        let mut gone: Vec<Retained<NSString>> = Vec::new();
        for notification in delivered.iter() {
            let request = unsafe { notification.request() };
            let info = unsafe { request.content().userInfo() };
            let activation = string_for(&info, KEY_ACTIVATION).and_then(|v| Activation::decode(&v, None));
            let expires = string_for(&info, KEY_EXPIRES).and_then(|e| e.parse().ok());
            if matches(activation.as_ref(), expires) {
                gone.push(unsafe { request.identifier() });
            }
        }
        if !gone.is_empty() {
            unsafe {
                UNUserNotificationCenter::currentNotificationCenter()
                    .removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&gone))
            };
        }
    });
    unsafe { UNUserNotificationCenter::currentNotificationCenter().getDeliveredNotificationsWithCompletionHandler(&found) };
}

/// The one category every message carries. macOS shows Reply as a button and the rest under
/// the notification's Options menu.
fn category() -> Retained<UNNotificationCategory> {
    let none = UNNotificationActionOptions::empty();
    let mut actions: Vec<Retained<UNNotificationAction>> = Vec::with_capacity(2 + super::MUTE_CHOICES.len());
    // The sender's name can be a fallback ("New Message") or masked, so the placeholder names nobody.
    let reply = unsafe {
        UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
            &NSString::from_str(REPLY),
            ns_string!("Reply"),
            none,
            ns_string!("Send"),
            ns_string!("Type a reply…"),
        )
    };
    actions.push(Retained::into_super(reply));
    actions.push(unsafe { UNNotificationAction::actionWithIdentifier_title_options(&NSString::from_str(READ), ns_string!("Mark as Read"), none) });
    for (label, ms) in super::MUTE_CHOICES {
        actions.push(unsafe {
            UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(&format!("{MUTE_PREFIX}{ms}")),
                &NSString::from_str(label),
                none,
            )
        });
    }
    unsafe {
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(CATEGORY),
            &NSArray::from_retained_slice(&actions),
            &NSArray::new(),
            UNNotificationCategoryOptions::empty(),
        )
    }
}

/// What the user asked for: the notification's target with the pressed action and typed text.
fn activation_of(response: &UNNotificationResponse) -> Option<Activation> {
    let info = unsafe { response.notification().request().content().userInfo() };
    let open = Activation::decode(&string_for(&info, KEY_ACTIVATION)?, None)?;
    let action = unsafe { response.actionIdentifier() }.to_string();
    if action == unsafe { UNNotificationDefaultActionIdentifier }.to_string() {
        return Some(open);
    }
    if action == REPLY {
        let text = response.downcast_ref::<UNTextInputNotificationResponse>().map(|r| unsafe { r.userText() }.to_string());
        let mut reply = open.with(Action::Reply, true);
        reply.reply = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        return Some(reply);
    }
    if action == READ {
        return Some(open.with(Action::MarkRead, true));
    }
    // Dismissal (and anything unknown) does nothing.
    action.strip_prefix(MUTE_PREFIX).and_then(|ms| ms.parse().ok()).map(|ms| open.mute(ms))
}

fn user_info(open: &Activation, expires_at: Option<u64>) -> Retained<NSDictionary> {
    let mut keys = vec![NSString::from_str(KEY_ACTIVATION)];
    let mut values = vec![NSString::from_str(&open.encode())];
    if let Some(at) = expires_at {
        keys.push(NSString::from_str(KEY_EXPIRES));
        values.push(NSString::from_str(&at.to_string()));
    }
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let dict = NSDictionary::from_retained_objects(&keys, &values);
    // SAFETY: a dictionary of strings is a dictionary of objects.
    unsafe { Retained::cast_unchecked(dict) }
}

fn string_for(info: &NSDictionary, key: &str) -> Option<String> {
    let key = NSString::from_str(key);
    let value = info.objectForKey(&*key as &AnyObject)?;
    value.downcast_ref::<NSString>().map(|s| s.to_string())
}

/// The round avatar (with the community as a corner badge) as the notification's image. The
/// system moves an attachment's file into its own store, so each notification gets a copy.
fn icon_attachment(data: &NotificationData, community: bool) -> Option<Retained<UNNotificationAttachment>> {
    let icon = super::notification_icon(
        data.avatar_path.as_deref().map(Path::new),
        data.group_avatar_path.as_deref().filter(|_| community).map(Path::new),
        ICON_PX,
    )?;
    let copy = attachment_copy(&icon)?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(&copy.to_string_lossy()));
    match unsafe { UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(ns_string!("avatar"), &url, None) } {
        Ok(attachment) => Some(attachment),
        Err(e) => {
            log_warn!("[Notify] Avatar not attached: {e}");
            let _ = std::fs::remove_file(&copy);
            None
        }
    }
}

fn attachment_copy(icon: &Path) -> Option<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = super::cache_dir()?.join("attachments");
    std::fs::create_dir_all(&dir).ok()?;
    let copy = dir.join(format!("{}-{}.png", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::copy(icon, &copy).ok()?;
    Some(copy)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_info_round_trips_the_activation_and_expiry() {
        let open = Activation {
            action: Action::Open,
            account: "npub1me".into(),
            chat: "npub1chat".into(),
            message: Some("ab12".into()),
            sender: Some("npub1from".into()),
            mute_ms: None,
            reply: None,
        };
        let info = user_info(&open, Some(1_700_000_000));
        assert_eq!(Activation::decode(&string_for(&info, KEY_ACTIVATION).unwrap(), None).unwrap(), open);
        assert_eq!(string_for(&info, KEY_EXPIRES).as_deref(), Some("1700000000"));
        assert!(string_for(&user_info(&open, None), KEY_EXPIRES).is_none());
    }

    #[test]
    fn the_category_carries_reply_read_and_every_mute() {
        let category = category();
        let ids: Vec<String> = unsafe { category.actions() }.iter().map(|a| unsafe { a.identifier() }.to_string()).collect();
        assert_eq!(ids[0], REPLY);
        assert_eq!(ids[1], READ);
        assert_eq!(ids.len(), 2 + super::super::MUTE_CHOICES.len());
        for (id, (_, ms)) in ids[2..].iter().zip(super::super::MUTE_CHOICES) {
            assert_eq!(id.strip_prefix(MUTE_PREFIX).and_then(|m| m.parse::<i64>().ok()), Some(ms));
        }
    }
}
