//! The login screen (`#login-form`, components/auth/LoginScreen.svelte): Start, Import and the
//! encryption steps (choose, PIN or password; set up or unlock), painted on the design's frame:
//! the lockup on top, one centred step, the credits along the bottom, the art behind.

use std::time::Instant;

use gpui::{
    AnimationExt, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent,
    ObjectFit, ParentElement, Render, SharedString, Styled, Window, div, img, linear_color_stop, linear_gradient,
    prelude::*, px, svg,
};
use zeroize::Zeroizing;

use crate::backend::{self, account};
use crate::composer::{Look, SendText, TextInput};
use crate::kit::{self, Choice, Popup};
use crate::theme::*;

/// The account is open: the app can boot.
pub struct LoggedIn {
    pub npub: String,
}

impl EventEmitter<LoggedIn> for LoginView {}

/// Add Profile's milestones: the live account torn down for the new one, or the user backing out.
pub enum AddProfile {
    Committed,
    Back,
}

impl EventEmitter<AddProfile> for LoginView {}

/// Another account on this device was picked from the pill: open it instead.
pub struct PickAccount(pub String);

impl EventEmitter<PickAccount> for LoginView {}

/// The pre-login account pill (`#login-account-picker`) and its list of the other accounts.
struct Picker {
    rows: Vec<crate::switcher::Row>,
    active: Option<String>,
    open: bool,
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
}

const METHODS: &[Choice] = &[
    Choice { value: "qr", label: "QR Code" },
    Choice { value: "key", label: "Private Key" },
    Choice { value: "signer", label: "Remote Signer" },
];

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
    /// The credential just refused: Aggroboi glares in the title's place.
    wrong: Option<&'static str>,
    /// The first entry of a new credential, awaiting its confirmation.
    first: Option<Zeroizing<String>>,
    bg_hidden: bool,
    popup: Option<Popup>,
    born: Instant,
    /// Adding a profile over a live account: whether that account was already torn down.
    adding: Option<bool>,
    picker: Option<Picker>,
}

const DECRYPTING: &str = "Decrypting your keys...";
const ENCRYPTING: &str = "Encrypting your keys...";

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
        let key = cx.new(|cx| TextInput::new("Enter nsec or Seed Phrase...", cx).with_look(field.clone()).keep_on_submit());
        let password = cx.new(|cx| TextInput::new("Enter Password...", cx).with_look(field).keep_on_submit());
        cx.subscribe_in(&key, window, |this, _, _: &SendText, window, cx| this.import_key(window, cx)).detach();
        cx.subscribe_in(&password, window, |this, _, _: &SendText, window, cx| this.submit_password(window, cx)).detach();
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
            wrong: None,
            first: None,
            bg_hidden: false,
            popup: None,
            born: Instant::now(),
            adding: None,
            picker: None,
        };
        if let Some(security) = unlock {
            view.unlock = true;
            view.step = Step::Encrypt;
            view.start_credential(if security == "password" { Cred::Password } else { Cred::Pin }, window, cx);
        }
        view
    }

    /// The welcome screen over a live account, which stays until Create or Login commits.
    pub fn adding(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(None, window, cx);
        view.adding = Some(false);
        view
    }

    /// The device's accounts, for the pill: it shows only when there is a choice to make.
    pub fn set_accounts(&mut self, rows: Vec<crate::switcher::Row>, active: Option<String>, open: bool, cx: &mut Context<Self>) {
        self.picker = (rows.len() >= 2 && self.adding.is_none()).then(|| {
            let active = active.filter(|a| rows.iter().any(|r| &r.npub == a));
            Picker { rows, active, open }
        });
        cx.notify();
    }

    fn pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.picker.as_ref()?;
        let me = p.active.as_ref().and_then(|a| p.rows.iter().find(|r| &r.npub == a));
        let label = match me {
            Some(r) => r.name.clone().unwrap_or_else(|| r.npub.clone()),
            None => "Select Profile".into(),
        };
        let avatar = match me.and_then(|r| r.avatar.clone()) {
            Some(path) => img(path).size(px(44.)).rounded_full().flex_shrink_0().into_any_element(),
            None => img("icons/user-placeholder.svg").size(px(44.)).rounded_full().flex_shrink_0().into_any_element(),
        };
        let chevron = svg().path("icons/login/chevron.svg").w(px(10.)).h(px(6.)).ml(px(2.)).flex_shrink_0().text_color(c(0x474747));
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
                            p.open = !p.open;
                        }
                        cx.notify();
                    }))
                })
                .child(avatar)
                .child(kit::spaced(&label, 2.).min_w_0().truncate().font_weight(FontWeight::BOLD).text_size(px(20.)).line_height(px(24.)))
                .child(if p.open { chevron.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI))) } else { chevron })
                .into_any_element(),
        )
    }

    /// `.login-account-list`: the other accounts, below the pill over a dimmed screen.
    fn account_list(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.picker.as_ref().filter(|p| p.open)?;
        let rows = p.rows.iter().filter(|r| p.active.as_ref() != Some(&r.npub)).map(|r| {
            let npub = r.npub.clone();
            let avatar = match &r.avatar {
                Some(path) => img(path.clone()).size(px(28.)).rounded_full().flex_shrink_0().into_any_element(),
                None => img("icons/user-placeholder.svg").size(px(28.)).rounded_full().flex_shrink_0().into_any_element(),
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
                        p.open = false;
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
                        .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_size(px(14.)).line_height(px(24.)).child(r.name.clone().unwrap_or_else(|| "Unnamed".into())))
                        .child(div().flex_1().min_w_0().mt(px(1.)).truncate().text_size(px(10.)).line_height(px(24.)).text_color(c(0x5b6060)).child(r.npub.clone())),
                )
        });
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    div().id("login-account-list-backdrop").absolute().inset_0().bg(ca(3, 3, 3, 0.9)).on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = this.picker.as_mut() {
                            p.open = false;
                        }
                        cx.notify();
                    })),
                )
                .child(
                    div().absolute().top(px(170.)).left_0().right_0().flex().justify_center().child(
                        div()
                            .id("login-account-list")
                            .occlude()
                            .w(px(420.))
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .border_2()
                            .border_color(c(0x030303))
                            .rounded(px(10.))
                            .bg(ca(3, 3, 3, 0.5))
                            .children(rows)
                            .with_animation("login-account-list-in", gpui::Animation::new(std::time::Duration::from_millis(160)).with_easing(gpui::ease_out_quint()), |d, t| {
                                d.opacity(t).mt(px(-8. * (1. - t)))
                            }),
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
        cx.notify();
    }

    fn back_to_start(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        account::abandon_pending();
        self.step = Step::Start;
        self.select_open = false;
        cx.notify();
    }

    fn create_account(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let reset = self.commit_adding(cx);
        let rx = backend::run(async move {
            if reset {
                account::reset().await;
            }
            let npub = account::create().await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update(cx, |this, cx| match result {
                Ok(_) => this.open_encryption(cx),
                Err(e) => this.show_error(e, cx),
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
        let reset = self.commit_adding(cx);
        let rx = backend::run(async move {
            if reset {
                account::reset().await;
            }
            let imported = account::import(key).await?;
            if matches!(imported, account::Imported::Staged(_)) {
                account::connect().await;
            }
            Ok::<_, String>(imported)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(account::Imported::Staged(_)) => this.open_encryption(cx),
                // Already on this device: unlock it as a boot would.
                Ok(account::Imported::Existing(_)) => {
                    let info = account::boot();
                    if info.encrypted {
                        this.unlock = true;
                        this.step = Step::Encrypt;
                        this.start_credential(if info.security_type == "password" { Cred::Password } else { Cred::Pin }, window, cx);
                    } else {
                        this.finish_unlock(None, window, cx);
                    }
                }
                Err(e) => this.show_error(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// `openEncryptionFlow(false)`: a new account chooses how its key is kept.
    fn open_encryption(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Encrypt;
        self.unlock = false;
        self.cred = Cred::Choose;
        self.busy = false;
        self.wrong = None;
        cx.notify();
    }

    fn choose(&mut self, which: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        match which {
            "pin" => self.start_credential(Cred::Pin, window, cx),
            "password" => self.start_credential(Cred::Password, window, cx),
            _ => {
                self.set_title("Setting up your account...", true, cx);
                self.cred = Cred::Choose;
                self.commit(None, "pin", window, cx);
            }
        }
    }

    fn set_title(&mut self, title: &'static str, busy: bool, cx: &mut Context<Self>) {
        self.title = title.into();
        self.busy = busy;
        self.error = title.contains("Incorrect") || title.contains("match") || title.contains("at least");
        cx.notify();
    }

    fn start_credential(&mut self, cred: Cred, window: &mut Window, cx: &mut Context<Self>) {
        self.cred = cred;
        self.first = None;
        self.pin.clear();
        self.wrong = None;
        let title = match (cred, self.unlock) {
            (Cred::Pin, true) => "Enter Your Decryption Pin",
            (Cred::Pin, false) => "Create Your Pin",
            (_, true) => "Enter Your Password",
            (_, false) => "Create Your Password",
        };
        self.set_title(title, false, cx);
        match cred {
            Cred::Pin => window.focus(&self.pin_focus, cx),
            _ => {
                self.password.update(cx, |p, cx| p.clear(window, cx));
                window.focus(&self.password.focus_handle(cx), cx);
            }
        }
    }

    fn pin_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let key = e.keystroke.key.as_str();
        if key == "backspace" {
            self.pin.pop();
            self.revert_error_title(cx);
        } else if let Some(ch) = e.keystroke.key_char.as_deref().filter(|k| k.len() == 1 && k.as_bytes()[0].is_ascii_digit()) {
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

    fn revert_error_title(&mut self, cx: &mut Context<Self>) {
        if self.error && self.cred == Cred::Pin {
            let title = if self.unlock {
                "Enter Your Decryption Pin"
            } else if self.first.is_some() {
                "Re-enter Your Pin"
            } else {
                "Create Your Pin"
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
            self.set_title("Password must be at least 4 characters", false, cx);
            return;
        }
        self.credential_entered(password, window, cx);
    }

    /// A full PIN or a submitted password: unlock, or the first or confirming entry of a new one.
    fn credential_entered(&mut self, value: Zeroizing<String>, window: &mut Window, cx: &mut Context<Self>) {
        let pin = self.cred == Cred::Pin;
        if self.unlock {
            self.wrong = None;
            self.set_title(DECRYPTING, true, cx);
            self.finish_unlock(Some(value), window, cx);
            return;
        }
        match self.first.take() {
            None => {
                self.first = Some(value);
                self.set_title(if pin { "Re-enter Your Pin" } else { "Re-enter Your Password" }, false, cx);
            }
            Some(first) if *first == *value => {
                self.set_title(ENCRYPTING, true, cx);
                self.commit(Some(first), if pin { "pin" } else { "password" }, window, cx);
            }
            Some(_) => {
                self.set_title(if pin { "Pin doesn't match, re-try" } else { "Passwords don't match, re-try" }, false, cx);
            }
        }
    }

    fn commit(&mut self, password: Option<Zeroizing<String>>, security: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let rx = backend::run(async move { account::commit(password, security).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    let npub = vector_core::my_public_key().and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok()).unwrap_or_default();
                    cx.emit(LoggedIn { npub });
                }
                Err(e) => {
                    this.show_error(format!("Could not finish setup: {e}"), cx);
                    this.open_encryption(cx);
                    let _ = window;
                }
            })
            .ok();
        })
        .detach();
    }

    fn finish_unlock(&mut self, password: Option<Zeroizing<String>>, window: &mut Window, cx: &mut Context<Self>) {
        let rx = backend::run(async move {
            let npub = account::unlock(password).await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(npub) => cx.emit(LoggedIn { npub }),
                Err(e) if e.contains("Incorrect") => {
                    let pin = this.cred == Cred::Pin;
                    this.set_title(if pin { "Incorrect pin, try again" } else { "Incorrect password, try again" }, false, cx);
                    this.wrong = Some(if pin { "pin" } else { "password" });
                    this.pin.clear();
                    if pin {
                        window.focus(&this.pin_focus, cx);
                    } else {
                        window.focus(&this.password.focus_handle(cx), cx);
                    }
                }
                Err(e) => {
                    this.set_title(if this.cred == Cred::Pin { "Enter Your Decryption Pin" } else { "Enter Your Password" }, false, cx);
                    this.show_error(e, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

// ── Painting ────────────────────────────────────────────────────────────────────────────

fn lg_btn(id: &'static str, label: &'static str, kind: u8) -> gpui::Stateful<gpui::Div> {
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
        1 => base.border_color(c(PRIMARY)).bg(c(PRIMARY)).text_color(c(APP_BG)).hover(|s| s.bg(c(0x3be9a4))),
        2 => base.border_color(c(PRIMARY)).text_color(c(0xffffff)).hover(|s| s.bg(ca(51, 219, 152, 0.08))),
        _ => base.border_color(c(0x393b3c)).text_color(c(0xffffff)).hover(|s| s.border_color(c(0x5b6060))),
    }
}

fn lg_title(text: SharedString, lock: bool, color: u32) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .mb(px(25.))
        .when(lock, |d| d.child(img("icons/login/lock.svg").w(px(22.)).h(px(29.))))
        .child(kit::spaced(&text, 1.6).font_weight(FontWeight::BOLD).text_size(px(16.)).line_height(px(24.)).text_color(c(color)))
}

fn lg_back(prompt: &'static str, label: &'static str, on: impl Fn(&mut Window, &mut App) + 'static) -> gpui::Div {
    div()
        .mt(px(25.))
        .flex()
        .items_baseline()
        .text_size(px(11.))
        .line_height(px(24.))
        .text_color(c(ICON_MUTED))
        .child(kit::spaced(prompt, 1.1))
        .child(
            div()
                .id("lg-back")
                .cursor_pointer()
                .text_color(c(PRIMARY))
                .on_click(move |_, w, cx| on(w, cx))
                .child(kit::spaced(label, 1.1)),
        )
}

impl LoginView {
    fn field(&self, input: Entity<TextInput>, eye: bool, on_go: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static, cx: &mut Context<Self>) -> gpui::Div {
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
                    .child(div().flex_1().min_w_0().pl(px(11.)).flex().items_center().child(input))
                    .when(eye, |d| {
                        d.child(
                            div()
                                .id("lg-eye")
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
                                    svg().path("icons/eye-off.svg").w(px(22.)).h(px(18.)).text_color(c(0x959899)).into_any_element()
                                } else {
                                    img("icons/login/eye.svg").w(px(22.)).h(px(16.)).into_any_element()
                                }),
                        )
                    })
                    .child(
                        div()
                            .id("lg-go")
                            .h_full()
                            .pl(px(4.))
                            .pr(px(6.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| on_go(this, window, cx)))
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
                if v == "signer" {
                    this.show_error("Remote signers aren't available in this client yet.", cx);
                } else {
                    this.method = v;
                    if v == "key" {
                        window.focus(&this.key.focus_handle(cx), cx);
                    }
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
                    .child("Sign in with")
                    .child(svg().path("icons/info.svg").size(px(18.)).text_color(c(0x333333))),
            )
            .child(kit::select("lg-method", 150., METHODS, self.method, self.select_open, toggle, pick))
    }

    fn stage(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.step {
            Step::Start => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(13.))
                .child(lg_btn("lg-create", "Create Account", 1).on_click(cx.listener(|this, _, w, cx| this.create_account(w, cx))))
                .child(lg_btn("lg-login", "Login", 2).on_click(cx.listener(|this, _, w, cx| {
                    this.method = "qr";
                    this.step = Step::Import;
                    let _ = w;
                    cx.notify();
                })))
                .when(self.adding.is_some(), |d| {
                    let view = cx.entity();
                    d.child(lg_back("Changed your mind?\u{a0}", "Go Back", move |_, cx| view.update(cx, |_, cx| cx.emit(AddProfile::Back))))
                })
                .into_any_element(),
            Step::Import => {
                let back = cx.entity();
                let mut block = div().flex().flex_col().items_center();
                if self.method == "key" {
                    block = block
                        .child(lg_title("Enter Your Private Key".into(), true, 0xffffff))
                        .child(self.field(self.key.clone(), false, |this, w, cx| this.import_key(w, cx), cx));
                } else {
                    block = block.child(lg_title("Login with QR Code".into(), true, 0xffffff)).child(
                        lg_btn("lg-device", "Sign in with Another Device", 1)
                            .w(px(300.))
                            .text_size(px(18.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_error("Signing in with another device isn't available in this client yet.", cx)
                            })),
                    );
                }
                block
                    .child(self.sign_in_with(cx))
                    .child(lg_back("Want to change accounts?\u{a0}", "Back to Login", move |w, cx| {
                        back.update(cx, |this, cx| this.back_to_start(w, cx))
                    }))
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
                .child(lg_btn("lg-skip", "Skip Encryption", 0).on_click(cx.listener(|this, _, w, cx| this.choose("skip", w, cx))))
                .child(lg_btn("lg-password", "Create Password", 0).on_click(cx.listener(|this, _, w, cx| this.choose("password", w, cx))))
                .child(div().my(px(-4.)).text_size(px(10.)).line_height(px(24.)).text_color(c(ACCENT)).child("*Recommended Option"))
                .child(lg_btn("lg-pin", "Create PIN", 1).on_click(cx.listener(|this, _, w, cx| this.choose("pin", w, cx))))
                .into_any_element();
        }
        if let Some(what) = self.wrong {
            block = block.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .mb(px(25.))
                    .child(
                        div()
                            .relative()
                            .w(px(172.))
                            .h(px(179.))
                            .child(img("icons/login/aggroboi.svg").absolute().left_0().bottom_0().size(px(136.)))
                            .child(img("icons/login/warning.svg").absolute().right(px(-4.)).top(px(-4.)).w(px(79.)).h(px(70.))),
                    )
                    .child(
                        kit::spaced(format!("Oops! You entered the wrong {what}!"), 1.4)
                            .mt(px(14.))
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(c(0xff2ea9)),
                    )
                    .child(
                        kit::spaced(if what == "pin" { "Enter the Correct Pin." } else { "Enter the Correct Password." }, 2.4)
                            .mt(px(8.))
                            .text_size(px(24.))
                            .font_weight(FontWeight::BOLD),
                    ),
            );
        } else if self.busy {
            window.request_animation_frame();
            let phase = (self.born.elapsed().as_secs_f32() / 6.).fract();
            block = block.child(
                div()
                    .mb(px(25.))
                    .child(kit::gradient_text(&self.title, 1.6, phase).font_weight(FontWeight::BOLD).text_size(px(16.)).line_height(px(24.))),
            );
        } else {
            block = block.child(lg_title(self.title.clone(), true, if self.error { 0xff2ea9 } else { 0xffffff }));
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
                            .when(filled, |d| d.child("\u{2022}")),
                    );
                }
                block = block.child(row);
            }
            Cred::Password => {
                block = block.child(self.field(self.password.clone(), true, |this, w, cx| this.submit_password(w, cx), cx));
            }
            Cred::Choose => {}
        }
        block.into_any_element()
    }
}

impl Render for LoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wide = window.viewport_size().width >= px(900.);
        let aggro = self.wrong.is_some();
        let stage = self.stage(window, cx);
        let account_list = self.account_list(cx);
        let link = |id: &'static str, label: &'static str, url: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .text_color(c(PRIMARY))
                .hover(|s| s.text_color(c(ACCENT)))
                .on_click(move |_, _, cx| cx.open_url(url))
                .child(kit::spaced(label, 0.8))
        };
        let sep = || kit::spaced("|", 0.8);
        let mut root = div().size_full().relative().font_family(RUBIK).text_color(c(0xffffff)).bg(c(APP_BG));

        // .lg-art: the camera, faded out towards the centre.
        if !self.bg_hidden {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
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
                    ))),
            );
        }
        if aggro {
            let face = |src: &'static str, l: f32, t: f32, w: f32, h: f32| img(src).absolute().left(gpui::relative(l)).top(gpui::relative(t)).w(px(w)).h(px(h));
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .child(face("icons/login/aggro-bg-1.svg", 0.494, 0.669, 226., 224.))
                    .child(face("icons/login/aggro-bg-2.svg", 0.02, 0.122, 128., 127.))
                    .child(face("icons/login/aggro-bg-3.svg", 0.857, 0.445, 98., 97.))
                    .child(face("icons/login/aggro-bg-4.svg", 0.051, 0.823, 60., 59.)),
            );
        }

        root.child(
            // .lg-head
            div().absolute().top(px(51.)).left_0().right_0().flex().justify_center().child(img("icons/login/lockup.svg").w(px(429.72)).h(px(44.))),
        )
        .child(
            // .lg-stage: centred in the window.
            div().absolute().inset_0().flex().flex_col().items_center().justify_center().px(px(16.)).py(px(24.)).child(stage),
        )
        .child(
            // .lg-foot
            div()
                .absolute()
                .bottom(px(25.))
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
                        .child(link("lk-web", "Website", "https://vectorapp.io"))
                        .child(sep())
                        .child(link("lk-docs", "Documentation", "https://docs.vectorapp.io"))
                        .child(sep())
                        .child(link("lk-privacy", "Privacy Policy", "https://vectorapp.io/privacy-policy"))
                        .child(sep())
                        .child(link("lk-donate", "Donate", "https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/more/donations")),
                ),
        )
        .child(
            // .lg-bg-toggle
            div()
                .id("lg-bg-toggle")
                .absolute()
                .right(px(12.))
                .bottom(px(14.))
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
                .on_click(cx.listener(|this, _, _, cx| {
                    this.bg_hidden = !this.bg_hidden;
                    cx.notify();
                }))
                .child(
                    div().w(px(17.)).h(px(16.)).flex().items_center().child(if self.bg_hidden {
                        img("icons/login/eye.svg").w(px(17.)).h(px(12.))
                    } else {
                        img("icons/login/hide.svg").w(px(17.)).h(px(16.))
                    }),
                )
                .child(kit::spaced(if self.bg_hidden { "Show Background" } else { "Hide Background" }, 0.8)),
        )
        .children(account_list)
        .children(self.pill(cx).map(|pill| div().absolute().top(px(118.)).left_0().right_0().flex().justify_center().child(pill)))
        .when_some(self.popup.clone(), |root, p| {
            let entity = cx.entity();
            root.child(kit::popup(&p, move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    this.popup = None;
                    cx.notify();
                })
            }))
        })
    }
}

impl Focusable for LoginView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.pin_focus.clone()
    }
}
