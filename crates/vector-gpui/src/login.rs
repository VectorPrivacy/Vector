//! The login screen (`#login-form`, components/auth/LoginScreen.svelte): Start, Import and the
//! encryption steps (choose, PIN or password; set up or unlock), painted on the design's frame:
//! the lockup on top, one centred step, the credits along the bottom, the art behind.

use std::time::Instant;

use crate::kit::Tipped as _;
use gpui::{AnimationExt, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, ObjectFit, ParentElement, Render, SharedString, Styled, Window, div, linear_color_stop, linear_gradient, prelude::*, px, svg};
use crate::pic::img;
use zeroize::Zeroizing;

#[path = "loginnet.rs"]
mod loginnet;
#[path = "loginsigner.rs"]
mod signer;

use crate::backend::{self, account};
use crate::composer::{Look, SendText, TextInput};
use crate::kit::{self, Choice, Popup};
use crate::t;
use crate::theme::*;

/// The account is open: the app can boot.
pub struct LoggedIn {
    pub npub: String,
}

const BG_FADE: f32 = 0.3;

impl EventEmitter<LoggedIn> for LoginView {}

/// A re-pair started from Settings ended, either way.
pub struct ReauthClosed;
impl EventEmitter<ReauthClosed> for LoginView {}

/// Add Profile's milestones: the live account torn down for the new one, or the user backing out.
pub enum AddProfile {
    Committed,
    Back,
}

impl EventEmitter<AddProfile> for LoginView {}

/// Another account on this device was picked from the pill: open it instead.
pub struct PickAccount(pub String);

/// Sign in with Another Device: the transfer card, as the receiver.
pub struct OpenTransfer;
impl EventEmitter<OpenTransfer> for LoginView {}

/// The ⓘ beside "Sign in with": what each way means.
pub struct MethodInfo;
impl EventEmitter<MethodInfo> for LoginView {}

impl EventEmitter<PickAccount> for LoginView {}

/// This device's copy of the account can no longer be opened: remove it, so its keys can sign in
/// afresh.
pub struct ResetAccount(pub String);
impl EventEmitter<ResetAccount> for LoginView {}

/// The pre-login account pill (`#login-account-picker`) and its list of the other accounts.
struct Picker {
    rows: Vec<crate::switcher::Row>,
    active: Option<String>,
    open: bool,
    /// Times the list opened or closed: the chevron turns only on a change, keyed by this.
    flips: u64,
}

impl Picker {
    fn set_open(&mut self, open: bool) {
        if self.open != open {
            self.open = open;
            self.flips += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Start,
    Import,
    Encrypt,
}

#[derive(Clone, Copy, PartialEq)]
enum Cred {
    Choose,
    Pin,
    Password,
    /// The account opens only behind the phone's own prompt.
    Biometric,
}

fn methods() -> &'static [Choice] {
    kit::choices("login-methods", || {
        vec![
            Choice { value: "qr", label: kit::interned(&t::login_method_qr()) },
            Choice { value: "key", label: kit::interned(&t::login_method_key()) },
            Choice { value: "signer", label: kit::interned(&t::login_method_signer()) },
        ]
    })
}

pub struct LoginView {
    step: Step,
    method: &'static str,
    select_open: bool,
    key: Entity<TextInput>,
    password: Entity<TextInput>,
    reveal: bool,
    pin: String,
    pin_focus: FocusHandle,
    /// Unlocking a stored account rather than setting up a new one.
    unlock: bool,
    cred: Cred,
    title: SharedString,
    /// A step is running: the title breathes in the brand gradient.
    busy: bool,
    error: bool,
    /// Setting up an account left unencrypted: no lock beside the title.
    lockless: bool,
    /// The credential just refused: Aggroboi glares in the title's place.
    wrong: Option<&'static str>,
    /// The first entry of a new credential, awaiting its confirmation.
    first: Option<Zeroizing<String>>,
    bg_hidden: bool,
    /// When the background last flipped: `.lg-art` fades over 0.3 s.
    bg_flip: Option<Instant>,
    popup: Option<Popup>,
    born: Instant,
    /// Adding a profile over a live account: whether that account was already torn down.
    adding: Option<bool>,
    picker: Option<Picker>,
    net: loginnet::LoginNet,
    signer: signer::Signer,
    /// Re-pairing this account's signer: `true` when an unlock is waiting on it.
    reauth: Option<bool>,
    reauth_from: Step,
    /// The open popup offers that re-pair.
    popup_reauth: bool,
    /// The open popup offers to reset this device's copy of the account.
    popup_reset: bool,
    /// The open popup is the warning before a new account takes the phone's prompt.
    popup_bio_setup: bool,
    live: bool,
    /// The system's language, offered once before anything else is chosen.
    offer: Option<LangOffer>,
    /// The phone's prompt is offered, under this label.
    bio: Option<SharedString>,
    /// Its prompt is up, or the unlock it passed to is running.
    bio_busy: bool,
    /// A new account may choose the phone's prompt, under this label.
    bio_option: Option<SharedString>,
    /// A signer app (Amber) is installed to sign in with.
    nip55: bool,
    nip55_busy: bool,
}

struct LangOffer {
    tag: &'static str,
    name: &'static str,
    /// Downloading, 0 to 1000.
    busy: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
    error: Option<String>,
}

impl LoginView {
    /// The welcome screen, or the unlock step of an encrypted account (`security` "pin"/"password").
    pub fn new(unlock: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = Look {
            size: px(14.),
            line_height: px(17.),
            color: c(0xffffff),
            placeholder: c(ICON_MUTED),
            masked: true,
            masked_size: Some(px(20.)),
            ..Default::default()
        };
        let key = cx.new(|cx| {
            TextInput::new(t::login_key_placeholder(), cx)
                .with_look(field.clone())
                .keep_on_submit()
        });
        let password = cx.new(|cx| {
            TextInput::new(t::login_password_placeholder(), cx)
                .with_look(field.clone())
                .keep_on_submit()
        });
        let bunker_url = cx.new(|cx| {
            TextInput::new(t::login_bunker_placeholder(), cx)
                .with_look(Look { masked: false, masked_size: None, ..field.clone() })
                .keep_on_submit()
        });
        cx.subscribe_in(&bunker_url, window, |this, _, _: &SendText, window, cx| {
            this.signer_connect(window, cx)
        })
        .detach();
        cx.subscribe_in(&key, window, |this, _, _: &SendText, window, cx| {
            this.import_key(window, cx)
        })
        .detach();
        cx.subscribe_in(&password, window, |this, _, _: &SendText, window, cx| {
            this.submit_password(window, cx)
        })
        .detach();
        let mut view = Self {
            step: Step::Start,
            method: "qr",
            select_open: false,
            key,
            password,
            reveal: false,
            pin: String::new(),
            pin_focus: cx.focus_handle(),
            unlock: false,
            cred: Cred::Choose,
            title: SharedString::default(),
            busy: false,
            error: false,
            lockless: false,
            wrong: None,
            first: None,
            bg_hidden: false,
            bg_flip: None,
            popup: None,
            born: Instant::now(),
            adding: None,
            picker: None,
            net: Default::default(),
            signer: signer::Signer::new(bunker_url),
            reauth: None,
            reauth_from: Step::Start,
            popup_reauth: false,
            popup_reset: false,
            popup_bio_setup: false,
            live: false,
            offer: crate::lang::offer().map(|o| LangOffer { tag: o.tag, name: o.name, busy: None, error: None }),
            bio: None,
            bio_busy: false,
            bio_option: None,
            nip55: false,
            nip55_busy: false,
        };
        if let Some(security) = unlock {
            view.unlock = true;
            view.step = Step::Encrypt;
            view.start_credential(cred_for(security), window, cx);
        }
        view
    }

    /// The welcome screen over a live account, which stays until Create or Login commits.
    pub fn adding(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(None, window, cx);
        view.adding = Some(false);
        view
    }

    /// Switch: the index names the pack, the pack is checked against it, then everything reads in it.
    fn offer_switch(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.offer.as_mut().filter(|o| o.busy.is_none()) else { return };
        // Nothing leaves before a network is chosen; the welcome screen asks for one.
        if self.live && vector_core::transport::state() == vector_core::transport::TransportState::Unknown {
            o.error = Some(t::routing_choose().to_string());
            cx.notify();
            return;
        }
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        o.busy = Some(progress.clone());
        o.error = None;
        let tag = o.tag;
        let rx = backend::run(async move {
            let index = backend::langpacks::refresh().await?;
            let entry = index.entry(tag).cloned().ok_or_else(|| t::language_not_listed().to_string())?;
            backend::langpacks::download(entry, progress).await
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err(t::language_pack_unsaved().to_string()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(pack) => {
                        crate::lang::offered(tag);
                        this.offer = None;
                        crate::lang::install(pack, true, cx);
                    }
                    Err(e) => {
                        if let Some(o) = this.offer.as_mut() {
                            o.busy = None;
                            o.error = Some(e);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            let going = this
                .update(cx, |this, cx| {
                    let on = this.offer.as_ref().is_some_and(|o| o.busy.is_some());
                    if on {
                        cx.notify();
                    }
                    on
                })
                .unwrap_or(false);
            if !going {
                break;
            }
        })
        .detach();
        cx.notify();
    }

    /// `.lg-offer`: one line above the footer, gone for good once answered.
    fn offer_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let o = self.offer.as_ref().filter(|_| self.adding.is_none() && (self.live || std::env::var_os("VECTOR_LANG_OFFER").is_some()))?;
        let text: AnyElement = if let Some(p) = &o.busy {
            div().child(t::language_downloading(p.load(std::sync::atomic::Ordering::Relaxed).min(1000) / 10)).into_any_element()
        } else if let Some(e) = &o.error {
            div().text_color(c(0xFCA559)).child(t::language_download_failed(&*crate::coretext::say(e))).into_any_element()
        } else {
            let strong = gpui::HighlightStyle { color: Some(c(0xffffff)), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
            div().child(kit::run_text(&t::language_offer(kit::escape(o.name)), strong)).into_any_element()
        };
        let busy = o.busy.is_some();
        let tag = o.tag;
        Some(
            div()
                .id("lang-offer")
                .max_w(px(560.))
                .min_w_0()
                .min_h(px(38.))
                .pl(px(14.))
                .pr(px(5.))
                .py(px(5.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(19.))
                .bg(c(0x141414))
                .border_1()
                .border_color(c(0x262626))
                .text_size(px(13.))
                .line_height(px(16.))
                .text_color(c(0xc8c8c8))
                .child(svg().path("icons/globe.svg").size(px(16.)).flex_shrink_0().text_color(c(PRIMARY)))
                .child(div().min_w_0().flex_shrink(1.).child(text))
                .when(!busy, |d| {
                    d.child(
                        div()
                            .id("lang-offer-switch")
                            .flex_shrink_0()
                            .h(px(26.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .rounded(px(13.))
                            .bg(c(PRIMARY))
                            .hover(|d| d.bg(c(ACCENT)))
                            .text_color(c(0x052b1e))
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.offer_switch(cx)))
                            .child(t::language_offer_switch()),
                    )
                    .child(
                        div()
                            .id("lang-offer-dismiss")
                            .flex_shrink_0()
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .hover(|d| d.bg(c(0x262626)))
                            .cursor_pointer()
                            .tip(t::language_offer_dismiss())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                crate::lang::offered(tag);
                                this.offer = None;
                                cx.notify();
                            }))
                            .child(svg().path("icons/x.svg").size(px(14.)).text_color(c(0x8a8a8a))),
                    )
                })
                .into_any_element(),
        )
    }

    /// The device's accounts, for the pill: it shows only when there is a choice to make.
    pub fn set_accounts(
        &mut self,
        rows: Vec<crate::switcher::Row>,
        active: Option<String>,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        self.picker = (rows.len() >= 2 && self.adding.is_none()).then(|| {
            let active = active.filter(|a| rows.iter().any(|r| &r.npub == a));
            Picker { rows, active, open, flips: 0 }
        });
        cx.notify();
    }

    fn pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.picker.as_ref()?;
        let me = p
            .active
            .as_ref()
            .and_then(|a| p.rows.iter().find(|r| &r.npub == a));
        let label = match me {
            Some(r) => r.name.clone().unwrap_or_else(|| r.npub.clone()),
            None => t::login_select_profile().into(),
        };
        let avatar = match me.and_then(|r| r.avatar.clone()) {
            Some(path) => img(path).object_fit(gpui::ObjectFit::Cover)
                .size(px(44.))
                .rounded_full()
                .flex_shrink_0()
                .into_any_element(),
            None => img("icons/user-placeholder.svg")
                .size(px(44.))
                .rounded_full()
                .flex_shrink_0()
                .into_any_element(),
        };
        let chevron = svg()
            .path("icons/login/chevron.svg")
            .w(px(10.))
            .h(px(6.))
            .ml(px(2.))
            .flex_shrink_0()
            .text_color(c(0x474747));
        Some(
            div()
                .id("login-account-picker")
                .max_w(px(429.72))
                .h(px(44.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .when(!self.busy, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = this.picker.as_mut() {
                            p.set_open(!p.open);
                        }
                        cx.notify();
                    }))
                })
                .child(avatar)
                .child(
                    kit::spaced(&label, 2.)
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(20.))
                        .line_height(px(24.)),
                )
                // `.lg-account-chevron`: a half turn over 0.16 s as the list opens or closes.
                .child({
                    let open = p.open;
                    let turned = move |c: gpui::Svg, t: f32| {
                        let t = if open { t } else { 1. - t };
                        c.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI * t)))
                    };
                    if p.flips == 0 {
                        turned(chevron, 1.).into_any_element()
                    } else {
                        chevron
                            .with_animation(
                                gpui::ElementId::NamedInteger("lg-chevron".into(), p.flips),
                                gpui::Animation::new(std::time::Duration::from_millis(160)).with_easing(gpui::ease_in_out),
                                turned,
                            )
                            .into_any_element()
                    }
                })
                .into_any_element(),
        )
    }

    /// `.login-account-list`: the other accounts, below the pill over a dimmed screen.
    /// `top`: just under the account pill, which follows the lockup's height.
    fn account_list(&self, top: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.picker.as_ref().filter(|p| p.open)?;
        let rows = p
            .rows
            .iter()
            .filter(|r| p.active.as_ref() != Some(&r.npub))
            .map(|r| {
                let npub = r.npub.clone();
                let avatar = match &r.avatar {
                    Some(path) => img(path.clone()).object_fit(gpui::ObjectFit::Cover)
                        .size(px(28.))
                        .rounded_full()
                        .flex_shrink_0()
                        .into_any_element(),
                    None => img("icons/user-placeholder.svg")
                        .size(px(28.))
                        .rounded_full()
                        .flex_shrink_0()
                        .into_any_element(),
                };
                div()
                    .id(SharedString::from(format!("login-pick-{}", r.npub)))
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .pl(px(21.))
                    .pr(px(21.))
                    .border_l_3()
                    .border_color(gpui::transparent_black())
                    .cursor_pointer()
                    .hover(|s| s.bg(ca(89, 252, 179, 0.1)).border_color(c(ACCENT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(p) = this.picker.as_mut() {
                            p.set_open(false);
                        }
                        cx.emit(PickAccount(npub.clone()));
                        cx.notify();
                    }))
                    .child(avatar)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_size(px(14.))
                                    .line_height(px(24.))
                                    .child(r.name.clone().unwrap_or_else(|| crate::t::account_unnamed().into())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .mt(px(1.))
                                    .truncate()
                                    .text_size(px(10.))
                                    .line_height(px(24.))
                                    .text_color(c(0x5b6060))
                                    .child(r.npub.clone()),
                            ),
                    )
            });
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("login-account-list-backdrop")
                        .role(gpui::Role::GenericContainer)
                        .absolute()
                        .inset_0()
                        .bg(ca(3, 3, 3, 0.9))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(p) = this.picker.as_mut() {
                                p.set_open(false);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(top))
                        .left_0()
                        .right_0()
                        .px(px(24.))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("login-account-list")
                                .occlude()
                                .w_full()
                                .max_w(px(420.))
                                .flex()
                                .flex_col()
                                .overflow_hidden()
                                .border_2()
                                .border_color(c(0x030303))
                                .rounded(px(10.))
                                .bg(ca(3, 3, 3, 0.5))
                                .children(rows)
                                .with_animation(
                                    "login-account-list-in",
                                    gpui::Animation::new(std::time::Duration::from_millis(160))
                                        .with_easing(gpui::ease_out_quint()),
                                    |d, t| d.opacity(t).mt(px(-8. * (1. - t))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Commit Add Profile: true when the live account must be torn down first.
    fn commit_adding(&mut self, cx: &mut Context<Self>) -> bool {
        if self.adding != Some(false) {
            return false;
        }
        self.adding = Some(true);
        cx.emit(AddProfile::Committed);
        true
    }

    fn show_error(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.popup = Some(Popup::notice(title));
        self.popup_reauth = false;
        self.popup_reset = false;
        self.popup_bio_setup = false;
        cx.notify();
    }

    fn back_to_start(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.method == "signer" {
            self.signer_leave(cx);
        }
        account::abandon_pending();
        self.step = Step::Start;
        self.select_open = false;
        self.net_refresh(cx);
        cx.notify();
    }

    /// Where the network choice reads from: the backend, or the fixtures' view.
    pub fn net_setup(&mut self, live: bool, fixture: Option<serde_json::Value>, cx: &mut Context<Self>) {
        self.live = live;
        self.net.fixture = fixture;
        self.nip55 = cfg!(target_os = "android") && live && backend::nip55::installed();
        if self.step == Step::Start {
            self.net_refresh(cx);
        }
        if self.unlock && self.step == Step::Encrypt {
            self.offer_biometric(cx);
        }
    }

    /// Add Profile's commit carries the welcome screen's network; anything else waits for it.
    fn gate(&mut self, cx: &mut Context<Self>) -> Result<Option<(Option<String>, Option<serde_json::Value>)>, ()> {
        if self.adding == Some(false) {
            let choice = self.net_choice();
            self.commit_adding(cx);
            cx.notify();
            return Ok(Some(choice));
        }
        if let Err(e) = self.net_ready() {
            self.show_error(e, cx);
            return Err(());
        }
        Ok(None)
    }

    fn create_account(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Ok(add) = self.gate(cx) else { return };
        let rx = backend::run(async move {
            if let Some((kind, opts)) = add {
                account::enter_add_mode(kind, opts).await?;
            }
            let npub = account::create().await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update(cx, |this, cx| match result {
                Ok(_) => this.open_encryption(cx),
                Err(e) => {
                    this.net_settle(cx);
                    this.show_error(e, cx)
                }
            })
            .ok();
        })
        .detach();
    }

    fn import_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key.read(cx).text().trim().to_string();
        if key.is_empty() {
            return;
        }
        self.key.update(cx, |k, cx| k.clear(window, cx));
        let Ok(add) = self.gate(cx) else { return };
        let rx = backend::run(async move {
            if let Some((kind, opts)) = add {
                account::enter_add_mode(kind, opts).await?;
            }
            let imported = account::import(key).await?;
            if matches!(imported, account::Imported::Staged(_)) {
                account::connect().await;
            }
            Ok::<_, String>(imported)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(imported) => this.imported(imported, window, cx),
                Err(e) => {
                    this.net_settle(cx);
                    this.show_error(e, cx)
                }
            })
            .ok();
        })
        .detach();
    }

    /// Sign in through the signer app on this phone: it pairs, then the account picks its
    /// security like any other.
    fn login_nip55(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.nip55_busy {
            return;
        }
        let Ok(add) = self.gate(cx) else { return };
        self.nip55_busy = true;
        cx.notify();
        #[cfg(target_os = "android")]
        let rx = backend::run(async move {
            if let Some((kind, opts)) = add {
                account::enter_add_mode(kind, opts).await?;
            }
            let imported = backend::nip55::login().await?;
            if matches!(imported, account::Imported::Staged(_)) {
                account::connect().await;
            }
            Ok::<_, String>(imported)
        });
        #[cfg(not(target_os = "android"))]
        let rx = {
            drop(add);
            backend::run(async { Err::<account::Imported, String>(t::login_signer_app_missing().into()) })
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update_in(cx, |this, window, cx| {
                this.nip55_busy = false;
                match result {
                    Ok(imported) => this.imported(imported, window, cx),
                    Err(e) => {
                        this.net_settle(cx);
                        this.show_error(e, cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// `.lg-link`: Sign in with Amber, under the key and signer forms.
    fn nip55_link(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.nip55 || self.reauth.is_some() || !matches!(self.method, "key" | "signer") {
            return None;
        }
        let busy = self.nip55_busy;
        Some(
            div()
                .id("lg-nip55")
                .mt(px(14.))
                .text_size(px(11.))
                .text_color(c(PRIMARY))
                .when(busy, |d| d.opacity(0.5))
                .when(!busy, |d| d.cursor_pointer().hover(|s| s.underline()))
                .child(kit::spaced(t::login_nip55(), 1.1))
                .on_click(cx.listener(|this, _, w, cx| this.login_nip55(w, cx)))
                .into_any_element(),
        )
    }

    /// Signed in with a key: a new one picks its security, one already here unlocks as at boot.
    fn imported(
        &mut self,
        imported: account::Imported,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match imported {
            account::Imported::Staged(_) => self.open_encryption(cx),
            account::Imported::Existing(_) => {
                let info = account::boot();
                if info.encrypted {
                    self.unlock = true;
                    self.step = Step::Encrypt;
                    self.start_credential(cred_for(&info.security_type), window, cx);
                    self.offer_biometric(cx);
                } else {
                    self.finish_unlock(None, window, cx);
                }
            }
        }
    }

    /// Sign in with the account another device sent. Resolves once the sign-in has gone through
    /// (or failed), before the security step.
    pub fn finish_transfer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> futures::channel::oneshot::Receiver<Result<(), String>> {
        let (tx, done) = futures::channel::oneshot::channel();
        let add = match self.gate(cx) {
            Ok(add) => add,
            Err(()) => {
                let _ = tx.send(Err(self.net_ready().err().unwrap_or_default()));
                return done;
            }
        };
        let rx = backend::run(backend::transfer::finish(add));
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(imported) => {
                    let _ = tx.send(Ok(()));
                    this.imported(imported, window, cx);
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            })
            .ok();
        })
        .detach();
        done
    }

    /// `openEncryptionFlow(false)`: a new account chooses how its key is kept.
    fn open_encryption(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Encrypt;
        self.unlock = false;
        self.cred = Cred::Choose;
        self.busy = false;
        self.wrong = None;
        cx.notify();
        if cfg!(target_os = "android") && self.live && self.bio_option.is_none() {
            let rx = backend::run(backend::biometric::status());
            cx.spawn(async move |this, cx| {
                let Ok(status) = rx.await else { return };
                if !status.supported {
                    return;
                }
                this.update(cx, |this, cx| {
                    this.bio_option = Some(if status.label.is_empty() {
                        t::security_use_biometrics()
                    } else {
                        status.label.into()
                    });
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// A new account guarded only by the phone's prompt, once its warning is accepted.
    fn commit_biometric(&mut self, cx: &mut Context<Self>) {
        self.set_title(t::login_setting_up(), true, cx);
        #[cfg(target_os = "android")]
        let rx = backend::run(account::commit_biometric());
        #[cfg(not(target_os = "android"))]
        let rx = backend::run(async { Err::<(), String>(backend::biometric::CANCELLED.into()) });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    let npub = vector_core::my_public_key()
                        .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok())
                        .unwrap_or_default();
                    cx.emit(LoggedIn { npub });
                }
                Err(e) => {
                    if e != backend::biometric::CANCELLED {
                        let mut p = Popup::notice(t::security_biometric_failed());
                        p.icon = None;
                        p.text = Some(kit::escape(&e).into());
                        this.popup = Some(p);
                        this.popup_reauth = false;
                    }
                    this.open_encryption(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn choose(&mut self, which: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        match which {
            "pin" => self.start_credential(Cred::Pin, window, cx),
            "password" => self.start_credential(Cred::Password, window, cx),
            _ => {
                self.set_title(t::login_setting_up(), true, cx);
                self.lockless = true;
                self.cred = Cred::Choose;
                self.commit(None, "pin", window, cx);
            }
        }
    }

    /// `encryption_upgrading`: this unlock also moves the key onto a fresh derivation.
    pub fn upgrading(&mut self, total: u64, completed: u64, cx: &mut Context<Self>) {
        if !(self.unlock && self.busy) {
            return;
        }
        self.title = if total > 0 { t::login_strengthening_at(completed * 100 / total) } else { t::login_strengthening() };
        cx.notify();
    }

    fn set_title(&mut self, title: SharedString, busy: bool, cx: &mut Context<Self>) {
        self.title = title;
        self.busy = busy;
        self.error = false;
        self.lockless = false;
        cx.notify();
    }

    /// A refusal in the title's place, until the next entry.
    fn set_error_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        self.set_title(title, false, cx);
        self.error = true;
    }

    fn start_credential(&mut self, cred: Cred, window: &mut Window, cx: &mut Context<Self>) {
        self.cred = cred;
        self.first = None;
        self.pin.clear();
        self.wrong = None;
        let title = match (cred, self.unlock) {
            (Cred::Biometric, _) => t::login_biometric_unlock(),
            (Cred::Pin, true) => t::login_enter_pin(),
            (Cred::Pin, false) => t::login_create_your_pin(),
            (_, true) => t::login_enter_password(),
            (_, false) => t::login_create_your_password(),
        };
        self.set_title(title, false, cx);
        match cred {
            Cred::Pin => window.focus(&self.pin_focus, cx),
            Cred::Biometric => {}
            _ => {
                self.password.update(cx, |p, cx| p.clear(window, cx));
                window.focus(&self.password.focus_handle(cx), cx);
            }
        }
    }

    /// On Android, the phone's prompt beside the pad when this account has a wrapped key, and
    /// straight away for an account that has nothing else.
    fn offer_biometric(&mut self, cx: &mut Context<Self>) {
        if !cfg!(target_os = "android") || !self.live {
            return;
        }
        let rx = backend::run(backend::biometric::status());
        cx.spawn(async move |this, cx| {
            let Ok(status) = rx.await else { return };
            this.update(cx, |this, cx| {
                if !this.unlock || this.step != Step::Encrypt {
                    return;
                }
                let only = this.cred == Cred::Biometric;
                if !status.enrolled {
                    if only {
                        this.biometric_recovery(cx);
                    }
                    return;
                }
                let label: SharedString = if status.label.is_empty() {
                    t::login_biometric_unlock()
                } else {
                    status.label.clone().into()
                };
                if only && !status.label.is_empty() {
                    this.set_title(t::login_biometric_title(status.label.as_str()), false, cx);
                }
                this.bio = Some(label);
                cx.notify();
                if only {
                    this.attempt_biometric(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn attempt_biometric(&mut self, cx: &mut Context<Self>) {
        if self.bio_busy || self.busy || self.bio.is_none() {
            return;
        }
        self.bio_busy = true;
        let (passed_tx, passed_rx) = futures::channel::oneshot::channel::<()>();
        #[cfg(target_os = "android")]
        let rx = backend::run(async move {
            let npub = backend::biometric::unlock(move || {
                let _ = passed_tx.send(());
            })
            .await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        #[cfg(not(target_os = "android"))]
        let rx = {
            drop(passed_tx);
            backend::run(async { Err::<String, String>(backend::biometric::UNAVAILABLE.into()) })
        };
        cx.spawn(async move |this, cx| {
            if passed_rx.await.is_ok() {
                this.update(cx, |this, cx| {
                    this.wrong = None;
                    this.set_title(t::login_decrypting(), true, cx);
                })
                .ok();
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update(cx, |this, cx| {
                this.bio_busy = false;
                let e = match result {
                    Ok(npub) => return cx.emit(LoggedIn { npub }),
                    Err(e) => e,
                };
                let only = this.cred == Cred::Biometric;
                // Past the prompt, the title was showing the unlock at work.
                let passed = this.busy;
                if passed {
                    let resting = match this.cred {
                        Cred::Biometric => t::login_biometric_unlock(),
                        Cred::Password => t::login_enter_password(),
                        _ => t::login_enter_pin(),
                    };
                    this.set_title(resting, false, cx);
                }
                match e.as_str() {
                    backend::biometric::INVALIDATED => {
                        this.bio = None;
                        if only {
                            this.biometric_recovery(cx);
                        } else if this.cred == Cred::Password {
                            this.set_title(t::login_biometric_changed_password(), false, cx);
                        } else {
                            this.set_title(t::login_biometric_changed_pin(), false, cx);
                        }
                    }
                    backend::biometric::UNAVAILABLE => {
                        if only {
                            this.set_title(t::login_biometric_unavailable(), false, cx);
                        }
                    }
                    backend::biometric::CANCELLED => {}
                    _ if only || passed => this.show_error(e, cx),
                    _ => log::warn!("biometric unlock: {e}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The wrap is gone and it was the only way in: reset this device's copy, or leave it be.
    fn biometric_recovery(&mut self, cx: &mut Context<Self>) {
        self.set_title(t::login_biometric_recovery_state(), false, cx);
        self.bio = None;
        let mut p = Popup::notice(t::login_biometric_recovery_title());
        p.text = Some(t::login_biometric_recovery_text());
        p.cancel = Some(t::dialog_cancel());
        p.confirm = t::dialog_confirm();
        self.popup = Some(p);
        self.popup_reauth = false;
        self.popup_reset = true;
        cx.notify();
    }

    fn pin_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let key = e.keystroke.key.as_str();
        if key == "backspace" {
            self.pin.pop();
            self.revert_error_title(cx);
        } else if let Some(ch) = e
            .keystroke
            .key_char
            .as_deref()
            .filter(|k| k.len() == 1 && k.as_bytes()[0].is_ascii_digit())
        {
            if self.pin.len() < 6 {
                self.pin.push_str(ch);
            }
            if self.pin.len() == 6 {
                let pin = Zeroizing::new(std::mem::take(&mut self.pin));
                self.credential_entered(pin, window, cx);
            }
        }
        cx.notify();
    }

    #[cfg(feature = "devctl")]
    pub(crate) fn dev_pin(&mut self, digits: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.cred != Cred::Pin {
            return;
        }
        self.pin.clear();
        self.pin.extend(digits.chars().filter(|c| c.is_ascii_digit()).take(6));
        if self.pin.len() == 6 {
            let pin = Zeroizing::new(std::mem::take(&mut self.pin));
            self.credential_entered(pin, window, cx);
        }
        cx.notify();
    }

    fn revert_error_title(&mut self, cx: &mut Context<Self>) {
        if self.error && self.cred == Cred::Pin {
            let title = if self.unlock {
                t::login_enter_pin()
            } else if self.first.is_some() {
                t::login_reenter_pin()
            } else {
                t::login_create_your_pin()
            };
            self.wrong = None;
            self.set_title(title, false, cx);
        }
    }

    fn submit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.cred != Cred::Password || self.step != Step::Encrypt {
            return;
        }
        let password = Zeroizing::new(self.password.read(cx).text().to_string());
        self.password.update(cx, |p, cx| p.clear(window, cx));
        if self.unlock && password.is_empty() {
            return;
        }
        if !self.unlock && self.first.is_none() && password.chars().count() < 4 {
            self.set_error_title(t::login_password_short(), cx);
            return;
        }
        self.credential_entered(password, window, cx);
    }

    /// A full PIN or a submitted password: unlock, or the first or confirming entry of a new one.
    fn credential_entered(
        &mut self,
        value: Zeroizing<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pin = self.cred == Cred::Pin;
        if self.unlock {
            self.wrong = None;
            self.set_title(t::login_decrypting(), true, cx);
            self.finish_unlock(Some(value), window, cx);
            return;
        }
        match self.first.take() {
            None => {
                self.first = Some(value);
                self.set_title(if pin { t::login_reenter_pin() } else { t::login_reenter_password() }, false, cx);
            }
            Some(first) if *first == *value => {
                self.set_title(t::login_encrypting(), true, cx);
                self.commit(
                    Some(first),
                    if pin { "pin" } else { "password" },
                    window,
                    cx,
                );
            }
            Some(_) => {
                self.set_error_title(if pin { t::login_pin_mismatch() } else { t::login_password_mismatch() }, cx);
            }
        }
    }

    fn commit(
        &mut self,
        password: Option<Zeroizing<String>>,
        security: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = backend::run(async move { account::commit(password, security).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    let npub = vector_core::my_public_key()
                        .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok())
                        .unwrap_or_default();
                    cx.emit(LoggedIn { npub });
                }
                Err(e) => {
                    let mut p = Popup::notice(if security == "password" {
                        t::login_save_password_failed()
                    } else {
                        t::login_save_pin_failed()
                    });
                    p.icon = None;
                    p.text = Some(e.into());
                    this.popup = Some(p);
                    this.popup_reauth = false;
                    this.open_encryption(cx);
                    let _ = window;
                }
            })
            .ok();
        })
        .detach();
    }

    fn finish_unlock(
        &mut self,
        password: Option<Zeroizing<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = backend::run(async move {
            let npub = account::unlock(password).await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(t::backend_stopped().into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(npub) => cx.emit(LoggedIn { npub }),
                Err(e) if e.contains("Incorrect") => { // i18n-exempt: core's words, matched
                    let pin = this.cred == Cred::Pin;
                    this.set_error_title(if pin { t::login_pin_incorrect() } else { t::login_password_incorrect() }, cx);
                    this.wrong = Some(if pin { "pin" } else { "password" });
                    this.pin.clear();
                    if pin {
                        window.focus(&this.pin_focus, cx);
                    } else {
                        window.focus(&this.password.focus_handle(cx), cx);
                    }
                }
                Err(e) => {
                    this.set_title(if this.cred == Cred::Pin { t::login_enter_pin() } else { t::login_enter_password() }, false, cx);
                    if !this.offer_reauth(&e, cx) {
                        this.show_error(e, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

// ── Painting ────────────────────────────────────────────────────────────────────────────

fn lg_btn(id: &'static str, label: SharedString, kind: u8) -> gpui::Stateful<gpui::Div> {
    // kind: 0 plain, 1 primary, 2 accent
    let base = div()
        .id(id)
        .w(px(250.))
        .h(px(50.))
        .flex()
        .items_center()
        .justify_center()
        .px(px(16.))
        .border_1()
        .rounded(px(7.))
        .text_size(px(20.))
        .cursor_pointer()
        .child(label);
    match kind {
        1 => base
            .border_color(c(PRIMARY))
            .bg(c(PRIMARY))
            .text_color(c(APP_BG))
            .hover(|s| s.bg(c(0x3be9a4))),
        2 => base
            .border_color(c(PRIMARY))
            .text_color(c(0xffffff))
            .hover(|s| s.bg(ca(51, 219, 152, 0.08))),
        _ => base
            .border_color(c(0x393b3c))
            .text_color(c(0xffffff))
            .hover(|s| s.border_color(c(0x5b6060))),
    }
}

fn lg_title(text: SharedString, lock: bool, color: u32) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .mb(px(25.))
        .when(lock, |d| {
            d.child(img("icons/login/lock.svg").w(px(22.)).h(px(29.)))
        })
        .child(
            kit::spaced(&text, 1.6)
                .font_weight(FontWeight::BOLD)
                .text_size(px(16.))
                .line_height(px(24.))
                .text_color(c(color)),
        )
}

/// A prompt with its `<back>` link.
fn lg_back(text: SharedString, on: impl Fn(&mut Window, &mut App) + 'static) -> gpui::Div {
    let (prompt, label, after) = kit::around(&text, "back");
    let on = std::rc::Rc::new(on);
    let pieces = kit::visual(vec![(prompt, false), (label, true), (after, false)]);
    div()
        .mt(px(25.))
        .flex()
        .items_baseline()
        .keep_ltr()
        .text_size(px(11.))
        .line_height(px(24.))
        .text_color(c(ICON_MUTED))
        .children(pieces.into_iter().filter(|p| !p.0.is_empty()).enumerate().map(|(i, (text, link))| {
            if link {
                let on = on.clone();
                div()
                    .id(("lg-back", i))
                    .cursor_pointer()
                    .text_color(c(PRIMARY))
                    .on_click(move |_, w, cx| on(w, cx))
                    .child(kit::spaced(text, 1.1))
                    .into_any_element()
            } else {
                kit::spaced(text, 1.1).into_any_element()
            }
        }))
}

impl LoginView {
    fn field(
        &self,
        input: Entity<TextInput>,
        eye: bool,
        go: SharedString,
        on_go: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w(px(306.))
                    .h(px(40.))
                    .border_2()
                    .border_color(c(PRIMARY))
                    .rounded(px(7.))
                    .bg(c(APP_BG))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl(px(11.))
                            .flex()
                            .items_center()
                            .child(input),
                    )
                    .when(eye, |d| {
                        d.child(
                            div()
                                .id("lg-eye")
                                .aria_label(if self.reveal { t::login_hide_password() } else { t::login_show_password() })
                                .h_full()
                                .px(px(6.))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reveal = !this.reveal;
                                    let reveal = this.reveal;
                                    this.password.update(cx, |p, cx| p.set_masked(!reveal, cx));
                                    window.focus(&this.password.focus_handle(cx), cx);
                                }))
                                .child(if self.reveal {
                                    svg()
                                        .path("icons/eye-off.svg")
                                        .w(px(22.))
                                        .h(px(18.))
                                        .text_color(c(0x959899))
                                        .into_any_element()
                                } else {
                                    img("icons/login/eye.svg")
                                        .w(px(22.))
                                        .h(px(16.))
                                        .into_any_element()
                                }),
                        )
                    })
                    .child(
                        div()
                            .id("lg-go")
                            .aria_label(go)
                            .h_full()
                            .pl(px(4.))
                            .pr(px(6.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_click(
                                cx.listener(move |this, _, window, cx| on_go(this, window, cx)),
                            )
                            .child(img("icons/login/enter.svg").size(px(22.))),
                    ),
            )
    }

    fn sign_in_with(&self, cx: &mut Context<Self>) -> gpui::Div {
        let entity = cx.entity();
        let toggle = {
            let entity = entity.clone();
            move |_: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.select_open = !this.select_open;
                    cx.notify();
                })
            }
        };
        let pick = move |v: &'static str, window: &mut Window, cx: &mut App| {
            entity.update(cx, |this, cx| {
                this.select_open = false;
                if this.method == "signer" && v != "signer" {
                    this.signer_leave(cx);
                }
                this.method = v;
                if v == "key" {
                    window.focus(&this.key.focus_handle(cx), cx);
                } else if v == "signer" {
                    window.focus(&this.signer.url.focus_handle(cx), cx);
                }
                cx.notify();
            })
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .w(px(300.))
            .mt(px(24.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(16.))
                    .line_height(px(19.))
                    .child(t::login_sign_in_with())
                    .child(
                        div()
                            .id("lg-method-info")
                            .aria_label(t::login_sign_in_with_about())
                            .cursor_pointer()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(MethodInfo)))
                            .child(
                                svg()
                                    .path("icons/info.svg")
                                    .size(px(18.))
                                    .text_color(c(0x333333)),
                            ),
                    ),
            )
            .child(kit::select(
                "lg-method",
                150.,
                methods(),
                self.method,
                self.select_open,
                toggle,
                pick,
            ))
    }

    fn stage(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.step {
            Step::Start => div()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(13.))
                        .relative()
                        .child(
                            lg_btn("lg-create", t::login_create_account(), 1)
                                .when(self.net.hold, |d| d.bg(ca(51, 219, 152, 0.25)).border_color(gpui::transparent_black()))
                                .on_click(cx.listener(|this, _, w, cx| this.create_account(w, cx))),
                        )
                        .child(lg_btn("lg-login", t::login_login(), 2)
                            .when(self.net.hold, |d| d.border_color(ca(51, 219, 152, 0.25)).text_color(white_a(0.25)))
                            .on_click(cx.listener(|this, _, w, cx| {
                            if let Err(e) = this.net_ready() {
                                return this.show_error(e, cx);
                            }
                            this.method = "qr";
                            this.step = Step::Import;
                            let _ = w;
                            cx.notify();
                        })))
                        // Held: nothing in the group takes a click or a hover.
                        .when(self.net.hold, |d| d.child(div().id("lg-held").absolute().inset_0().occlude())),
                )
                .children(self.net_row(cx))
                .children(self.net_line(cx))
                .when(self.adding.is_some(), |d| {
                    let view = cx.entity();
                    let reserve = self.net.shown;
                    d.child(
                        lg_back(t::login_back_changed_mind(), move |_, cx| view.update(cx, |_, cx| cx.emit(AddProfile::Back)))
                        .when(reserve, |d| d.mt(px(10.)))
                        .when(self.net.busy, |d| d.opacity(0.4).block_mouse_except_scroll()),
                    )
                })
                .into_any_element(),
            Step::Import => {
                let back = cx.entity();
                let mut block = div().flex().flex_col().items_center();
                if self.method == "signer" {
                    block = block.child(self.signer_block(cx));
                } else if self.method == "key" {
                    block = block
                        .child(lg_title(t::login_enter_key(), true, 0xffffff))
                        .child(self.field(
                            self.key.clone(),
                            false,
                            t::login_login(),
                            |this, w, cx| this.import_key(w, cx),
                            cx,
                        ));
                } else {
                    block = block
                        .child(lg_title(t::login_with_qr(), true, 0xffffff))
                        .child(
                            lg_btn("lg-device", t::login_another_device(), 1)
                                .w(px(300.))
                                .text_size(px(18.))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenTransfer))),
                        );
                }
                if self.reauth.is_some() {
                    return block
                        .child(lg_back(t::login_back_changed_mind(), move |_, cx| back.update(cx, |this, cx| this.end_reauth(cx))))
                        .into_any_element();
                }
                let link = self.nip55_link(cx);
                block
                    .child(self.sign_in_with(cx))
                    .child(lg_back(t::login_back_change_accounts(), move |w, cx| back.update(cx, |this, cx| this.back_to_start(w, cx))))
                    .children(link)
                    .into_any_element()
            }
            Step::Encrypt => self.encrypt(window, cx),
        }
    }

    fn encrypt(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut block = div().flex().flex_col().items_center();
        if self.cred == Cred::Choose && !self.busy {
            return block
                .gap(px(16.))
                .child(
                    lg_btn("lg-skip", t::login_skip_encryption(), 0)
                        .on_click(cx.listener(|this, _, w, cx| this.choose("skip", w, cx))),
                )
                .child(
                    lg_btn("lg-password", t::login_create_password(), 0)
                        .on_click(cx.listener(|this, _, w, cx| this.choose("password", w, cx))),
                )
                .child(
                    div()
                        .my(px(-4.))
                        .text_size(px(10.))
                        .line_height(px(24.))
                        .text_color(c(ACCENT))
                        .child(if self.bio_option.is_some() { t::login_recommended_options() } else { t::login_recommended() }),
                )
                .child(
                    lg_btn("lg-pin", t::login_create_pin(), 1)
                        .on_click(cx.listener(|this, _, w, cx| this.choose("pin", w, cx))),
                )
                .children(self.bio_option.clone().map(|label| {
                    lg_btn("lg-bio-option", label, 1).on_click(cx.listener(|this, _, _, cx| {
                        this.popup = Some(crate::security::biometric_warning());
                        this.popup_reauth = false;
                        this.popup_bio_setup = true;
                        cx.notify();
                    }))
                }))
                .into_any_element();
        }
        if let Some(what) = self.wrong {
            let aggro = div()
                    .id("lg-aggro")
                    .alert()
                    .flex()
                    .flex_col()
                    .items_center()
                    .mb(px(25.))
                    .child(
                        div()
                            .relative()
                            .w(px(172.))
                            .h(px(179.))
                            .child(
                                img("icons/login/aggroboi.svg")
                                    .absolute()
                                    .left_0()
                                    .bottom_0()
                                    .size(px(136.)),
                            )
                            .child(
                                img("icons/login/warning.svg")
                                    .absolute()
                                    .right(px(-4.))
                                    .top(px(-4.))
                                    .w(px(79.))
                                    .h(px(70.)),
                            ),
                    )
                    .child(
                        kit::spaced(if what == "pin" { t::login_wrong_pin() } else { t::login_wrong_password() }, 1.4)
                            .mt(px(14.))
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(c(0xff2ea9)),
                    )
                    .child(
                        kit::spaced(
                            if what == "pin" { t::login_correct_pin() } else { t::login_correct_password() },
                            2.4,
                        )
                        .mt(px(8.))
                        .text_size(px(24.))
                        .font_weight(FontWeight::BOLD),
                    );
            // `lgAggroIn`: up from 0.9 with a little overshoot, over 0.32 s.
            let ease = crate::kit::bezier(0.34, 1.56, 0.64, 1.);
            block = block.child(gpui::zoom(aggro).with_animation(
                "lg-aggro-in",
                gpui::Animation::new(std::time::Duration::from_millis(320)),
                move |z, t| {
                    let e = ease(t);
                    z.scale(0.9 + 0.1 * e).opacity(e.min(1.))
                },
            ));
        } else if self.busy {
            window.request_animation_frame();
            let phase = (self.born.elapsed().as_secs_f32() / 6.).fract();
            block = block.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .mb(px(25.))
                    // An account left unencrypted has no lock to show.
                    .when(!self.lockless, |d| {
                        d.child(img("icons/login/lock.svg").w(px(22.)).h(px(29.)))
                    })
                    .child(
                        kit::gradient_text(&self.title, 1.6, phase)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(16.))
                            .line_height(px(24.)),
                    ),
            );
        } else {
            block = block.child(lg_title(
                self.title.clone(),
                true,
                if self.error { 0xff2ea9 } else { 0xffffff },
            ));
        }
        if self.busy {
            return block.into_any_element();
        }
        match self.cred {
            Cred::Pin => {
                let focused = self.pin_focus.is_focused(window);
                let mut row = div()
                    .id("lg-pins")
                    .track_focus(&self.pin_focus)
                    .on_key_down(cx.listener(Self::pin_key))
                    .keep_ltr()
                    .flex()
                    .gap(px(11.7));
                for i in 0..6 {
                    let filled = i < self.pin.len();
                    let active = focused && i == self.pin.len().min(5);
                    row = row.child(
                        div()
                            .size(px(40.))
                            .border_2()
                            .border_color(c(if active { PRIMARY } else { 0x5b6060 }))
                            .rounded(px(7.))
                            .bg(c(APP_BG))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(20.))
                            .when(filled, |d| d.child("\u{2022}"))
                            .when(active && !filled, |d| d.child(div().w(px(2.)).h(px(23.)).bg(c(PRIMARY)))),
                    );
                }
                block = block.child(row);
            }
            Cred::Password => {
                block = block.child(self.field(
                    self.password.clone(),
                    true,
                    t::login_continue(),
                    |this, w, cx| this.submit_password(w, cx),
                    cx,
                ));
            }
            Cred::Choose | Cred::Biometric => {}
        }
        if let Some(label) = self.bio.clone().filter(|_| !self.bio_busy) {
            block = block.child(
                lg_btn("lg-bio", label, 0)
                    .mt(px(18.))
                    .gap(px(8.))
                    .text_size(px(16.))
                    .child(svg().path("ui/fingerprint.svg").size(px(20.)).flex_none().text_color(c(0xffffff)))
                    .on_click(cx.listener(|this, _, _, cx| this.attempt_biometric(cx))),
            );
        }
        block.into_any_element()
    }
}

/// The unlock step an account's `security_type` asks for.
fn cred_for(security: &str) -> Cred {
    match security {
        "password" => Cred::Password,
        "biometric" => Cred::Biometric,
        _ => Cred::Pin,
    }
}

impl Render for LoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wide = window.viewport_size().width >= px(900.);
        let vw = f32::from(window.viewport_size().width);
        let (lock_w, lock_h) = lockup_size(vw);
        // Under 640 the background toggle joins the footer's column, a grid row of its own.
        let stacked = vw <= 640.;
        let toggle_row = if stacked { 34. } else { 0. };
        let aggro = self.wrong.is_some();
        let stage = self.stage(window, cx);
        let account_list = self.account_list(51. + lock_h + 23. + 52., cx);
        let mut root = div()
            .size_full()
            .relative()
            .font_family(RUBIK)
            .text_color(c(0xffffff))
            .bg(c(APP_BG));

        // .lg-art: fades with the Background toggle.
        let fade = self.bg_flip.map_or(1., |t| (t.elapsed().as_secs_f32() / BG_FADE).min(1.));
        if fade < 1. {
            window.request_animation_frame();
        }
        let shown = if self.bg_hidden { 1. - kit::bezier(0.25, 0.1, 0.25, 1.)(fade) } else { kit::bezier(0.25, 0.1, 0.25, 1.)(fade) };
        if shown > 0. {
            root = root.child(art(shown));
        }
        if aggro {
            let face = |src: &'static str, l: f32, t: f32, w: f32, h: f32| {
                img(src)
                    .absolute()
                    .left(gpui::relative(l))
                    .top(gpui::relative(t))
                    .w(px(w))
                    .h(px(h))
            };
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .child(face("icons/login/aggro-bg-1.svg", 0.494, 0.669, 226., 224.))
                    .child(face("icons/login/aggro-bg-2.svg", 0.02, 0.122, 128., 127.))
                    .child(face("icons/login/aggro-bg-3.svg", 0.857, 0.445, 98., 97.))
                    .child(face("icons/login/aggro-bg-4.svg", 0.051, 0.823, 60., 59.))
                    // `.lg-aggro-bg`: fades in over 0.4 s.
                    .with_animation("lg-aggro-bg", gpui::Animation::new(std::time::Duration::from_millis(400)).with_easing(gpui::ease_in_out), |d, t| d.opacity(t)),
            );
        }

        root.child(head(lock_w, lock_h))
        .child(
            // .lg-stage: centred in the window.
            div().absolute().top_0().left_0().right_0().bottom(px(toggle_row)).flex().flex_col().items_center().justify_center().px(px(16.)).py(px(24.)).child(stage),
        )
        .child(foot(wide, stacked, toggle_row))
        .child(bg_toggle(self.bg_hidden, stacked, vw).on_click(cx.listener(|this, _, _, cx| {
            this.bg_hidden = !this.bg_hidden;
            // A flip mid-fade turns back from where it stands.
            let left = this.bg_flip.map_or(0., |t| (1. - t.elapsed().as_secs_f32() / BG_FADE).max(0.));
            this.bg_flip = Some(Instant::now() - std::time::Duration::from_secs_f32(left * BG_FADE));
            cx.notify();
        })))
        .children(account_list)
        .children(self.net_modal(vw <= 480., cx))
        .children(self.signer_qr(cx))
        .children(self.pill(cx).map(|pill| div().absolute().top(px(51. + lock_h + 23.)).left_0().right_0().px(px(24.)).flex().justify_center().child(pill)))
        // Above the footer's credit and links, clear of the window's own controls.
        .children(self.offer_pill(cx).map(|pill| {
            div().absolute().bottom(px(if stacked { toggle_row + 6. } else { 25. } + 72.)).left_0().right_0().px(px(16.)).flex().justify_center().child(pill)
        }))
        .when_some(self.popup.clone(), |root, p| {
            let entity = cx.entity();
            root.child(kit::popup(&p, move |yes, window, cx| {
                entity.update(cx, |this, cx| {
                    this.popup = None;
                    if std::mem::take(&mut this.popup_reauth) && yes {
                        this.start_reauth(true, window, cx);
                    }
                    if std::mem::take(&mut this.popup_bio_setup) && yes {
                        this.commit_biometric(cx);
                    }
                    if std::mem::take(&mut this.popup_reset) && yes {
                        let npub = vector_core::db::get_current_account().unwrap_or_default();
                        cx.emit(ResetAccount(npub));
                    }
                    cx.notify();
                })
            }))
        })
    }
}

/// `.lg-lockup`: min(429.72px, 100vw - 48px), height following.
fn lockup_size(vw: f32) -> (f32, f32) {
    let w = 429.72f32.min(vw - 48.);
    (w, 44. * w / 429.72)
}

/// `.lg-art`: the camera, faded out towards the centre.
fn art(shown: f32) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .opacity(shown)
        .child(
            img("icons/login/cctv.png")
                .absolute()
                .left(gpui::relative(-0.0935))
                .top(gpui::relative(-0.1508))
                .w(gpui::relative(1.1679))
                .h(gpui::relative(1.1679))
                .object_fit(ObjectFit::Cover)
                .opacity(0.2),
        )
        .child(div().absolute().inset_0().bg(linear_gradient(
            90.,
            linear_color_stop(c(APP_BG), 0.5719),
            linear_color_stop(ca(3, 3, 3, 0.), 0.9156),
        )))
}

/// `.lg-head`
fn head(lock_w: f32, lock_h: f32) -> gpui::Div {
    div().absolute().top(px(51.)).left_0().right_0().flex().justify_center().child(img("icons/login/lockup.svg").w(px(lock_w)).h(px(lock_h)))
}

/// `.lg-foot`: the credits and links.
fn foot(wide: bool, stacked: bool, toggle_row: f32) -> gpui::Div {
    let link = |id: &'static str, label: SharedString, url: &'static str| {
        div()
            .id(id)
            .cursor_pointer()
            .text_color(c(PRIMARY))
            .hover(|s| s.text_color(c(ACCENT)))
            .on_click(move |_, _, cx| cx.open_url(url))
            .child(kit::spaced(label, 0.8))
    };
    let sep = || kit::spaced("|", 0.8);
    div()
        .absolute()
        .bottom(px(if stacked { toggle_row + 6. } else { 25. }))
        .left_0()
        .right_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(17.))
        .child(
            div()
                .relative()
                .w(px(173.78))
                .h(px(10.47))
                .child(img("icons/login/credit-text.svg").size_full())
                .child(img("icons/login/credit-mark.svg").absolute().left(px(77.7)).top(px(-5.6)).w(px(13.16)).h(px(20.))),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(if wide { 12. } else { 8. }))
                .line_height(px(24.))
                .text_color(c(ICON_MUTED))
                .child(link("lk-web", t::login_link_website(), "https://vectorapp.io"))
                .child(sep())
                .child(link("lk-docs", t::login_link_docs(), "https://docs.vectorapp.io"))
                .child(sep())
                .child(link("lk-privacy", t::login_link_privacy(), "https://vectorapp.io/privacy-policy"))
                .child(sep())
                .child(link("lk-donate", t::login_link_donate(), "https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/more/donations")),
        )
}

/// `.lg-bg-toggle`
fn bg_toggle(hidden: bool, stacked: bool, vw: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id("lg-bg-toggle")
        .absolute()
        .map(|d| if stacked { d.left(px((vw - 114.) / 2.)).bottom(px(10.)) } else { d.right(px(12.)).bottom(px(14.)) })
        .w(px(114.))
        .h(px(24.))
        .p(px(4.))
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(8.))
        .line_height(px(8.))
        .text_color(c(CHROME_HELP))
        .hover(|s| s.text_color(c(0x5b6060)))
        .cursor_pointer()
        .child(div().w(px(17.)).h(px(16.)).flex().items_center().child(if hidden {
            img("icons/login/eye.svg").w(px(17.)).h(px(12.))
        } else {
            img("icons/login/hide.svg").w(px(17.)).h(px(16.))
        }))
        .child(kit::spaced(if hidden { t::login_show_background() } else { t::login_hide_background() }, 0.8))
}

/// The login frame while the account is found on disk (`.lg-booting`): three dots that fade in
/// after 0.35 s and pulse in turn.
pub fn booting(since: Instant, window: &mut Window) -> AnyElement {
    let vw = f32::from(window.viewport_size().width);
    let wide = window.viewport_size().width >= px(900.);
    let stacked = vw <= 640.;
    let toggle_row = if stacked { 34. } else { 0. };
    let (lock_w, lock_h) = lockup_size(vw);
    let t = since.elapsed().as_secs_f32();
    let ease = kit::bezier(0.25, 0.1, 0.25, 1.);
    let swing = kit::bezier(0.42, 0., 0.58, 1.);
    let shown = ease(((t - 0.35) / 0.4).clamp(0., 1.));
    let dots = (shown > 0.).then(|| {
        window.request_animation_frame();
        div().flex().gap(px(8.)).opacity(shown).children([0., 0.15, 0.3].map(|delay: f32| {
            // `lg-booting-pulse`: 0.2 → 0.9 at 40 %, back by 80 %, then rest.
            let p = ((t - delay).max(0.) % 1.2) / 1.2;
            let k = if p < 0.4 { swing(p / 0.4) } else if p < 0.8 { 1. - swing((p - 0.4) / 0.4) } else { 0. };
            let size = 7. * (0.8 + 0.2 * k);
            div().size(px(7.)).flex().items_center().justify_center().child(
                div().size(px(size)).rounded_full().bg(c(PRIMARY)).opacity(0.2 + 0.7 * k),
            )
        }))
    });
    div()
        .size_full()
        .relative()
        .font_family(RUBIK)
        .text_color(c(0xffffff))
        .bg(c(APP_BG))
        .child(art(1.))
        .child(head(lock_w, lock_h))
        .child(
            div().absolute().top_0().left_0().right_0().bottom(px(toggle_row)).flex().items_center().justify_center().children(dots),
        )
        .child(foot(wide, stacked, toggle_row))
        .child(bg_toggle(false, stacked, vw))
        .into_any_element()
}

impl Focusable for LoginView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.pin_focus.clone()
    }
}
