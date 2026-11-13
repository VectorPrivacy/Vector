//! The PIVX wallet (`js/pivx.js`, `components/miniapps/pivx/`): the card in the attachment
//! panel, its Deposit, Send, Withdraw and Settings dialogs, and the payments shown in chats.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Entity, FontWeight, SharedString, Task, Window, div, prelude::*, px, svg};
use crate::pic::img;
use serde_json::Value;

use super::{VectorApp, backend};
use crate::composer::{InputChanged, TextInput};
use crate::data::PivxPhase;
use crate::theme::*;

const PURPLE: u32 = 0x642d8f;
const VIOLET: u32 = 0xb359fc;
/// `PIVX_MAX_BALANCE_WARNING`: past this the wallet asks for a withdrawal before more deposits.
const MAX_BALANCE: f64 = 1000.;

pub(super) struct Wallet {
    opened: Instant,
    balance: Option<f64>,
    fiat: String,
    shown_at: Instant,
    deposit_loading: bool,
}

pub(super) enum Dialog {
    Deposit {
        address: String,
        received: f64,
        poll: Option<Task<()>>,
    },
    Send {
        chat: String,
        recipient: String,
        quick: bool,
        loading: bool,
        opened: Instant,
        promos: Vec<backend::pivx::PivxPromo>,
        error: String,
        selected: String,
        amount: Entity<TextInput>,
        available: f64,
        busy: bool,
    },
    Withdraw {
        address: Entity<TextInput>,
        amount: Entity<TextInput>,
        available: f64,
        busy: bool,
    },
    Settings {
        address: Entity<TextInput>,
        currencies: Vec<String>,
        currency: String,
        loading: bool,
        picking: bool,
    },
}

fn fixture(store: &crate::data::Store, key: &str) -> Option<Value> {
    store.fixture_pivx.as_ref()?.get(key).cloned()
}

/// `formatFiatValue`: two places and the currency's symbol where it has a common one.
fn fiat_text(value: f64, currency: &str) -> String {
    let symbol = match currency {
        "USD" | "CAD" | "AUD" | "NZD" | "SGD" | "HKD" | "MXN" => Some("$"),
        "EUR" => Some("€"),
        "GBP" => Some("£"),
        "JPY" | "CNY" => Some("¥"),
        "INR" => Some("₹"),
        "KRW" => Some("₩"),
        _ => None,
    };
    let prefix = match currency {
        "CAD" => "CA",
        "AUD" => "A",
        "NZD" => "NZ",
        "SGD" => "SG",
        "HKD" => "HK",
        "MXN" => "MX",
        "CNY" => "CN",
        _ => "",
    };
    let digits = format!("{value:.2}");
    let (int, frac) = digits.split_once('.').unwrap_or((&digits, "00"));
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 && ch != '-' {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    match symbol {
        Some(s) => format!("{prefix}{s}{grouped}.{frac}"),
        None => format!("{currency} {grouped}.{frac}"),
    }
}

fn valid_address(a: &str) -> bool {
    a.starts_with('D') && (30..=36).contains(&a.len())
}

impl VectorApp {
    fn pivx_input(&self, placeholder: &str, size: f32, cx: &mut Context<Self>) -> Entity<TextInput> {
        let input = cx.new(|cx| {
            let t = TextInput::new(placeholder.to_string(), cx).with_look(crate::composer::Look {
                family: INTER,
                size: px(size),
                line_height: px(size * 1.2),
                weight: FontWeight::MEDIUM,
                color: c(0xffffff),
                placeholder: white_a(0.3),
                ..Default::default()
            });
            if size > 20. { t.right_aligned() } else { t }
        });
        cx.subscribe(&input, |_, _, _: &InputChanged, cx| cx.notify()).detach();
        input
    }

    /// `showPivxWalletPanel`: the card takes the Mini Apps view's place.
    pub(super) fn show_pivx_wallet(&mut self, cx: &mut Context<Self>) {
        backend::device::set("pivx_last_used", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()).to_string());
        self.pivx_wallet = Some(Wallet {
            opened: Instant::now(),
            balance: None,
            fiat: String::new(),
            shown_at: Instant::now(),
            deposit_loading: false,
        });
        self.refresh_pivx_wallet(cx);
        cx.notify();
    }

    /// `refreshPivxWallet`: the balance, and its fiat value at the preferred currency.
    pub(super) fn refresh_pivx_wallet(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            let store = self.store.borrow();
            let balance = fixture(&store, "balance").and_then(|v| v.as_f64()).unwrap_or(0.);
            let price = fixture(&store, "price");
            drop(store);
            let fiat = price.and_then(|p| {
                let cur = p.get("currency")?.as_str()?.to_uppercase();
                let v = p.get("value")?.as_f64()?;
                self.store.borrow_mut().pivx_price = Some((v, cur.clone()));
                Some(fiat_text(balance * v, &cur))
            });
            if let Some(w) = self.pivx_wallet.as_mut() {
                w.balance = Some(balance);
                w.fiat = fiat.unwrap_or_default();
                w.shown_at = Instant::now();
            }
            return;
        }
        let rx = backend::run(async {
            let currency = backend::pivx::currency().unwrap_or_else(backend::pivx::default_currency);
            let (balance, price) = tokio::join!(backend::pivx::balance(), backend::pivx::price(currency.clone()));
            (balance.unwrap_or(0.), price.map(|p| (p, currency)))
        });
        cx.spawn(async move |this, cx| {
            let Ok((balance, price)) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(p) = price.clone() {
                    this.store.borrow_mut().pivx_price = Some(p);
                }
                if let Some(w) = this.pivx_wallet.as_mut() {
                    w.balance = Some(balance);
                    w.fiat = price.map(|(p, cur)| fiat_text(balance * p, &cur)).unwrap_or_default();
                    w.shown_at = Instant::now();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn pivx_animating(&self) -> bool {
        let spinning = match &self.pivx_dialog {
            Some(Dialog::Deposit { received, .. }) => *received <= 0.,
            Some(Dialog::Send { loading, opened, .. }) => *loading || opened.elapsed().as_secs_f32() < 1.,
            _ => false,
        };
        spinning
            || self.pivx_wallet.as_ref().is_some_and(|w| {
                w.balance.is_none() || w.opened.elapsed().as_secs_f32() < 0.6 || w.shown_at.elapsed().as_secs_f32() < 0.4
            })
            || self.store.borrow().pivx_bubbles.values().any(|b| matches!(b.0, PivxPhase::Syncing | PivxPhase::Claiming))
    }

    /// `showPivxDepositDialog`: a fresh promo address, polled until something lands on it.
    fn pivx_deposit(&mut self, cx: &mut Context<Self>) {
        let balance = self.pivx_wallet.as_ref().and_then(|w| w.balance).unwrap_or(0.);
        if balance >= MAX_BALANCE {
            let mut p = crate::kit::Popup::notice("Balance Too High");
            p.text = Some(format!("Your wallet balance is <b>{balance:.2} PIV</b>, which exceeds the recommended limit of {MAX_BALANCE} PIV.<br><br>Vector is not intended to replace a proper cryptocurrency wallet. Please <b>withdraw your funds</b> to a secure wallet before depositing more.").into());
            self.popup = Some((p, super::PopupAct::Notice));
            return cx.notify();
        }
        if !self.live {
            let address = fixture(&self.store.borrow(), "deposit_address")
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            self.pivx_dialog = Some(Dialog::Deposit { address, received: 0., poll: None });
            return cx.notify();
        }
        if let Some(w) = self.pivx_wallet.as_mut() {
            w.deposit_loading = true;
        }
        cx.notify();
        let rx = backend::run(backend::pivx::create_promo());
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                if let Some(w) = this.pivx_wallet.as_mut() {
                    w.deposit_loading = false;
                }
                match res {
                    Ok(promo) => {
                        let poll = this.pivx_poll_deposit(promo.address.clone(), cx);
                        this.pivx_dialog = Some(Dialog::Deposit { address: promo.address, received: 0., poll: Some(poll) });
                    }
                    Err(_) => this.show_toast("Failed to create deposit address", cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `startDepositPolling`: every 5s, until the address holds something.
    fn pivx_poll_deposit(&mut self, address: String, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let promos = backend::run(backend::pivx::promos(false)).await;
                let got = match promos {
                    Ok(Ok(list)) => list.into_iter().find(|p| p.address == address).map_or(0., |p| p.balance_piv),
                    _ => 0.,
                };
                if got <= 0. {
                    continue;
                }
                let still = this
                    .update(cx, |this, cx| {
                        let Some(Dialog::Deposit { address: shown, received, .. }) = this.pivx_dialog.as_mut() else {
                            return false;
                        };
                        if *shown != address {
                            return false;
                        }
                        *received = got;
                        this.show_toast(&format!("Received {got:.8} PIV!"), cx);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if still {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                    this.update(cx, |this, cx| {
                        this.pivx_dialog = None;
                        this.refresh_pivx_wallet(cx);
                        cx.notify();
                    })
                    .ok();
                }
                return;
            }
        })
    }

    /// `showPivxSendDialog`: quick send lists funded promos; custom takes an amount.
    fn pivx_send(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.open.as_ref().map(|c| c.to_string()) else {
            return self.show_toast("Open a chat first to send PIVX", cx);
        };
        let recipient = self.store.borrow().name(&chat);
        let amount = self.pivx_input("0.00", 24., cx);
        self.pivx_dialog = Some(Dialog::Send {
            chat,
            recipient: if recipient.is_empty() { "this chat".into() } else { recipient },
            quick: true,
            loading: true,
            opened: Instant::now(),
            promos: Vec::new(),
            error: String::new(),
            selected: String::new(),
            amount,
            available: 0.,
            busy: false,
        });
        cx.notify();
        let fill = |this: &mut Self, res: Result<Vec<backend::pivx::PivxPromo>, String>| {
            let Some(Dialog::Send { loading, promos, available, error, opened, .. }) = this.pivx_dialog.as_mut() else {
                return;
            };
            *loading = false;
            *opened = Instant::now();
            match res {
                Ok(list) => {
                    let mut funded: Vec<_> = list.into_iter().filter(|p| p.balance_piv > 0.).collect();
                    funded.sort_by(|a, b| b.balance_piv.total_cmp(&a.balance_piv));
                    *available = funded.iter().map(|p| p.balance_piv).sum();
                    *promos = funded;
                }
                Err(_) => *error = "Failed to load wallet.".into(),
            }
        };
        if !self.live {
            let list: Vec<backend::pivx::PivxPromo> = fixture(&self.store.borrow(), "promos")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            fill(self, Ok(list));
            return;
        }
        let rx = backend::run(backend::pivx::promos(false));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                fill(this, res);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `sendPivxPayment`: a whole promo, or a custom amount by coin selection.
    fn pivx_send_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Send { chat, quick, promos, selected, amount, available, busy, .. }) = self.pivx_dialog.as_mut()
        else {
            return;
        };
        if *busy {
            return;
        }
        let chat = chat.clone();
        let (fut, sent): (std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>, f64) = if *quick {
            let Some(p) = promos.iter().find(|p| p.gift_code == *selected) else {
                return self.show_toast("Select an amount to send", cx);
            };
            (Box::pin(backend::pivx::send_promo(chat, p.gift_code.clone())), p.balance_piv)
        } else {
            let value: f64 = amount.read(cx).text().trim().parse().unwrap_or(0.);
            if value <= 0. {
                return self.show_toast("Enter a valid amount", cx);
            }
            if value > *available {
                let max = *available;
                return self.show_toast(&format!("Insufficient funds (max: {max:.2} PIV)"), cx);
            }
            (Box::pin(backend::pivx::send_amount(chat, value)), value)
        };
        *busy = true;
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(fut);
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.pivx_dialog = None;
                        this.show_toast(&format!("Sent {sent:.2} PIV"), cx);
                        this.refresh_pivx_wallet(cx);
                    }
                    Err(e) => {
                        if let Some(Dialog::Send { busy, .. }) = this.pivx_dialog.as_mut() {
                            *busy = false;
                        }
                        this.show_toast(&format!("Failed to send: {e}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pivx_withdraw(&mut self, cx: &mut Context<Self>) {
        let address = self.pivx_input("D...", 14., cx);
        let amount = self.pivx_input("0.00", 24., cx);
        let available = self.pivx_wallet.as_ref().and_then(|w| w.balance).unwrap_or(0.);
        self.pivx_dialog = Some(Dialog::Withdraw { address, amount, available, busy: false });
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(backend::pivx::balance());
        cx.spawn(async move |this, cx| {
            let b = rx.await.ok().and_then(Result::ok).unwrap_or(0.);
            this.update(cx, |this, cx| {
                if let Some(Dialog::Withdraw { available, .. }) = this.pivx_dialog.as_mut() {
                    *available = b;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `executePivxWithdraw`.
    fn pivx_withdraw_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Withdraw { address, amount, available, busy }) = self.pivx_dialog.as_mut() else { return };
        if *busy {
            return;
        }
        let to = address.read(cx).text().trim().to_string();
        let value: f64 = amount.read(cx).text().trim().parse().unwrap_or(0.);
        if !valid_address(&to) {
            return self.show_toast("Invalid PIVX address", cx);
        }
        if value <= 0. {
            return self.show_toast("Enter a valid amount", cx);
        }
        if value > *available {
            return self.show_toast("Insufficient balance", cx);
        }
        *busy = true;
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(backend::pivx::withdraw(to, value));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.pivx_dialog = None;
                        this.show_toast(&format!("Withdrawn {value:.2} PIV"), cx);
                        this.refresh_pivx_wallet(cx);
                    }
                    Err(e) => {
                        if let Some(Dialog::Withdraw { busy, .. }) = this.pivx_dialog.as_mut() {
                            *busy = false;
                        }
                        this.show_toast(&format!("Withdrawal failed: {e}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `showPivxSettingsDialog`: the auto-withdraw address, and the currency list once fetched.
    fn pivx_settings(&mut self, cx: &mut Context<Self>) {
        let address = self.pivx_input("D...", 14., cx);
        if self.live
            && let Some(a) = backend::pivx::wallet_address()
        {
            address.update(cx, |i, cx| i.set_text(&a, cx));
        }
        self.pivx_dialog = Some(Dialog::Settings {
            address,
            currencies: Vec::new(),
            currency: String::new(),
            loading: true,
            picking: false,
        });
        cx.notify();
        let current = self
            .store
            .borrow()
            .pivx_price
            .as_ref()
            .map(|p| p.1.clone())
            .or_else(|| self.live.then(backend::pivx::currency).flatten())
            .unwrap_or_else(backend::pivx::default_currency)
            .to_uppercase();
        let fill = move |this: &mut Self, list: Vec<String>| {
            if let Some(Dialog::Settings { currencies, currency, loading, .. }) = this.pivx_dialog.as_mut() {
                let list = if list.is_empty() { vec!["USD".to_string()] } else { list };
                *currency = if list.contains(&current) { current.clone() } else { String::new() };
                *currencies = list;
                *loading = false;
            }
        };
        if !self.live {
            let list = fixture(&self.store.borrow(), "currencies")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            return fill(self, list);
        }
        let rx = backend::run(backend::pivx::currencies());
        cx.spawn(async move |this, cx| {
            let list = rx.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                fill(this, list);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `savePivxSettings`.
    fn pivx_settings_save(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Settings { address, currency, .. }) = self.pivx_dialog.as_ref() else { return };
        let address = address.read(cx).text().trim().to_string();
        let currency = currency.clone();
        if !address.is_empty() && !valid_address(&address) {
            return self.show_toast("Invalid PIVX address format", cx);
        }
        if self.live {
            let saved = backend::pivx::set_wallet_address(address)
                .and_then(|_| if currency.is_empty() { Ok(()) } else { backend::pivx::set_currency(currency.clone()) });
            if saved.is_err() {
                return self.show_toast("Failed to save settings", cx);
            }
        } else if !currency.is_empty() {
            let price = self.store.borrow().pivx_price.as_ref().map_or(0., |p| p.0);
            self.store.borrow_mut().pivx_price = Some((price, currency));
        }
        self.pivx_dialog = None;
        self.show_toast("Wallet settings saved", cx);
        if self.live {
            self.refresh_pivx_wallet(cx);
        }
        cx.notify();
    }

    /// `mergePivxPaymentsIntoChat`: a chat's payments, as messages, once it opens.
    pub(super) fn pivx_merge(&mut self, chat: String, cx: &mut Context<Self>) {
        if !self.live {
            let payments: Vec<Value> = fixture(&self.store.borrow(), "payments")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let mut store = self.store.borrow_mut();
            for p in payments.iter().filter(|p| p.get("conversation_id").and_then(Value::as_str) == Some(chat.as_str())) {
                store.add_pivx(&chat, p);
            }
            drop(store);
            return self.pivx_check_bubbles(cx);
        }
        let rx = backend::run(backend::pivx::chat_payments(chat.clone()));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(payments)) = rx.await else { return };
            if payments.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                {
                    let mut store = this.store.borrow_mut();
                    for p in &payments {
                        store.add_pivx(&chat, p);
                    }
                }
                if this.open.as_deref() == Some(chat.as_str()) {
                    this.refresh_rows(true);
                    this.pivx_check_bubbles(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `handlePivxPaymentReceived`: a payment sent or received, into its chat.
    pub(super) fn pivx_received(&mut self, payload: &Value, cx: &mut Context<Self>) {
        let Some(chat) = payload.get("conversation_id").and_then(Value::as_str).map(String::from) else { return };
        let chat = if chat.starts_with("npub1") || chat.len() != 64 {
            chat
        } else {
            nostr_sdk::prelude::PublicKey::from_hex(&chat)
                .ok()
                .and_then(|k| nostr_sdk::prelude::ToBech32::to_bech32(&k).ok())
                .unwrap_or(chat)
        };
        let added = self.store.borrow_mut().add_pivx(&chat, payload);
        if added && self.open.as_deref() == Some(chat.as_str()) {
            self.refresh_rows(true);
            self.pivx_check_bubbles(cx);
        }
        cx.notify();
    }

    /// `pivxEnsureBubble` / `checkPivxPaymentClaimedState`: settle a bubble from its address's balance.
    pub(super) fn pivx_check_bubbles(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.open.as_ref().map(|c| c.to_string()) else { return };
        let todo: Vec<(String, Option<String>, bool)> = {
            let store = self.store.borrow();
            let Some(ch) = store.chat(&chat) else { return };
            ch.messages
                .iter()
                .filter_map(|m| store.pivx.get(&m.id).map(|p| (p, m.mine)))
                .filter(|(p, _)| !store.pivx_bubbles.contains_key(&p.gift_code))
                .map(|(p, mine)| (p.gift_code.clone(), p.address.clone(), mine))
                .collect()
        };
        for (code, address, mine) in todo {
            let claimable = (PivxPhase::Claimable, if mine { "Click to reclaim" } else { "Click to claim" }.to_string());
            let Some(address) = address.filter(|_| self.live) else {
                self.store.borrow_mut().pivx_bubbles.insert(code, claimable);
                continue;
            };
            self.store.borrow_mut().pivx_bubbles.insert(code.clone(), (PivxPhase::Syncing, "Syncing...".into()));
            cx.spawn(async move |this, cx| {
                let mut tries = 0;
                let settled = loop {
                    let balance = backend::run(backend::pivx::address_balance(address.clone(), tries > 0)).await;
                    match balance {
                        Ok(Ok(b)) if b > 0. => break claimable,
                        Ok(Ok(_)) if tries < 3 => {
                            tries += 1;
                            this.update(cx, |this, cx| {
                                this.store
                                    .borrow_mut()
                                    .pivx_bubbles
                                    .insert(code.clone(), (PivxPhase::Syncing, "Confirming...".into()));
                                cx.notify();
                            })
                            .ok();
                            cx.background_executor().timer(Duration::from_secs(3)).await;
                        }
                        Ok(Ok(_)) => break (PivxPhase::Claimed, "Claimed".into()),
                        _ => break claimable,
                    }
                };
                this.update(cx, |this, cx| {
                    this.store.borrow_mut().pivx_bubbles.insert(code, settled);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// `claimPivxPayment`.
    pub(super) fn pivx_claim(&mut self, code: String, cx: &mut Context<Self>) {
        let phase = self.store.borrow().pivx_bubbles.get(&code).map(|b| b.0);
        if matches!(phase, Some(PivxPhase::Claimed | PivxPhase::Claiming)) {
            return;
        }
        self.store.borrow_mut().pivx_bubbles.insert(code.clone(), (PivxPhase::Claiming, "Claiming...".into()));
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(backend::pivx::claim(code.clone()));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(v) => {
                        this.store.borrow_mut().pivx_bubbles.insert(code, (PivxPhase::Claimed, "Claimed!".into()));
                        let amount = v.get("amount_piv").and_then(Value::as_f64).map(|a| format!("{a:.2}")).unwrap_or_default();
                        this.show_toast(&format!("Claimed {amount} PIV"), cx);
                        this.refresh_pivx_wallet(cx);
                    }
                    Err(e) => {
                        this.store
                            .borrow_mut()
                            .pivx_bubbles
                            .insert(code, (PivxPhase::Failed, "Claim failed - tap to retry".into()));
                        this.show_toast(&format!("Failed to claim: {e}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pivx_close_dialog(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.pivx_dialog,
            Some(Dialog::Send { busy: true, .. }) | Some(Dialog::Withdraw { busy: true, .. })
        ) {
            self.pivx_dialog = None;
            cx.notify();
        }
    }

    /// `PivxWallet`: balance with its fiat line, and the Send / Deposit / Withdraw / Settings dock.
    pub(super) fn pivx_wallet_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let w = self.pivx_wallet.as_ref()?;
        let since = w.opened.elapsed().as_secs_f32();
        let ease = |t: f32| 1. - (1. - t.clamp(0., 1.)).powi(3);
        let shown = ease(w.shown_at.elapsed().as_secs_f32() / 0.3);
        let amount: AnyElement = match w.balance {
            None => div()
                .h(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .child(crate::attachments::spinner(28., 3., 1., 0.25, white_a(0.2), c(VIOLET)))
                .into_any_element(),
            Some(b) => div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .relative()
                .left(px(-8. * (1. - shown)))
                .opacity(shown)
                .text_size(px(30.))
                .line_height(px(30.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(0xffffff))
                .child(format!("{b:.2}"))
                .child(div().text_color(c(PURPLE)).child("PIV"))
                .into_any_element(),
        };
        let fade = ease(since / 0.25);
        let balance = div()
            .relative()
            .top(px(-6. * (1. - fade)))
            .opacity(fade)
            .flex()
            .items_center()
            .gap(px(16.))
            .px(px(20.))
            .py(px(16.))
            .rounded(px(12.))
            .border_2()
            .border_color(c(PURPLE))
            .bg(gpui::linear_gradient(
                90.,
                gpui::linear_color_stop(c(0x452568), 0.),
                gpui::linear_color_stop(c(0x110b1b), 1.),
            ))
            .child(img("icons/pivx.svg").size(px(80.)).flex_shrink_0())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_end()
                    .justify_center()
                    .pr(px(12.))
                    .child(amount)
                    .when(!w.fiat.is_empty(), |d| {
                        d.child(
                            div()
                                .mt(px(4.))
                                .relative()
                                .left(px(-8. * (1. - shown)))
                                .opacity(shown)
                                .text_size(px(18.))
                                .line_height(px(18.))
                                .text_color(white_a(0.4))
                                .child(w.fiat.clone()),
                        )
                    }),
            );
        let deposit_off = w.balance.unwrap_or(0.) >= MAX_BALANCE;
        let dock_btn = |i: usize, id: &'static str, glyph: &'static str, label: &'static str, primary: bool, off: bool, loading: bool| {
            let t = ease((since - (i as f32 + 1.) * 0.06) / 0.25);
            let group = SharedString::from(id);
            div()
                .id(id)
                .group(group.clone())
                .relative()
                .left(px(-12. * (1. - t)))
                .opacity(t * if off { 0.4 } else if loading { 0.6 } else { 1. })
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .when(!off && !loading, |d| d.cursor_pointer())
                .child(
                    div()
                        .size(px(44.))
                        .rounded(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if primary { c(PURPLE).into() } else { white_a(0.12) })
                        .when(!primary && !off, |d| d.group_hover(group.clone(), |s| s.bg(white_a(0.18))))
                        .child(if loading {
                            crate::attachments::spinner(18., 2., 1., 0.25, white_a(0.3), c(0xffffff))
                        } else {
                            svg()
                                .path(format!("icons/{glyph}.svg"))
                                .size(px(20.))
                                .text_color(if primary { c(0xffffff).into() } else { white_a(0.7) })
                                .when(!primary && !off, |d| d.group_hover(group.clone(), |s| s.text_color(c(0xffffff))))
                                .into_any_element()
                        }),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .line_height(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(white_a(0.7))
                        .when(!off, |d| d.group_hover(group.clone(), |s| s.text_color(c(0xffffff))))
                        .child(label),
                )
        };
        let dock = div()
            .flex()
            .gap(px(20.))
            .justify_center()
            .py(px(8.))
            .child(
                dock_btn(0, "pivx-send-btn", "send", "Send", true, false, false)
                    .on_click(cx.listener(|app, _, _, cx| app.pivx_send(cx))),
            )
            .child({
                let b = dock_btn(1, "pivx-deposit-btn", "plus-circle", "Deposit", false, deposit_off, w.deposit_loading)
                    .when(deposit_off, |b| b.tooltip(crate::kit::tooltip("Balance too high - please withdraw first")));
                if w.deposit_loading { b } else { b.on_click(cx.listener(|app, _, _, cx| app.pivx_deposit(cx))) }
            })
            .child(
                dock_btn(2, "pivx-withdraw-btn", "arrow-up", "Withdraw", false, false, false)
                    .on_click(cx.listener(|app, _, _, cx| app.pivx_withdraw(cx))),
            )
            .child(
                dock_btn(3, "pivx-settings-btn", "settings", "Settings", false, false, false)
                    .on_click(cx.listener(|app, _, _, cx| app.pivx_settings(cx))),
            );
        let close = div()
            .id("pivx-close")
            .absolute()
            .top(px(-5.))
            .right_0()
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_2()
            .border_color(c(0x323232))
            .bg(c(0x000000))
            .hover(|s| s.bg(c(0x1a1a1a)))
            .cursor_pointer()
            .on_click(cx.listener(|app, _, _, cx| {
                app.pivx_wallet = None;
                if let Some(m) = app.mini.as_mut() {
                    m.opened = Instant::now();
                }
                cx.notify();
            }))
            .child(svg().path("icons/x.svg").size(px(14.)).text_color(c(PURPLE)));
        Some(
            div()
                .relative()
                .w_full()
                .font_family(INTER)
                .flex()
                .flex_col()
                .gap(px(15.))
                .pt(px(28.))
                .child(close)
                .child(balance)
                .child(dock)
                .into_any_element(),
        )
    }

    pub(super) fn pivx_dialog_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.pivx_dialog.as_ref()?;
        let title = match d {
            Dialog::Deposit { .. } => "Deposit PIVX",
            Dialog::Send { .. } => "Send PIVX",
            Dialog::Withdraw { .. } => "Withdraw PIVX",
            Dialog::Settings { .. } => "PIVX Wallet Settings",
        };
        let spinner = || crate::attachments::spinner(28., 3., 1., 0.25, white_a(0.2), c(VIOLET));
        let big_btn = |id: &'static str, label: SharedString, enabled: bool| {
            div()
                .id(id)
                .w_full()
                .p(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .rounded(px(8.))
                .bg(if enabled { c(PURPLE).into() } else { white_a(0.2) })
                .text_size(px(16.))
                .line_height(px(20.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c(0xffffff))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.9)))
                .child(label)
        };
        let well = |child: AnyElement| {
            div()
                .p(px(12.))
                .rounded(px(8.))
                .bg(ca(0, 0, 0, 0.3))
                .border_1()
                .border_color(white_a(0.1))
                .child(child)
        };
        let label = |t: &'static str| div().text_size(px(14.)).line_height(px(24.)).font_weight(FontWeight::MEDIUM).text_color(c(0xffffff)).child(t);
        let hint = |t: &'static str| div().text_size(px(12.)).line_height(px(15.6)).text_color(white_a(0.5)).child(t);
        let max_btn = |id: &'static str| {
            div()
                .id(id)
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .bg(ca(0xb3, 0x59, 0xfc, 0x30 as f32 / 255.))
                .hover(|s| s.bg(ca(0xb3, 0x59, 0xfc, 0x50 as f32 / 255.)))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(VIOLET))
                .cursor_pointer()
                .child("MAX")
        };
        let amount_box = |input: &Entity<TextInput>| {
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(10.))
                .px(px(15.))
                .py(px(16.))
                .rounded(px(8.))
                .bg(ca(0, 0, 0, 0.3))
                .border_1()
                .border_color(white_a(0.1))
                .child(div().w(px(179.)).h(px(57.8)).flex().items_center().justify_end().child(input.clone()))
                .child(div().text_size(px(18.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(white_a(0.6)).child("PIV"))
        };
        let text_box = |input: &Entity<TextInput>| {
            let focused = gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window);
            div()
                .p(px(12.))
                .rounded(px(8.))
                .bg(ca(0, 0, 0, 0.3))
                .border_1()
                .border_color(if focused { c(VIOLET).into() } else { white_a(0.1) })
                .child(input.clone())
        };

        let mut body = div().p(px(20.)).flex().flex_col().gap(px(15.));
        match d {
            Dialog::Deposit { address, received, .. } => {
                body = body
                    .child(div().text_size(px(14.)).line_height(px(24.)).text_color(white_a(0.7)).child("Send PIVX to this address to deposit:"))
                    .child(well(
                        div()
                            .text_center()
                            .text_size(px(14.))
                            .line_height(px(24.))
                            .text_color(c(0xffffff))
                            .child(address.clone())
                            .into_any_element(),
                    ))
                    .child(
                        big_btn("pivx-copy", "Copy Address".into(), true)
                            .on_click(cx.listener(|app, _, _, cx| {
                                if let Some(Dialog::Deposit { address, .. }) = app.pivx_dialog.as_ref() {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(address.clone()));
                                    app.show_toast("Address copied!", cx);
                                }
                            })),
                    )
                    .child(
                        div()
                            .mt(px(10.))
                            .pt(px(16.))
                            .border_t_1()
                            .border_color(white_a(0.1))
                            .child(if *received > 0. {
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(10.))
                                    .p(px(15.))
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(c(ACCENT))
                                    .child(svg().path("icons/check.svg").size(px(20.)).text_color(c(ACCENT)))
                                    .child(format!("Received {received:.8} PIV!"))
                            } else {
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(12.))
                                    .px(px(15.))
                                    .py(px(18.))
                                    .text_size(px(14.))
                                    .text_color(white_a(0.7))
                                    .child(spinner())
                                    .child("Awaiting Deposit...")
                            }),
                    );
            }
            Dialog::Send { recipient, quick, loading, opened, promos, error, selected, amount, available, busy, .. } => {
                if *quick {
                    let mut list = div().id("pivx-promos").flex().flex_col().gap(px(8.)).max_h(px(180.)).overflow_y_scroll().pr(px(8.));
                    let empty = |t: &'static str| div().p(px(20.)).text_center().text_size(px(13.)).text_color(white_a(0.4)).child(t);
                    if *loading {
                        list = list.child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap(px(10.))
                                .p(px(20.))
                                .text_size(px(13.))
                                .text_color(white_a(0.5))
                                .child(spinner())
                                .child("Loading..."),
                        );
                    } else if !error.is_empty() {
                        list = list.child(div().p(px(20.)).text_center().text_size(px(13.)).text_color(white_a(0.4)).child(error.clone()));
                    } else if promos.is_empty() {
                        list = list.child(empty("No funds available to send.")).child(empty("Deposit PIVX first.").pt_0().mt(px(-20.)));
                    }
                    let step = if promos.len() > 1 { (0.3 / (promos.len() - 1) as f32).min(0.06) } else { 0. };
                    let since = opened.elapsed().as_secs_f32();
                    for (i, p) in promos.iter().enumerate() {
                        let t = ((since - i as f32 * step) / 0.25).clamp(0., 1.);
                        let on = *selected == p.gift_code;
                        let code = p.gift_code.clone();
                        list = list.child(
                            div()
                                .id(SharedString::from(format!("pivx-promo-{code}")))
                                .flex()
                                .items_center()
                                .justify_between()
                                .px(px(15.))
                                .py(px(12.))
                                .rounded(px(8.))
                                .relative()
                                .top(px(-8. * (1. - t)))
                                .opacity(t)
                                .border_1()
                                .map(|d| {
                                    if on {
                                        d.bg(ca(92, 77, 154, 0.4)).border_color(c(VIOLET))
                                    } else {
                                        d.bg(ca(0, 0, 0, 0.3))
                                            .border_color(white_a(0.1))
                                            .hover(|s| s.bg(ca(92, 77, 154, 0.3)).border_color(ca(92, 77, 154, 0.5)))
                                    }
                                })
                                .cursor_pointer()
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    if let Some(Dialog::Send { selected, .. }) = app.pivx_dialog.as_mut() {
                                        *selected = code.clone();
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .text_size(px(16.))
                                        .line_height(px(24.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(c(0xffffff))
                                        .child(format!("{:.2} PIV", p.balance_piv)),
                                )
                                .child(div().text_size(px(11.)).text_color(white_a(0.4)).child(p.gift_code.clone())),
                        );
                    }
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(div().text_size(px(13.)).line_height(px(24.)).text_color(white_a(0.6)).child("Select amount to send:"))
                            .child(list)
                            .child(
                                div()
                                    .id("pivx-custom")
                                    .px(px(8.))
                                    .pb(px(8.))
                                    .text_center()
                                    .text_size(px(16.))
                                    .line_height(px(24.))
                                    .text_color(c(VIOLET))
                                    .hover(|s| s.text_color(ca(0xb3, 0x59, 0xfc, 0.5)))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        if let Some(Dialog::Send { quick, selected, amount, .. }) = app.pivx_dialog.as_mut() {
                                            *quick = false;
                                            selected.clear();
                                            window.focus(&gpui::Focusable::focus_handle(amount.read(cx), cx), cx);
                                        }
                                        cx.notify();
                                    }))
                                    .child("Send Custom Amount"),
                            ),
                    );
                } else {
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .mb(px(20.))
                                    .px(px(12.))
                                    .py(px(10.))
                                    .rounded(px(6.))
                                    .bg(ca(0x56, 0x56, 0x56, 0x20 as f32 / 255.))
                                    .flex()
                                    .items_start()
                                    .gap(px(8.))
                                    .text_size(px(12.))
                                    .text_color(c(0xb2b2b2))
                                    .child(svg().path("icons/info.svg").size(px(28.)).mr(px(4.)).flex_shrink_0().text_color(c(0xffffff)))
                                    .child("Custom amounts require on-chain confirmation and may take longer to send."),
                            )
                            .child(amount_box(amount))
                            .child(
                                div()
                                    .id("pivx-send-max")
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap(px(8.))
                                    .pt(px(6.))
                                    .pb(px(20.))
                                    .text_size(px(12.))
                                    .text_color(white_a(0.5))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(Dialog::Send { amount, available, .. }) = app.pivx_dialog.as_ref()
                                            && *available > 0.
                                        {
                                            let v = format!("{available:.2}");
                                            amount.update(cx, |i, cx| i.set_text(&v, cx));
                                        }
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(4.))
                                            .child("Available:")
                                            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{available:.2}")))
                                            .child("PIV"),
                                    )
                                    .child(max_btn("pivx-send-max-btn")),
                            )
                            .child(
                                div()
                                    .id("pivx-quick")
                                    .px(px(8.))
                                    .pb(px(8.))
                                    .text_center()
                                    .text_size(px(16.))
                                    .line_height(px(24.))
                                    .text_color(c(VIOLET))
                                    .hover(|s| s.text_color(ca(0xb3, 0x59, 0xfc, 0.5)))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(Dialog::Send { quick, amount, .. }) = app.pivx_dialog.as_mut() {
                                            *quick = true;
                                            amount.update(cx, |i, cx| i.set_text("", cx));
                                        }
                                        cx.notify();
                                    }))
                                    .child("Back to Quick Send"),
                            ),
                    );
                }
                body = body
                    .child(
                        div()
                            .m(px(-6.))
                            .flex()
                            .justify_center()
                            .gap(px(4.))
                            .text_size(px(12.))
                            .line_height(px(24.))
                            .text_color(white_a(0.6))
                            .child("Sending to:")
                            .child(div().font_weight(FontWeight::MEDIUM).text_color(c(VIOLET)).child(recipient.clone())),
                    )
                    .child({
                        let b = big_btn("pivx-send-go", if *busy { "Sending..." } else { "Send to Chat" }.into(), !*busy)
                            .when(*loading, |d| d.opacity(0.5));
                        if *busy || *loading { b } else { b.on_click(cx.listener(|app, _, _, cx| app.pivx_send_confirm(cx))) }
                    });
            }
            Dialog::Withdraw { address, amount, available, busy } => {
                body = body
                    .child(div().flex().flex_col().gap(px(8.)).mb(px(16.)).child(label("Destination Address")).child(text_box(address)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .mb(px(16.))
                            .child(label("Amount"))
                            .child(amount_box(amount))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap(px(8.))
                                    .py(px(4.))
                                    .text_size(px(12.))
                                    .text_color(white_a(0.5))
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(4.))
                                            .child("Available:")
                                            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{available:.2}")))
                                            .child("PIV"),
                                    )
                                    .child(max_btn("pivx-withdraw-max").on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(Dialog::Withdraw { amount, available, .. }) = app.pivx_dialog.as_ref()
                                            && *available > 0.
                                        {
                                            let v = format!("{available:.2}");
                                            amount.update(cx, |i, cx| i.set_text(&v, cx));
                                        }
                                    }))),
                            ),
                    )
                    .child(div().mb(px(-6.)).text_center().text_size(px(12.)).text_color(white_a(0.4)).child("Network fee: ~0.0001 PIV"))
                    .child({
                        let b = big_btn("pivx-withdraw-go", if *busy { "Withdrawing..." } else { "Withdraw" }.into(), !*busy);
                        if *busy { b } else { b.on_click(cx.listener(|app, _, _, cx| app.pivx_withdraw_confirm(cx))) }
                    });
            }
            Dialog::Settings { address, currencies, currency, loading, picking } => {
                let select = div()
                    .id("pivx-currency")
                    .w(px(270.))
                    .relative()
                    .px(px(12.))
                    .py(px(12.))
                    .pr(px(36.))
                    .rounded(px(8.))
                    .bg(ca(0, 0, 0, 0.3))
                    .border_1()
                    .border_color(if *picking { c(VIOLET) } else { c(0x383838) })
                    .text_size(px(14.))
                    .line_height(px(17.))
                    .text_color(c(0xffffff))
                    .when(!*loading, |d| {
                        d.cursor_pointer().on_click(cx.listener(|app, _, _, cx| {
                            if let Some(Dialog::Settings { picking, .. }) = app.pivx_dialog.as_mut() {
                                *picking = !*picking;
                            }
                            cx.notify();
                        }))
                    })
                    .child(if *loading { "Loading...".to_string() } else { currency.clone() })
                    .child(
                        svg()
                            .path("icons/chevron-down.svg")
                            .absolute()
                            .right(px(12.))
                            .top(px(14.5))
                            .size(px(12.))
                            .text_color(white_a(0.5)),
                    );
                let mut options = div()
                    .id("pivx-currency-list")
                    .absolute()
                    .top(px(44.))
                    .left(px(45.))
                    .right(px(45.))
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .rounded(px(8.))
                    .bg(c(0x1a1a2e))
                    .border_1()
                    .border_color(c(0x383838))
                    .flex()
                    .flex_col();
                for cur in currencies {
                    let pick = cur.clone();
                    options = options.child(
                        div()
                            .id(SharedString::from(format!("pivx-cur-{cur}")))
                            .px(px(12.))
                            .py(px(6.))
                            .text_size(px(14.))
                            .text_color(c(0xffffff))
                            .when(cur == currency, |d| d.bg(white_a(0.12)))
                            .hover(|s| s.bg(white_a(0.08)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |app, _, _, cx| {
                                if let Some(Dialog::Settings { currency, picking, .. }) = app.pivx_dialog.as_mut() {
                                    *currency = pick.clone();
                                    *picking = false;
                                }
                                cx.notify();
                            }))
                            .child(cur.clone()),
                    );
                }
                body = body
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(label("Auto Withdraw Address"))
                            .child(text_box(address))
                            .child(hint("If set, claimed PIVX will be sent directly to this address. Leave empty to keep funds in your Vector wallet.")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(label("Display Currency"))
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .justify_center()
                                    .child(select)
                                    .when(*picking, |d| d.child(gpui::deferred(options).with_priority(12))),
                            )
                            .child(hint("Choose your preferred fiat currency for balance display.")),
                    )
                    .child(
                        big_btn("pivx-settings-save", "Save Settings".into(), true)
                            .on_click(cx.listener(|app, _, _, cx| app.pivx_settings_save(cx))),
                    );
            }
        }
        let card = div()
            .id("pivx-dialog")
            .font_family(INTER)
            .w(px(404f32.min(f32::from(window.viewport_size().width) - 40.)))
            .overflow_hidden()
            .rounded(px(16.))
            .border_2()
            .border_color(c(0x262626))
            .bg(ca(20, 20, 20, 0.95))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.9),
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(40.),
                spread_radius: px(0.),
                inset: false,
            }])
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(20.))
                    .py(px(15.))
                    .border_b_2()
                    .border_color(c(0x262626))
                    .bg(gpui::linear_gradient(
                        90.,
                        gpui::linear_color_stop(c(0x110b1b), 0.),
                        gpui::linear_color_stop(c(0x452568), 1.),
                    ))
                    .child(div().text_size(px(18.)).font_weight(FontWeight::BOLD).text_color(c(0xffffff)).child(title))
                    .child(
                        div()
                            .id("pivx-dialog-x")
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .text_size(px(24.))
                            .text_color(white_a(0.6))
                            .hover(|s| s.bg(white_a(0.1)).text_color(c(0xffffff)))
                            .cursor_pointer()
                            .on_click(cx.listener(|app, _, _, cx| app.pivx_close_dialog(cx)))
                            .child("×"),
                    ),
            )
            .child(body);
        let backdrop = cx.listener(|app, _: &gpui::MouseDownEvent, _, cx| app.pivx_close_dialog(cx));
        Some(
            gpui::deferred(
                div()
                    .id("pivx-overlay")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(ca(0, 0, 0, 0.9))
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, backdrop)
                    .child(card),
            )
            .with_priority(11)
            .into_any_element(),
        )
    }
}

