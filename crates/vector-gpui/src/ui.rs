//! The widescreen shell: window chrome, rail, chat list and the open chat. Every size and
//! offset is the Tauri layout's own (measured from `reference/` renders); the comment on a
//! block names the CSS selector it reproduces.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::AnimationExt;
use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, IntoElement, ListAlignment, ListState,
    MouseButton, ParentElement, Render, SharedString, Styled, Window, WindowControlArea, div, img,
    linear_color_stop, linear_gradient, list, prelude::*, px, svg,
};
use vector_core::chat::SerializableChat;
use vector_core::types::Message;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use serde_json::Value;
use vector_core::profile::SlimProfile;

use crate::backend::{self, UiEvent, account, messaging, sync};
use crate::composer::{CancelInput, SendText, TextInput};
use crate::data::{self, Row, Store};
use crate::inline::Target;
use crate::kit;
use crate::login::{LoggedIn, LoginView, ReauthClosed};
use crate::theme::*;
use crate::timeline;
use chrono::TimeZone;

const CHROME_H: f32 = 32.;
pub(crate) const RAIL_W: f32 = 201.;
/// The collapsed rail, its border included.
pub(crate) const RAIL_SLIM_W: f32 = 73.;
/// Narrower than this, the rail can't afford its labels.
const RAIL_AUTO_COLLAPSE_W: f32 = 1080.;
pub(crate) const LIST_W: f32 = 239.;
const ROW_PITCH: f32 = 54.;

type Shared = Rc<RefCell<Store>>;

enum Screen {
    /// Finding the account on disk.
    Booting,
    Login(Entity<LoginView>),
    Main,
}

pub struct VectorApp {
    screen: Screen,
    /// This build is older than the account's database: (this build, the one it needs).
    downgrade: Option<(String, String)>,
    /// Re-pairing the remote signer from Settings: the signer form over everything.
    reauth: Option<Entity<LoginView>>,
    /// The transport's view, as the backend last reported it.
    net_view: Option<Value>,
    net_seq: Option<u64>,
    /// I2P's settings, where this build has I2P.
    net_config: Option<Value>,
    /// I2P-Only with none of the account's relays inside I2P.
    net_stranded: bool,
    net_polling: bool,
    /// A new I2P address was asked for: its toast waits until it is in use.
    net_renewed: Option<std::time::Instant>,
    /// The Tor or I2P explainer, while open.
    net_info: Option<&'static str>,
    /// Backed by a live account (else fixtures, for parity renders).
    live: bool,
    store: Shared,
    open: Option<SharedString>,
    timeline: ListState,
    /// The open chat's rows, rebuilt when it changes rather than per frame.
    rows: Rc<Vec<Row>>,
    read_anchor: Option<data::Unread>,
    input: Entity<TextInput>,
    syncing: bool,
    /// Spoilers revealed, by message.
    revealed: Rc<RefCell<HashMap<String, HashSet<u32>>>>,
    /// Reads Timeouts again as the soonest one ends.
    timeout_expiry: Option<gpui::Task<()>>,
    started: std::time::Instant,
    /// What the composer is doing beyond a fresh message.
    mode: Mode,
    /// The row carrying the hover toolbar.
    hovered: Option<String>,
    /// The message whose reaction menu is open.
    picker: Option<String>,
    popup: Option<(kit::Popup, PopupAct)>,
    rekey: Option<crate::rekey::Rekey>,
    fail_reasons: Rc<RefCell<HashMap<String, String>>>,
    media: Rc<RefCell<crate::media::Media>>,
    emoji_picker: Entity<crate::picker::EmojiPicker>,
    /// The emoji picker is showing (for `picker`'s message, else the composer).
    picker_open: bool,
    /// The picker serves the status dialog's field.
    picker_status: bool,
    picker_closed: std::time::Instant,
    /// Bumped by each hover, so a stale leave doesn't hide the toolbar.
    hover_gen: u64,
    viewer: Option<Entity<crate::viewer::Viewer>>,
    model_viewer: Option<(Entity<crate::model3d::viewer::ModelViewer>, String, String)>,
    /// The attachment panel is open, and since when (its items fade in from then).
    attach: Option<std::time::Instant>,
    /// The attachment panel's Mini Apps view, while it shows.
    mini: Option<miniapps_ops::MiniPanel>,
    nexus: Option<crate::nexus::Nexus>,
    launch: Option<miniapps_ops::Launch>,
    perm_prompt: Option<nexus_ops::PermPrompt>,
    publish: Option<publish_ops::PublishForm>,
    pivx_wallet: Option<pivx_ops::Wallet>,
    pivx_dialog: Option<pivx_ops::Dialog>,
    file_preview: Option<Entity<crate::filepreview::FilePreview>>,
    /// The avatar cropper, and the picked file it cuts.
    cropper: Option<(Entity<crate::cropper::Cropper>, std::path::PathBuf)>,
    /// The pack creator's square crop, and where its answer goes.
    pack_details: Option<crate::packdetails::Details>,
    /// A link that arrived before an account was open.
    pending_link: Option<vector_core::deep_link::DeepLinkAction>,
    /// The composer's `/` command picker, and each chat's bot commands.
    commands: crate::commands::Picker,
    cmd_snaps: HashMap<String, crate::commands::Snapshot>,
    cmd_loading: HashSet<String>,
    /// A bot command's argument pills, in place of the draft.
    cmd_form: Option<Entity<crate::cmdform::CommandForm>>,
    /// The composer's `:shortcode` suggestions.
    shortcode: Option<crate::shortcodes::Popup>,
    /// `:)` and friends suggest their emoji.
    emoticons: bool,
    /// The composer's `@` or `#` suggestions.
    mention: Option<crate::mentions::Popup>,
    /// The `@time` picker.
    time_pick: Option<crate::texttime::Picker>,
    /// The `@time` an Escape closed, until the draft moves past it.
    time_dismissed: Option<usize>,
    /// A time phrase the draft ends with, offered as a time code.
    time_chip: Option<crate::texttime::Chip>,
    /// Suggestions passed on for this draft.
    chip_passed: HashSet<String>,
    time_suggestions: bool,
    /// The composer box as last painted, to put the chip under its phrase.
    composer_box: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// The chat header's ⋯ as last painted, which its menus hang from.
    chat_menu_btn: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    new_btn: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    signer_offline_told: bool,
    call_mic_told: Option<String>,
    jump_miss: Option<(String, &'static str)>,
    /// The window is narrower than widescreen: the slim layout.
    is_slim: bool,
    /// The window's width as of this frame, for helpers that don't take a `Window`.
    vw: f32,
    slim: slim_ops::Slim,
    send_btn: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// A press on Send, by number, while it is held; a long one opened the timer instead.
    send_held: Option<u64>,
    send_long: bool,
    /// The open chat's Self-Destruct Timer, in seconds.
    self_destruct: Option<u64>,
    wallpaper: crate::wallpaper::Wallpaper,
    /// The chat pane as last painted, which the wallpaper covers.
    chat_box: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// Display → Background Wallpaper.
    chat_bg: bool,
    popout: crate::popout::Popout,
    rec: crate::recorder::Recorder,
    /// Who a `@` can name in the open chat, gathered once per open panel.
    mention_pool: Option<Rc<Vec<crate::mentions::Item>>>,
    /// The draft's picked mentions: the name shown, and what it sends as.
    tracked: Vec<(String, String)>,
    /// Each Community's taggable Roles and @everyone rule.
    mention_rules: HashMap<String, crate::mentions::Rules>,
    /// The draft has text: Send stands where the mic does.
    send_shown: bool,
    /// Typed flips between the two, each animated once.
    send_flips: usize,
    /// The message a jump landed on, flashing.
    jumped: Option<(String, std::time::Instant)>,
    /// Older history is on its way.
    paging: bool,
    /// A jump asked for while the chat's first page loads.
    opening: Option<Option<String>>,
    /// Chats whose first message is loaded.
    history_done: HashSet<String>,
    /// A Community channel's history past the database: all fetched, or the last ask failed.
    history_net: HashMap<String, bool>,
    history_net_busy: HashSet<String>,
    pack_crop: Option<(
        Entity<crate::cropper::Cropper>,
        futures::channel::oneshot::Sender<Option<vector_core::image_crop::CropRect>>,
    )>,
    /// The expanded profile, shown in place of the chat.
    profile: Option<crate::profile::Screen>,
    toast: Option<(SharedString, std::time::Instant)>,
    /// The Status dialog's field, while it is open.
    status: Option<Entity<TextInput>>,
    /// The QR overlay's code, and what holds focus while it shows.
    qr: Option<(std::sync::Arc<gpui::RenderImage>, gpui::FocusHandle)>,
    switcher: Option<crate::switcher::Switcher>,
    badge_card: Option<crate::badgecard::Card>,
    new_chat: Option<crate::newchat::NewChat>,
    create_community: Option<crate::createcommunity::CreateCommunity>,
    invite: Option<crate::invite::Invite>,
    comm_settings: Option<crate::commsettings::CommSettings>,
    /// The cropper over a Community's art, and the pick it is cutting.
    comm_crop: Option<(
        Entity<crate::cropper::Cropper>,
        art_ops::Art,
        crate::commsettings::Pick,
    )>,
    moderation: Option<crate::moderation::Moderation>,
    /// Each community's last raid check: `(detected, suspects)`.
    raid: HashMap<String, (bool, usize)>,
    /// The expanded Nostr post, and a reference it waits on.
    embed_modal: Option<crate::embeds::Modal>,
    embed_wanted: Option<(String, Option<String>)>,
    /// The pending invite whose details show.
    invite_details: Option<String>,
    /// Invite links looked up, by invite key.
    invite_previews: HashMap<String, crate::newchat::Preview>,
    /// The open chat as last scanned for invite links: its id, length and newest message.
    invite_scan: Option<(String, usize, String)>,
    /// Add Profile is showing the login screen over this account: the account Back returns to,
    /// and whether the old session was already torn down for the new one.
    adding: Option<(String, bool)>,
    /// The list shows Requests rather than Messages; `tab_slid` once a pick moved the bar.
    requests_tab: bool,
    tab_slid: bool,
    chatlist: ListState,
    chatlist_keys: RefCell<Vec<(u8, String)>>,
    /// Rail folders open on this device.
    rail_open: HashSet<String>,
    /// The user's choice to collapse the rail; a narrow window collapses it regardless.
    rail_pref: bool,
    archive: chatlist_ops::Fold,
    /// The request senders last asked about for shared Communities.
    mutuals_asked: Vec<String>,
    /// Viktor, while the Messages list is empty.
    viktor: Option<Entity<crate::viktor::Viktor>>,
    settings: Option<crate::settings::Settings>,
    security: Entity<crate::security::SecurityFlow>,
    /// The release check, and where its find is installed from.
    updates: crate::settings::Updates,
    update_url: Option<String>,
    /// A volume change waits on its throttle.
    audio_save_pending: bool,
    call: crate::call::Call,
    video: crate::callvideo::Video,
    screen_pick: Option<crate::screenpick::Picker>,
    call_seen: crate::call::Seen,
    voice_seen: crate::voice::Seen,
    /// Members' pictures the worker decodes, by key (hex) and track.
    voice_wanted: std::collections::HashSet<(String, usize)>,
    /// The processing switches the call panel shows, read when a call first appears.
    call_audio: Option<vector_core::calls::settings::AudioSettings>,
    call_ticking: bool,
    /// The open Community, whose channels replace the chat list.
    community: Option<crate::community::View>,
    create_channel: Option<crate::createchannel::CreateChannel>,
    channel_editor: Option<crate::chaneditor::ChannelEditor>,
    /// The menus a submenu was opened from, for its row back.
    menu_trail: Vec<(Vec<kit::MenuItem>, Vec<MenuAct>)>,
    scanner: Option<qrscan_ops::Scanner>,
    /// Scanner frames off screen, for the atlas to let go of.
    retired_frames: Vec<Arc<gpui::RenderImage>>,
    start: Option<crate::startmenu::StartMenu>,
    /// The context menu open: where, its rows, and what each does.
    menu: Option<(gpui::Point<gpui::Pixels>, Vec<kit::MenuItem>, Vec<MenuAct>)>,
    /// The menu slider being dragged.
    menu_sliding: Rc<Cell<Option<usize>>>,
    voice: crate::voice::Voice,
    voice_ticking: bool,
    mini_profile: Option<crate::miniprofile::Mini>,
    pins: crate::pins::Pins,
    edit_history: Option<crate::edithistory::EditHistory>,
    reaction_tip: Option<crate::reactpop::Pop>,
    reaction_details: Option<crate::reactpop::Pop>,
    rx_hover: Option<(String, String)>,
    rx_gen: u64,
    rx_long: bool,
    bar: crate::scrollbar::Bar,
    /// A seek's run and its newest message, while a gap lies between it and the held tail.
    gap: Option<(String, String)>,
    paging_newer: bool,
    /// Messages that arrived below a reader scrolled up.
    unread_below: u32,
    return_shown: bool,
    /// The typing strip's shadow: none while pinned, full a few pixels up the history.
    typing_fade: f32,
    /// When this client last told the open chat it was typing, and the draft's length then.
    typing_sent: Option<(String, std::time::Instant)>,
    draft_len: usize,
    secret_guard: Option<crate::secretguard::Guard>,
    /// The next send has been through the secret guard.
    secret_pass: bool,
    anchors: kit::Anchors,
    /// The role a member's change is under way for.
    role_busy: Option<String>,
}

#[derive(Clone, PartialEq)]
enum Mode {
    Normal,
    Reply {
        id: String,
        name: String,
        snippet: String,
    },
    Edit {
        id: String,
    },
}

/// What a popup's confirm does.
#[derive(Clone)]
/// What a context menu row does.
enum MenuAct {
    FoldRail,
    /// The Messages door's catch-up: every unread DM.
    ReadAllDms,
    AddFriend,
    Bookmark,
    AppStore,
    StartDetails(String),
    StartCopyLink(String),
    NewMessage,
    NewCommunity,
    BannerHidden(String, bool),
    StartFolder(String),
    StartRename(String),
    StartUngroup(String),
    StartMoveOut(String),
    StartUnpin(String),
    StartTogglePin(String),
    Device(&'static str, Option<String>, backend::mic::DevicePrefs),
    /// Rows that act on nothing: dividers and headers.
    Nothing,
    /// Drill into a submenu, behind a row back.
    Sub(SharedString, Vec<kit::MenuItem>, Vec<MenuAct>),
    Back,
    CommunityRead(String),
    CreateChannel {
        community: String,
        category: Option<String>,
    },
    CreateCategory(String),
    InvitePeople(String),
    CommunitySettings(String),
    CommunityNickname(String),
    Moderation(String),
    LeaveCommunity {
        id: String,
        name: String,
        owner: bool,
    },
    Copy(String),
    NotifyLevel(String, Option<&'static str>),
    NotifyMute(String, i64),
    SuppressEveryone(String, bool),
    ChannelRead(String),
    ChatRead(String),
    ChatUnread(String),
    ChatPin(String, bool),
    ChatArchive(String, bool),
    ChatBlock(String),
    PackShare(String),
    PackEdit(String),
    PackRemove(String),
    CreatorRename(usize),
    CreatorDelete(usize),
    /// The chat's ⋯: open its Self-Destruct Timer picker, or set the timer.
    SelfDestruct(String),
    SetSelfDestruct(String, Option<u64>),
    ChangeWallpaper(String),
    RemoveWallpaper(String),
    EditChannel {
        community: String,
        channel: String,
        category: bool,
    },
    DeleteChannel {
        community: String,
        channel: String,
        category: bool,
    },
    CopyId(String, &'static str),
    NexusLink(String),
    /// A rail folder's communities read at once.
    RailRead(Vec<String>),
    RailRename(String),
    RailHue(String, Option<u16>),
    RailUngroup(String),
    VoiceVolume(String, f32),
    /// A voice member's volume slider: its moves land in `on_menu_slide`.
    VoiceSlide(String),
    VoiceModerate {
        community: String,
        channel: String,
        npub: String,
        action: &'static str,
        to: Option<String>,
    },
    ViewProfile(String),
    MemberNickname(String, String),
    /// View Profile atop a member's menu: (npub, community, anchor key).
    MemberMini(String, String, SharedString),
    MemberTimeout(String, String, u64),
    /// Kick, or (true) ban, once confirmed.
    MemberRemove(String, String, bool),
    /// A message's menu row.
    Msg(String, msgmenu_ops::MsgAct),
    /// Give (`on`) or take a role; the menu stays open and repaints.
    MemberRole {
        community: String,
        npub: String,
        role: String,
        on: bool,
    },
}

enum PopupAct {
    OpenMicSettings,
    MoveRelays { id: String, relays: Vec<String> },
    /// A moderator's hide of someone else's message, once confirmed.
    Hide(String),
    RemoveMember {
        community: String,
        npub: String,
        ban: bool,
    },
    RailRename(String),
    NexusUninstall(String),
    Notice,
    OpenWeb(String),
    DeleteChannel {
        community: String,
        channel: String,
        category: bool,
    },
    RemoveWallpaper(String),
    DeleteModel(&'static str),
    ModRevoke,
    ModRotate,
    ModBanRotate,
    RenameFolder {
        id: String,
    },
    Delete {
        id: String,
        failed: bool,
    },
    Block {
        npub: String,
    },
    /// Block from a request's bar, which also closes the chat.
    BlockRequest {
        npub: String,
    },
    Nickname {
        npub: String,
    },
    RemoveAccount {
        npub: String,
    },
    Unblock {
        npub: String,
    },
    Logout,
    ClearStorage,
    RemoveMiniApp {
        name: String,
        pivx: bool,
    },
    /// A multiplayer Mini App launch waiting on consent to connect outside the network.
    RealtimeThen(Box<dyn FnOnce(&mut VectorApp, &mut Context<VectorApp>)>),
    JoinInvite {
        key: String,
        name: String,
    },
    LeaveCommunity {
        id: String,
        name: String,
        owner: bool,
    },
    /// The first link anywhere makes the Community public: then ask for its label.
    InvitePublic,
    InviteLabel,
    /// Revoking the last link anywhere makes it private again.
    InvitePrivate {
        token: String,
    },
    DeleteRole(String),
    CommunityNickname {
        community: String,
        npub: String,
    },
    ActivateRoles,
    AddServer,
    DisableRelay {
        url: String,
    },
    RemoveRelay {
        url: String,
    },
    RemoveServer {
        url: String,
    },
    /// Delete one slice of the storage chart: its category and the extensions it counted.
    DeleteSlice {
        category: &'static str,
        exts: Vec<String>,
    },
    /// Turn Keep Files off, deleting what it kept.
    StopKeeping,
    KeepPlain,
    /// Agree to connect outside the chosen network, then go on with the call.
    Realtime(CallGo),
    /// Switch networks though the router check failed.
    NetUse(String),
    /// Keep (yes) or end (no) the Mini App session across a switch.
    NetKeepRealtime(String),
    NetI2pOnly,
    NetOpRemove(usize),
    NetOpReset,
}

/// A call step that waits on consent to connect outside the network.
#[derive(Clone)]
enum CallGo {
    Start(String),
    Accept,
}

#[path = "ui_art.rs"]
mod art_ops;
#[path = "ui_slim.rs"]
mod slim_ops;
#[path = "ui_chaneditor.rs"]
mod chaneditor_ops;
#[path = "ui_commands.rs"]
mod commands_ops;
#[path = "ui_moderation.rs"]
mod moderation_ops;
#[path = "ui_popout.rs"]
mod popout_ops;
#[path = "ui_miniapps.rs"]
mod miniapps_ops;
#[path = "ui_nexus.rs"]
mod nexus_ops;
#[path = "ui_publish.rs"]
mod publish_ops;
#[path = "ui_pivx.rs"]
mod pivx_ops;
#[path = "ui_chatlist.rs"]
mod chatlist_ops;
#[path = "ui_qrscan.rs"]
mod qrscan_ops;
#[path = "ui_rail.rs"]
mod rail_ops;
#[path = "ui_voice.rs"]
mod voice_ops;
#[path = "ui_miniprofile.rs"]
mod miniprofile_ops;
#[path = "ui_members.rs"]
mod members_ops;
#[path = "ui_scrollbar.rs"]
mod ui_scrollbar;
#[path = "ui_routing.rs"]
mod ui_routing;
#[path = "ui_pins.rs"]
mod pins_ops;
#[path = "ui_msgmenu.rs"]
mod msgmenu_ops;
#[path = "ui_timeouts.rs"]
mod timeouts_ops;
#[path = "ui_secretguard.rs"]
mod secretguard_ops;

impl VectorApp {
    fn base(store: Store, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store: Shared = Rc::new(RefCell::new(store));
        let input = cx.new(|cx| {
            let mut t = TextInput::new("Enter message...", cx)
                .multiline(px(126.))
                .twemoji()
                .decorated();
            let names = Rc::downgrade(&store);
            t.set_npub_labels(Rc::new(move |npub: &str| {
                names
                    .upgrade()
                    .and_then(|s| s.try_borrow().ok().map(|s| s.name(npub)))
                    .unwrap_or_else(|| crate::commsettings::short_npub(npub))
            }));
            t
        });
        {
            // Nobody here watches theirs while we're out of sight: the peer may stop spending upload.
            let app = cx.entity().downgrade();
            window
                .observe_window_visibility(move |vis, _, cx| {
                    app.update(cx, |app, _| {
                        if app.video.link_open() {
                            let hidden = !vis.is_visible();
                            drop(backend::run(async move { vector_core::calls::session::set_video_pause(hidden).await }));
                        }
                    })
                    .ok();
                })
                .detach();
        }
        cx.subscribe(&input, |app, _, SendText(text), cx| {
            app.send(text.clone(), cx)
        })
        .detach();
        cx.subscribe_in(
            &input,
            window,
            |app, _, pasted: &crate::composer::Pasted, window, cx| app.on_paste(pasted, window, cx),
        )
        .detach();
        cx.subscribe(&input, |app, _, _: &CancelInput, cx| app.cancel_mode(cx))
            .detach();
        cx.subscribe(&input, |app, _, _: &crate::composer::InputChanged, cx| {
            app.update_shortcode(cx);
            app.typed(cx);
        })
        .detach();
        cx.subscribe_in(
            &input,
            window,
            |app, _, nav: &crate::composer::Nav, window, cx| app.shortcode_nav(*nav, window, cx),
        )
        .detach();
        let emoji_picker = cx.new(crate::picker::EmojiPicker::new);
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, crate::picker::Picked(emoji, shift), window, cx| {
                app.picked(emoji.clone(), *shift, window, cx)
            },
        )
        .detach();
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, _: &crate::picker::Dismissed, window, cx| {
                // A crop or a menu the creator raised sits outside the panel.
                if app.pack_crop.is_none() && app.menu.is_none() {
                    app.close_picker(window, cx);
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, crate::picker::CropFor(image, natural, answer), window, cx| {
                let Some(tx) = answer.borrow_mut().take() else {
                    return;
                };
                let (image, natural) = (image.clone(), *natural);
                let cropper = cx.new(|cx| {
                    crate::cropper::Cropper::new(
                        image,
                        natural,
                        "Crop to Square",
                        "Drag to move. Drag a corner to resize.",
                        false,
                        window,
                        cx,
                    )
                });
                cx.subscribe(&cropper, |app, _, answer: &crate::cropper::Answer, cx| {
                    if let Some((_, tx)) = app.pack_crop.take() {
                        let _ = tx.send(match answer {
                            crate::cropper::Answer::Crop(r) => Some(r.clone()),
                            crate::cropper::Answer::Cancel => None,
                        });
                    }
                    cx.notify();
                })
                .detach();
                app.pack_crop = Some((cropper, tx));
                cx.notify();
            },
        )
        .detach();
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, crate::picker::CellMenu(idx, at), _, cx| {
                let rows = vec![
                    (
                        kit::MenuItem::new("Rename Emoji", "edit"),
                        MenuAct::CreatorRename(*idx),
                    ),
                    (
                        kit::MenuItem::new("Delete Emoji", "trash").danger(),
                        MenuAct::CreatorDelete(*idx),
                    ),
                ];
                app.show_menu(*at, rows, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, _: &crate::picker::ShowAgain, window, cx| {
                if !app.picker_open {
                    app.picker_closed =
                        std::time::Instant::now() - std::time::Duration::from_secs(1);
                    app.toggle_picker(None, window, cx);
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, crate::picker::PickedCustom(code, url, shift), window, cx| {
                app.picked_custom(code.clone(), url.clone(), *shift, window, cx)
            },
        )
        .detach();
        cx.subscribe(
            &emoji_picker,
            |app, _, crate::picker::RemovePack(id), cx| app.remove_pack(id.clone(), cx),
        )
        .detach();
        cx.subscribe(
            &emoji_picker,
            |app, _, crate::picker::PackMenu(id, own, theme, at), cx| {
                let mut rows = vec![(
                    kit::MenuItem::new("Share Pack", "share"),
                    MenuAct::PackShare(id.clone()),
                )];
                if *own {
                    rows.push((
                        kit::MenuItem::new("Edit Pack", "edit"),
                        MenuAct::PackEdit(id.clone()),
                    ));
                }
                if !*theme {
                    rows.push((
                        kit::MenuItem::new("Remove Pack", "x-user").danger(),
                        MenuAct::PackRemove(id.clone()),
                    ));
                }
                app.show_menu(*at, rows, cx);
            },
        )
        .detach();
        cx.subscribe(
            &emoji_picker,
            |app, _, crate::picker::ReorderPacks(ids), _| {
                if !app.live {
                    return;
                }
                let ids = ids.clone();
                // The order is already on screen; a failure only needs the log.
                let _ = backend::run(async move {
                    if let Err(e) = vector_core::emoji_packs::reorder_emoji_packs(ids) {
                        vector_core::log_warn!("[EmojiPacks] reorder failed: {e}");
                    }
                });
            },
        )
        .detach();
        // A GIF alone in the draft sends at once; beside text it waits for the user's Enter.
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, crate::picker::PickedGif(url), window, cx| {
                let was_empty = app.input.read(cx).text().trim().is_empty();
                app.input
                    .update(cx, |input, cx| input.insert_spaced(url, window, cx));
                app.close_picker(window, cx);
                if was_empty {
                    app.input.update(cx, |input, cx| input.submit(window, cx));
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &emoji_picker,
            window,
            |app, _, crate::picker::PickedText(face, shift, recents), window, cx| {
                if app.live {
                    let recents = recents.clone();
                    let _ =
                        backend::run(async move { backend::actions::set_kaomoji_recents(recents) });
                }
                let field = if app.picker_status {
                    app.status.clone()
                } else {
                    None
                }
                .unwrap_or_else(|| app.input.clone());
                field.update(cx, |input, cx| input.insert_spaced(face, window, cx));
                if !*shift {
                    app.close_picker(window, cx);
                }
            },
        )
        .detach();
        let security = cx.new(|cx| crate::security::SecurityFlow::new(live, window, cx));
        cx.subscribe_in(
            &security,
            window,
            |app, _, e: &crate::security::Event, window, cx| app.on_security(e, window, cx),
        )
        .detach();
        if !live {
            backend::fixture_runtime();
        }
        if let Some(mut rx) = crate::audio::listen() {
            cx.spawn(async move |this, cx| {
                use futures::StreamExt;
                while let Some((name, v)) = rx.next().await {
                    if this
                        .update(cx, |this, cx| this.on_audio_event(&name, v, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        Self {
            screen: Screen::Main,
            downgrade: None,
            reauth: None,
            net_view: None,
            net_seq: None,
            net_config: None,
            net_stranded: false,
            net_polling: false,
            net_renewed: None,
            net_info: None,
            live,
            store,
            open: None,
            timeline: ListState::new(0, ListAlignment::Top, px(600.)),
            rows: Rc::default(),
            read_anchor: None,
            input,
            syncing: false,
            revealed: Rc::default(),
            timeout_expiry: None,
            started: std::time::Instant::now(),
            mode: Mode::Normal,
            hovered: None,
            picker: None,
            popup: None,
            rekey: None,
            fail_reasons: Rc::default(),
            media: Rc::default(),
            pack_details: None,
            pending_link: None,
            commands: Default::default(),
            cmd_snaps: HashMap::new(),
            cmd_loading: HashSet::new(),
            cmd_form: None,
            shortcode: None,
            emoticons: true,
            mention: None,
            time_pick: None,
            time_dismissed: None,
            time_chip: None,
            chip_passed: HashSet::new(),
            time_suggestions: true,
            composer_box: Rc::default(),
            chat_menu_btn: Rc::default(),
            new_btn: Rc::default(),
            signer_offline_told: false,
            call_mic_told: None,
            jump_miss: None,
            is_slim: false,
            vw: 0.,
            slim: slim_ops::Slim::new(),
            send_btn: Rc::default(),
            send_held: None,
            send_long: false,
            self_destruct: None,
            wallpaper: Default::default(),
            chat_box: Rc::default(),
            chat_bg: true,
            popout: Default::default(),
            rec: Default::default(),
            mention_pool: None,
            tracked: Vec::new(),
            mention_rules: HashMap::new(),
            send_shown: false,
            send_flips: 0,
            jumped: None,
            paging: false,
            opening: None,
            history_done: HashSet::new(),
            history_net: HashMap::new(),
            history_net_busy: HashSet::new(),
            pack_crop: None,
            emoji_picker,
            picker_open: false,
            picker_status: false,
            picker_closed: std::time::Instant::now(),
            hover_gen: 0,
            viewer: None,
            model_viewer: None,
            cropper: None,
            attach: None,
            mini: None,
            nexus: None,
            launch: None,
            perm_prompt: None,
            pivx_wallet: None,
            pivx_dialog: None,
            publish: None,
            file_preview: None,
            profile: None,
            toast: None,
            status: None,
            qr: None,
            switcher: None,
            badge_card: None,
            new_chat: None,
            create_community: None,
            invite: None,
            comm_settings: None,
            comm_crop: None,
            moderation: None,
            raid: HashMap::new(),
            embed_modal: None,
            embed_wanted: None,
            invite_details: None,
            invite_previews: HashMap::new(),
            invite_scan: None,
            adding: None,
            requests_tab: false,
            chatlist: ListState::new(0, ListAlignment::Top, px(400.)),
            chatlist_keys: RefCell::new(Vec::new()),
            rail_open: HashSet::new(),
            rail_pref: backend::device::get("ws_rail_collapsed").as_deref() == Some("true"),
            archive: Default::default(),
            tab_slid: false,
            mutuals_asked: Vec::new(),
            viktor: None,
            settings: None,
            security,
            updates: crate::settings::Updates {
                beta: backend::updates::beta(),
                ..Default::default()
            },
            update_url: None,
            audio_save_pending: false,
            call: crate::call::Call::at(Self::saved_pill_pos()),
            video: Default::default(),
            screen_pick: None,
            call_seen: Default::default(),
            voice_seen: Default::default(),
            voice_wanted: Default::default(),
            call_audio: None,
            call_ticking: false,
            community: None,
            create_channel: None,
            channel_editor: None,
            menu_trail: Vec::new(),
            scanner: None,
            retired_frames: Vec::new(),
            start: None,
            menu: None,
            menu_sliding: Default::default(),
            voice: Default::default(),
            voice_ticking: false,
            mini_profile: None,
            pins: Default::default(),
            edit_history: None,
            reaction_tip: None,
            reaction_details: None,
            rx_hover: None,
            rx_gen: 0,
            rx_long: false,
            bar: Default::default(),
            gap: None,
            paging_newer: false,
            unread_below: 0,
            return_shown: false,
            typing_fade: 0.,
            typing_sent: None,
            draft_len: 0,
            secret_guard: None,
            secret_pass: false,
            anchors: Default::default(),
            role_busy: None,
        }
    }

    /// A 3D model's frame landed: paint it.
    fn listen_video(&mut self, cx: &mut Context<Self>) {
        let Some((mut rx, pending)) = self.video.take_wake() else { return };
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while rx.next().await.is_some() {
                pending.store(false, std::sync::atomic::Ordering::Release);
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Video, each frame: the link follows the call (or the voice Channel's), the stage's
    /// wants are told, stopped captures are told, and the newest pictures are taken.
    fn video_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::callvideo::VideoKind;
        let key = if self.call.live() {
            self.call
                .peer_decodes
                .then(|| self.call.id.clone())
                .flatten()
        } else {
            self.voice
                .session
                .as_ref()
                .map(|s| format!("voice:{}", s.channel_id))
        };
        self.video.follow(key.as_deref());
        let kinds = [VideoKind::Camera, VideoKind::Screen];
        let voice = self.voice.session.as_ref().filter(|_| !self.call.live());
        let mine = match voice {
            Some(v) => [v.camera, v.sharing],
            None => self.call.video_mine,
        };
        for (k, kind) in kinds.into_iter().enumerate() {
            // The call stopped a picture of ours: the capture stops with it.
            if self.video.capturing(kind) && !mine[k] && !self.video.starting[k] {
                self.video.stop(kind);
            }
        }
        let lost: Vec<_> = std::mem::take(&mut self.video.pics().lost);
        for kind in lost {
            self.set_video(kind.index(), false, cx);
        }
        self.voice_watch();
        let mut p = self.video.pics();
        for old in p.retired.drain(..) {
            let _ = window.drop_image(old);
        }
        self.call_seen = crate::call::Seen {
            peer: std::array::from_fn(|k| p.peer[k].as_ref().map(|x| (x.image.clone(), x.w, x.h))),
            mine: std::array::from_fn(|k| p.mine[k].as_ref().map(|x| x.image.clone())),
            dec_fps: p.dec_fps,
            can_send: self.call.peer_decodes,
        };
        self.voice_seen = crate::voice::Seen {
            members: p
                .members
                .iter()
                .map(|(k, v)| (k.clone(), v.image.clone()))
                .collect(),
            mine: std::array::from_fn(|k| p.mine[k].as_ref().map(|x| x.image.clone())),
        };
    }

    /// The stage asks for what it shows of each member, and the worker decodes just that.
    fn voice_watch(&mut self) {
        let me = self.store.borrow().my_npub.clone();
        let shown = self.open.as_deref().map(str::to_string);
        let wants = self.voice.wants(shown.as_deref(), &me);
        let mut decode = std::collections::HashSet::new();
        for (npub, (screen, camera)) in &wants {
            if let Some(key) = self.voice.key_of(npub) {
                if *screen {
                    decode.insert((key.to_string(), 1));
                }
                if *camera {
                    decode.insert((key.to_string(), 0));
                }
            }
        }
        let kinds = [
            crate::callvideo::VideoKind::Camera,
            crate::callvideo::VideoKind::Screen,
        ];
        for (key, k) in decode.difference(&self.voice_wanted) {
            self.video.want(key.clone(), kinds[*k], true);
        }
        for (key, k) in self.voice_wanted.difference(&decode) {
            self.video.want(key.clone(), kinds[*k], false);
        }
        self.voice_wanted = decode;
        if !self.live {
            self.voice.watched = wants;
            return;
        }
        let before = std::mem::take(&mut self.voice.watched);
        for npub in before
            .keys()
            .chain(wants.keys())
            .collect::<std::collections::HashSet<_>>()
        {
            let (a, b) = (before.get(npub), wants.get(npub));
            if a == b {
                continue;
            }
            let (screen, camera) = b.copied().unwrap_or((false, false));
            let _ = backend::run(backend::voice::watch(npub.clone(), screen, camera));
        }
        self.voice.watched = wants;
    }

    /// Tell whichever call we're in that a picture of ours started or stopped.
    fn video_backend(
        &self,
        k: usize,
        on: bool,
    ) -> futures::channel::oneshot::Receiver<Result<(), String>> {
        let kind = [
            crate::callvideo::VideoKind::Camera,
            crate::callvideo::VideoKind::Screen,
        ][k];
        if self.call.live() || self.voice.session.is_none() {
            backend::run(backend::calls::set_video(kind, on))
        } else {
            backend::run(backend::voice::set_video(kind, on))
        }
    }

    fn listen_models(&mut self, cx: &mut Context<Self>) {
        let Some(mut rx) = self.media.borrow_mut().models.take_wake() else { return };
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while rx.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Fixture data, offline: what the parity renders compare.
    pub fn fixtures(
        store: Store,
        open: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut app = Self::base(store, false, window, cx);
        app.listen_models(cx);
        app.listen_video(cx);
        app.reload_picker_packs(cx);
        if let Some(v) = app.net_initial() {
            app.on_transport_view(v, cx);
        }
        if let Some(id) = open {
            app.open_chat(id.into(), window, cx);
        }
        if let Ok(who) = std::env::var("VECTOR_PROFILE") {
            let npub = if who == "me" {
                app.store.borrow().my_npub.clone()
            } else {
                who
            };
            app.open_profile(npub, cx);
        }
        if let Ok(mode) = std::env::var("VECTOR_LOGIN") {
            let signer = mode.strip_prefix("signer");
            app.show_login((mode != "welcome" && signer.is_none()).then_some("pin"), window, cx);
            if let (Some(rest), Screen::Login(view)) = (signer, &app.screen) {
                view.clone().update(cx, |v, cx| v.fixture_signer(rest, window, cx));
            }
        }
        if let Ok(n) = std::env::var("VECTOR_BADGE_CARD").map(|v| v.parse::<usize>().unwrap_or(1))
            && let Some(p) = app.profile.as_ref()
        {
            use crate::profile::Badge;
            let b = p.badges;
            let held: Vec<Badge> = [
                (b.invites > 0, Badge::Inviter),
                (b.fawkes, Badge::Fawkes),
                (b.bug > 0, Badge::BugHunter),
                (b.v2026, Badge::V2026),
            ]
            .into_iter()
            .filter_map(|(on, w)| on.then_some(w))
            .collect();
            if let Some(w) = held.get(n.saturating_sub(1)) {
                app.on_profile(crate::profile::Act::Badge(*w), window, cx);
            }
        }
        // Fixtures: VECTOR_DOWNGRADE=<version> shows the takeover for an account that version wrote.
        if let Ok(v) = std::env::var("VECTOR_DOWNGRADE") {
            app.screen = Screen::Booting;
            app.downgrade = Some(crate::downgrade::versions(&vector_core::db::DowngradeBlock { db_schema: 90, supported_schema: 87, last_app_version: Some(v) }));
        }
        // Fixtures: VECTOR_SETTINGS=<category> opens Settings there.
        if let Ok(cat) = std::env::var("VECTOR_SETTINGS") {
            let cats = crate::settings::categories(&crate::settings::Offers {
                voice: std::env::var_os("VECTOR_TRANSCRIPT").is_some(),
                routing: true,
                updates: true,
            });
            let id = cats
                .iter()
                .find(|c| c.id == cat)
                .map(|c| c.id)
                .unwrap_or("appearance");
            app.open_settings(id, window, cx);
            if let Some(s) = app.settings.as_mut() {
                s.slim_open = Some(id);
            }
            // VECTOR_TOGGLES=key,key turns those switches on; VECTOR_MOVING=done/total[/plain].
            if let Some(s) = app.settings.as_mut() {
                for key in std::env::var("VECTOR_TOGGLES").unwrap_or_default().split(',') {
                    if let Some(t) = crate::settings::toggle_of(key) {
                        s.on.insert(t.key, true);
                    }
                }
                if let Ok(m) = std::env::var("VECTOR_MOVING") {
                    let p: Vec<&str> = m.split('/').collect();
                    let n = |i: usize| p.get(i).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                    s.moving = Some((format!("{} of {}", crate::security::thousands(n(0)), crate::security::thousands(n(1))), p.len() > 2));
                }
            }
        }
        // Fixtures: VECTOR_REC=recording|locked|preview holds the voice recorder in that state.
        if let Ok(state) = std::env::var("VECTOR_REC") {
            app.rec.fixture(&state);
            app.rec.can_dictate = std::env::var_os("VECTOR_TRANSCRIPT").is_some();
        }
        // Fixtures: VECTOR_NEXUS=discover|library|all|<app id> opens the App Store there.
        if let Ok(at) = std::env::var("VECTOR_NEXUS") {
            use crate::nexus::View;
            let view = match at.as_str() {
                "discover" => Some(View::Discover),
                "library" => Some(View::Library),
                "all" => Some(View::All),
                "" | "default" => None,
                _ => Some(View::Discover),
            };
            app.show_nexus(view, window, cx);
            if !matches!(at.as_str(), "discover" | "library" | "all" | "" | "default") {
                app.on_nexus(crate::nexus::Act::Details(at.clone()), window, cx);
                // VECTOR_NEXUS_PICK=1 opens its Play with Friends picker.
                if std::env::var_os("VECTOR_NEXUS_PICK").is_some() {
                    app.on_nexus(crate::nexus::Act::PlayWith(at), window, cx);
                }
            }
        }
        // Fixtures: VECTOR_XDC=<package> launches it solo, through the real host and bridge.
        if let Ok(path) = std::env::var("VECTOR_XDC") {
            crate::miniapp::init(|_| {});
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::miniapp::open(path, "solo".into(), String::new(), None, None).await {
                    eprintln!("VECTOR_XDC: {e}");
                }
            });
        }
        // Fixtures: VECTOR_CALL=ringing|calling|active|open|video|video-open|video-max shows a call with alice in that state
        // (VECTOR_CALL_PICS=1 fills the pictures from reference/media).
        if let Ok(state) = std::env::var("VECTOR_CALL") {
            app.fixture_call(&state, cx);
        }
        // VECTOR_VOICE=video with VECTOR_CALL_PICS=1: alice's camera and screen, and our camera.
        if std::env::var("VECTOR_VOICE").as_deref() == Ok("video")
            && std::env::var("VECTOR_CALL_PICS").is_ok()
        {
            let load = |f: &str| {
                image::open(format!("reference/media/{f}"))
                    .ok()
                    .map(|i| i.to_rgba8())
            };
            let alice = "1".repeat(64);
            let members = [
                ((alice.clone(), 0), load("portrait.png")),
                ((alice, 1), load("cover.png")),
            ];
            app.video.show_fixture(
                [None, None],
                [load("photo.png"), None],
                members
                    .into_iter()
                    .filter_map(|(k, i)| Some((k, i?)))
                    .collect(),
            );
        }
        // VECTOR_LOCK=blocked|dissolved|perms|writer|timeout: why the composer can't send.
        if let Ok(lock) = std::env::var("VECTOR_LOCK") {
            app.fixture_lock(&lock, window, cx);
        }
        // Fixtures: VECTOR_COMMUNITY=<name> opens that Community; VECTOR_CHANNEL=<name> a channel in it.
        if let Ok(name) = std::env::var("VECTOR_COMMUNITY") {
            let found = app
                .store
                .borrow()
                .communities
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.community_id.clone());
            if let Some(id) = found {
                app.open_community(id.clone(), window, cx);
                let channel = std::env::var("VECTOR_CHANNEL").ok().and_then(|n| {
                    let store = app.store.borrow();
                    store
                        .community_of(app.open.as_deref()?)?
                        .channels
                        .iter()
                        .find(|c| c.name == n)
                        .map(|c| c.channel_id.clone())
                });
                if let Some(ch) = channel {
                    app.open_channel(ch, window, cx);
                }
                // VECTOR_COMMUNITY_MENU shows its header menu; VECTOR_CREATE=channel|category the modal.
                if std::env::var("VECTOR_COMMUNITY_MENU").is_ok() {
                    app.on_community(crate::community::Act::HeadMenu, window, cx);
                }
                match std::env::var("VECTOR_CREATE").as_deref() {
                    Ok("channel") => app.open_create_channel(id.clone(), None, false, window, cx),
                    Ok("category") => app.open_create_channel(id.clone(), None, true, window, cx),
                    Ok("invite") => app.open_invite(id.clone(), window, cx),
                    _ => {}
                }
                // VECTOR_CHANNEL_EDITOR=overview|permissions opens Channel Settings on VECTOR_CHANNEL, or dev.
                if let Ok(section) = std::env::var("VECTOR_CHANNEL_EDITOR") {
                    let name = std::env::var("VECTOR_CHANNEL").unwrap_or_else(|_| "dev".into());
                    let channel = app.store.borrow().community(&id).and_then(|c| {
                        c.channels.iter().find(|ch| ch.name == name).map(|ch| ch.channel_id.clone())
                    });
                    if let Some(channel) = channel {
                        app.open_channel_editor(id.clone(), channel, false, window, cx);
                        if section == "permissions" {
                            app.on_channel_editor(crate::chaneditor::Act::Section("permissions"), window, cx);
                        }
                    }
                }
                // VECTOR_MODERATION=members|policies|<preset id> opens the console there.
                if let Ok(tab) = std::env::var("VECTOR_MODERATION") {
                    app.open_moderation(id.clone(), window, cx);
                    if tab != "members" {
                        app.on_moderation(
                            crate::moderation::Act::Tab(crate::moderation::Tab::Policies),
                            window,
                            cx,
                        );
                    }
                    if !matches!(tab.as_str(), "members" | "policies") {
                        let act = if tab == "defaults" {
                            crate::policies::Act::Inspect
                        } else {
                            crate::policies::Act::Open(tab)
                        };
                        app.on_moderation(crate::moderation::Act::Policy(act), window, cx);
                    }
                }
                if let Ok(section) = std::env::var("VECTOR_COMMUNITY_SETTINGS") {
                    let section = ["overview", "relays", "roles", "bans", "audit"]
                        .into_iter()
                        .find(|s| *s == section)
                        .unwrap_or("overview");
                    app.open_comm_settings(id.clone(), section, window, cx);
                    // VECTOR_SYNC_REPORT=<any>|epoch-error opens the Sync Report.
                    if std::env::var("VECTOR_SYNC_REPORT").is_ok() {
                        app.load_sync_report(cx);
                    }
                    // VECTOR_ROLE=<name> opens that role in the editor.
                    if let Ok(name) = std::env::var("VECTOR_ROLE") {
                        let role = app.comm_settings.as_ref().and_then(|m| {
                            m.roles
                                .rows()
                                .iter()
                                .find(|r| r["name"].as_str() == Some(name.as_str()))
                                .and_then(|r| r["role_id"].as_str().map(String::from))
                        });
                        app.edit_role(role.or(Some(name)), cx);
                    }
                }
            }
        }
        if !app.live {
            app.load_rail();
            app.load_voice_calls(cx);
        }
        // Fixtures: VECTOR_REKEY=<pct> shows the relay move's progress ring.
        if let Some(pct) = std::env::var("VECTOR_REKEY").ok().and_then(|p| p.parse::<f32>().ok()) {
            let mut r = crate::rekey::Rekey::new("Moving to new relays");
            r.set(pct, Some("Copying history".into()));
            app.rekey = Some(r);
        }
        // Fixtures: VECTOR_UPLOAD_ERR=<raw error> shows its upload failure popup.
        if let Ok(raw) = std::env::var("VECTOR_UPLOAD_ERR") {
            app.upload_failed(&raw.replace("\\n", "\n"), cx);
        }
        // Fixtures: VECTOR_START opens the Main Menu.
        if std::env::var("VECTOR_START").is_ok() {
            app.toggle_start(window, cx);
        }
        // Fixtures: VECTOR_UPDATES=latest|avail shows what a check found.
        match std::env::var("VECTOR_UPDATES").as_deref() {
            Ok("latest") => app.set_updates(|u| u.latest = true),
            Ok("avail") => app.set_updates(|u| {
                u.phase = crate::settings::UpdPhase::Available;
                u.found = Some((
                    "v0.5.2".into(),
                    "- Faster sync\n- Fixed a crash when opening large images".into(),
                ));
            }),
            _ => {}
        }
        // Fixtures: VECTOR_XFER=approve|match opens the transfer card at that step.
        if let Ok(step) = std::env::var("VECTOR_XFER") {
            let send = step != "match";
            let payload = if send {
                serde_json::json!({ "id": 1, "stage": step })
            } else {
                serde_json::json!({ "id": 1, "stage": "match", "sas": "482913", "sender": "npub1ytm2q9jl8kcn4ajqqv2d6xtqy9pyzhml3s7d9dsg7nv8x8ufsjhsn5xzkl", "name": "jskitty" })
            };
            app.security.update(cx, |f, cx| {
                f.open_transfer(send, cx);
                f.transfer_state(&payload, cx);
            });
        }
        // Fixtures: VECTOR_TYPING=<n> has each DM's contact typing, and n members in each group.
        if let Some(n) = std::env::var("VECTOR_TYPING").ok().and_then(|v| v.parse::<usize>().ok()) {
            let mut store = app.store.borrow_mut();
            let now = store.now_ms();
            let me = store.my_npub.clone();
            let mut others: Vec<String> = store.profiles.keys().filter(|k| **k != me).cloned().collect();
            others.sort();
            let typing: Vec<(String, Vec<String>)> = store
                .chats
                .iter()
                .map(|c| {
                    let who = if c.chat_type == vector_core::ChatType::DirectMessage { vec![c.id.clone()] } else { others.iter().take(n).cloned().collect() };
                    (c.id.clone(), who)
                })
                .collect();
            for (id, who) in typing {
                store.typing.insert(id, (who, now));
            }
        }
        // Fixtures: VECTOR_EMBED_VIDEO=idle|loading|failed|ready|preview|grid gives the video embed a file of its
        // own, in that state.
        if let Ok(state) = std::env::var("VECTOR_EMBED_VIDEO") {
            let url = "https://cdn.example.org/vector-tour.mp4".to_string();
            if let Some((_, v)) = app.store.borrow_mut().fixture_embeds.iter_mut().find(|(_, v)| v["class"] == "video") {
                v["link"] = Value::Null;
                v["video"] = serde_json::json!({ "url": url, "is_video": true, "width": 1280, "height": 720, "size": 48_000_000u64, "duration": 83.0, "poster": null, "sha256": null, "fallbacks": [] });
            }
            if state == "grid"
                && let Some((_, v)) = app.store.borrow_mut().fixture_embeds.iter_mut().find(|(_, v)| v["media"].as_array().is_some_and(|m| m.len() > 1))
            {
                v["media"][1] = serde_json::json!({ "url": "https://cdn.example.org/tile.mp4", "is_video": true, "width": 480, "height": 640, "size": 48_000_000u64, "duration": 12.0, "poster": "media/portrait.png", "sha256": null, "fallbacks": [] });
            }
            use crate::embeds::VideoState;
            let clip = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/reference/media/clip.mp4"));
            let mut media = app.media.borrow_mut();
            match state.as_str() {
                "loading" => drop(media.embed_videos.insert(url, VideoState::Loading(42))),
                "failed" => drop(media.embed_videos.insert(url, VideoState::Failed("Media server returned an error status".into()))),
                "ready" => drop(media.embed_videos.insert(url, VideoState::Ready(crate::audio::AudioInputSpec::File(clip)))),
                "preview" => drop(media.embed_previews.insert(url, Some(crate::audio::AudioInputSpec::File(clip)))),
                _ => {}
            }
        }
        // Fixtures: VECTOR_EMBED=post|article|video expands that fixture embed.
        if let Ok(class) = std::env::var("VECTOR_EMBED") {
            let found = {
                let store = app.store.borrow();
                let entity = store
                    .fixture_embeds
                    .iter()
                    .find(|(_, v)| v["class"] == class.as_str())
                    .map(|(k, _)| k.clone());
                entity.map(|e| {
                    let url = store
                        .chats
                        .iter()
                        .flat_map(|c| &c.messages)
                        .flat_map(|m| crate::embeds::refs(&m.content).to_vec())
                        .find(|r| r.entity == e)
                        .and_then(|r| r.url);
                    (e, url)
                })
            };
            if let Some((entity, url)) = found {
                app.push_embed(entity, url, cx);
            }
        }
        // Fixtures: VECTOR_INVITES=1 shows the Requests tab with its invites; =details opens the first.
        if let Ok(v) = std::env::var("VECTOR_INVITES") {
            app.requests_tab = true;
            app.tab_slid = true;
            if v == "details" {
                app.invite_details = app
                    .store
                    .borrow()
                    .invites
                    .first()
                    .map(|i| i.community_id.clone());
            }
        }
        // Fixtures: VECTOR_CREATE_COMMUNITY=1|2|public|private, the step to show.
        if let Ok(step) = std::env::var("VECTOR_CREATE_COMMUNITY") {
            app.open_create_community(window, cx);
            if let Ok(icon) = std::env::var("VECTOR_CC_ICON") {
                app.fixture_new_icon(icon.into(), window, cx);
            }
            if let Some(m) = app.create_community.as_mut() {
                m.name.update(cx, |t, cx| t.set_text("Cool Kids", cx));
                match step.as_str() {
                    "2" => m.step = 2,
                    "public" | "private" => {
                        m.step = 3;
                        m.public = step == "public";
                        m.created = Some(("fixture".into(), "fixture".into()));
                        if m.public {
                            let link = "https://vectorapp.io/c/ncommunity1qvzqqqr4gupzq3jw0e7vhm9a4h2kq6l8fmd3ss8xn2dpz5v7ya0xu3wk6lqcr9eq".to_string();
                            m.link_qr = crate::qr::image(
                                &link,
                                (114. * window.scale_factor()).round() as u32,
                            );
                            m.link = Some(link);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Ok(pick) = std::env::var("VECTOR_NEW_CHAT") {
            app.open_new_chat(window, cx);
            if let Ok(text) = std::env::var("VECTOR_NEW_CHAT_TEXT") {
                if let Some(key) = crate::newchat::invite_key(&text) {
                    app.resolve_invite(key, cx);
                }
                app.new_chat
                    .as_ref()
                    .unwrap()
                    .filter
                    .update(cx, |t, cx| t.set_text(&text, cx));
            }
            if let Ok(n) = pick.parse::<usize>()
                && n > 0
            {
                let first = {
                    let nc = app.new_chat.as_ref().unwrap();
                    let store = app.store.borrow();
                    crate::newchat::rows_for(nc, &store).get(n - 1).cloned()
                };
                if let Some(npub) = first {
                    app.on_new_chat(crate::newchat::Act::Toggle(npub), window, cx);
                }
            }
        }
        if let Ok(mode) = std::env::var("VECTOR_SWITCHER") {
            app.on_profile(crate::profile::Act::Switcher, window, cx);
            if mode == "edit" {
                app.on_profile(crate::profile::Act::SwitcherEdit, window, cx);
            }
        }
        if let Ok(which) = std::env::var("VECTOR_PACK_CREATOR") {
            app.picker_closed = std::time::Instant::now() - std::time::Duration::from_secs(1);
            app.toggle_picker(None, window, cx);
            let id = (which == "edit")
                .then(|| {
                    app.store
                        .borrow()
                        .fixture_packs
                        .values()
                        .find(|p| p.is_own)
                        .map(|p| p.id.clone())
                })
                .flatten();
            app.emoji_picker
                .update(cx, |p, cx| p.open_creator(id, window, cx));
            if let Ok(files) = std::env::var("VECTOR_PACK_ADD") {
                app.emoji_picker.update(cx, |p, cx| {
                    p.add_paths(files.split(':').map(Into::into).collect(), window, cx)
                });
            }
        }
        if let Ok(url) = std::env::var("VECTOR_LINK") {
            app.deep_link(&url, window, cx);
        }
        if let Ok(naddr) = std::env::var("VECTOR_PACK_DETAILS") {
            app.open_pack_details(naddr, cx);
        }
        if std::env::var("VECTOR_STATUS").is_ok() {
            app.open_status(window, cx);
        }
        if let Ok(draft) = std::env::var("VECTOR_DRAFT") {
            app.input.update(cx, |t, cx| t.set_text(&draft, cx));
            app.update_commands(&draft, cx);
        }
        if let Ok(files) = std::env::var("VECTOR_SEND") {
            app.open_paths_preview(files.split(':').map(Into::into).collect(), window, cx);
        }
        if let Ok(file) = std::env::var("VECTOR_CROP")
            && app.profile.is_some()
            && {
                app.on_profile(crate::profile::Act::Edit, window, cx);
                true
            }
            && let Some((image, w, h)) = std::fs::read(&file)
                .ok()
                .and_then(|b| crate::media::decode(&b, "png", (2048, 2048)))
        {
            let cropper = cx.new(|cx| {
                crate::cropper::Cropper::new(
                    Arc::new(image),
                    (w, h),
                    "Crop Profile Picture",
                    "Drag to move, or drag a corner to resize.",
                    true,
                    window,
                    cx,
                )
            });
            cx.subscribe_in(
                &cropper,
                window,
                |this, _, answer: &crate::cropper::Answer, window, cx| {
                    this.on_crop(answer, window, cx)
                },
            )
            .detach();
            app.cropper = Some((cropper, file.into()));
        }
        app
    }

    /// A live account: login or unlock, then the app as its events arrive.
    pub fn live(
        mut events: UnboundedReceiver<UiEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut app = Self::base(Store::live(), true, window, cx);
        app.listen_models(cx);
        app.listen_video(cx);
        crate::callvideo::announce_caps();
        app.screen = Screen::Booting;
        cx.spawn_in(window, async move |this, cx| {
            while let Some(ev) = events.next().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_event(ev, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        app.probe(window, cx);
        cx.spawn(async move |this, cx| {
            loop {
                if this
                    .update(cx, |this, cx| this.check_updates(true, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(4 * 60 * 60))
                    .await;
            }
        })
        .detach();
        app
    }

    fn toggle_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.start.take().is_some() {
            cx.notify();
            return;
        }
        let search = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                size: px(15.),
                line_height: px(18.),
                weight: FontWeight::NORMAL,
                placeholder: white_a(0.5),
                ..kit::field_look()
            })
        });
        cx.subscribe(&search, |app, _, _: &crate::composer::InputChanged, cx| {
            if let Some(sm) = app.start.as_mut() {
                sm.selected = 0;
            }
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&search, window, |app, _, _: &SendText, window, cx| {
            let ix = app.start.as_ref().map(|s| s.selected).unwrap_or(0);
            app.on_start(crate::startmenu::Act::Pick(ix), window, cx);
        })
        .detach();
        cx.subscribe_in(&search, window, |app, _, _: &CancelInput, window, cx| {
            app.start_escape(window, cx)
        })
        .detach();
        let (accounts, at_cap) = if self.live {
            let cap = account::account_cap();
            let rows: Vec<_> = account::accounts()
                .into_iter()
                .map(|a| crate::switcher::Row {
                    npub: a.npub,
                    name: a.name,
                    avatar: a.avatar.map(Into::into),
                })
                .collect();
            let full = rows.len() >= cap;
            (rows, full)
        } else {
            (self.fixture_accounts(), false)
        };
        let (layout, mic) = if self.live {
            (
                vector_core::synced_prefs::load_start(),
                vector_core::calls::settings::mic_state(),
            )
        } else {
            // Fixtures: VECTOR_START_PINS=key,f:Name=key|key stands in for the synced layout.
            use vector_core::rail_layout::RailNode;
            let nodes = std::env::var("VECTOR_START_PINS")
                .unwrap_or_default()
                .split(',')
                .filter(|k| !k.is_empty())
                .map(
                    |k| match k.strip_prefix("f:").and_then(|f| f.split_once('=')) {
                        Some((name, keys)) => RailNode::Folder {
                            id: name.into(),
                            name: name.into(),
                            hue: None,
                            keys: keys.split('|').map(String::from).collect(),
                        },
                        None => RailNode::item(k),
                    },
                )
                .collect();
            (
                vector_core::rail_layout::RailLayout {
                    nodes,
                    ..Default::default()
                },
                Default::default(),
            )
        };
        window.focus(&search.focus_handle(cx), cx);
        self.start = Some(crate::startmenu::StartMenu {
            layout,
            list: backend::device::get("start_view").as_deref() == Some("list"),
            search,
            folder: None,
            mic,
            accounts,
            at_cap,
            opened: std::time::Instant::now(),
            adding: false,
            picking: false,
            selected: 0,
        });
        cx.notify();
    }

    fn on_menu(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((at, items, mut acts)) = self.menu.take() else {
            return;
        };
        cx.notify();
        if ix >= acts.len() {
            return;
        }
        match acts.swap_remove(ix) {
            MenuAct::Nothing => {
                acts.insert(ix.min(acts.len()), MenuAct::Nothing);
                self.menu = Some((at, items, acts));
            }
            MenuAct::StartDetails(id) => {
                self.start = None;
                self.nexus_show_app(id, window, cx);
            }
            MenuAct::StartCopyLink(link) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(link));
                self.show_toast("Copied Link", cx);
            }
            MenuAct::NewMessage => self.open_new_chat(window, cx),
            MenuAct::AddFriend => {
                let me = self.store.borrow().my_npub.clone();
                self.show_profile_qr(&me, window, cx);
            }
            MenuAct::Bookmark => {
                let me = self.store.borrow().my_npub.clone();
                self.settings = None;
                self.store.borrow_mut().ensure_dm(&me);
                self.open_chat(me.into(), window, cx);
            }
            MenuAct::AppStore => self.show_nexus(None, window, cx),
            MenuAct::FoldRail => self.slim_set_rail(false, cx),
            MenuAct::NewCommunity => self.open_create_community(window, cx),
            MenuAct::Sub(label, sub_items, sub_acts) => {
                acts.insert(ix.min(acts.len()), MenuAct::Nothing);
                self.menu_trail.push((items, acts));
                let mut items = vec![kit::MenuItem::back(label)];
                items.extend(sub_items);
                let mut acts = vec![MenuAct::Back];
                acts.extend(sub_acts);
                self.menu = Some((at, items, acts));
            }
            MenuAct::Back => {
                if let Some((items, acts)) = self.menu_trail.pop() {
                    self.menu = Some((at, items, acts));
                }
            }
            MenuAct::SelfDestruct(chat) => self.open_self_destruct(chat, at),
            MenuAct::Msg(id, act) => self.message_menu_act(id, act, window, cx),
            MenuAct::ChangeWallpaper(chat) => self.pick_wallpaper(chat, cx),
            MenuAct::RemoveWallpaper(chat) => {
                let mut popup = kit::Popup::notice("Remove wallpaper?");
                popup.text = Some("This will remove the wallpaper and revert to the default theme. This change syncs to your contact and your other devices.".into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::RemoveWallpaper(chat)));
            }
            MenuAct::SetSelfDestruct(chat, secs) => {
                if self.live && vector_core::self_destruct::set_chat_duration_secs(&chat, secs).is_err() {
                    return;
                }
                if self.open.as_deref() == Some(chat.as_str()) {
                    self.self_destruct = secs;
                }
            }
            MenuAct::CommunityRead(id) => {
                if self.live {
                    let rx = backend::run(backend::community::mark_read(id));
                    cx.spawn(async move |this, cx| {
                        let _ = rx.await;
                        this.update(cx, |this, cx| this.refresh_unread(cx)).ok();
                    })
                    .detach();
                }
            }
            MenuAct::ChatRead(id) => self.chat_mark_read(id, cx),
            MenuAct::ReadAllDms => {
                for id in self.unread_dms() {
                    self.chat_mark_read(id, cx);
                }
            }
            MenuAct::ChatUnread(id) => self.chat_mark_unread(id, cx),
            MenuAct::ChatPin(id, pin) => self.chat_pin(id, pin, cx),
            MenuAct::ChatArchive(id, archive) => self.chat_archive(id, archive, cx),
            MenuAct::ChatBlock(npub) => {
                if self.blocked_by_sync(cx) {
                    return;
                }
                let mut popup = kit::Popup::notice("Block User");
                popup.text = Some("Are you sure you want to block this user? You will no longer receive DMs from them.".into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::Block { npub }));
            }
            MenuAct::ChannelRead(id) => {
                if self.live {
                    let rx = backend::run(messaging::mark_as_read(id, None));
                    cx.spawn(async move |this, cx| {
                        let _ = rx.await;
                        this.update(cx, |this, cx| this.refresh_unread(cx)).ok();
                    })
                    .detach();
                }
            }
            MenuAct::CreateChannel {
                community,
                category,
            } => self.open_create_channel(community, category, false, window, cx),
            MenuAct::CreateCategory(community) => {
                self.open_create_channel(community, None, true, window, cx)
            }
            MenuAct::InvitePeople(community) => self.open_invite(community, window, cx),
            MenuAct::CommunitySettings(community) => {
                self.open_comm_settings(community, "overview", window, cx)
            }
            MenuAct::BannerHidden(community, hidden) => {
                self.set_banner_hidden(community, hidden, cx)
            }
            MenuAct::CommunityNickname(community) => {
                let me = self.store.borrow().my_npub.clone();
                self.ask_community_nickname(community, me, window, cx);
            }
            MenuAct::Moderation(community) => self.open_moderation(community, window, cx),
            MenuAct::LeaveCommunity { id, name, owner } => {
                self.ask_leave_community(id, name, owner, window, cx)
            }
            MenuAct::PackShare(id) => {
                self.on_pack(crate::packs::Act::Copy(id), cx);
                self.toast = Some(("Copied to Clipboard".into(), std::time::Instant::now()));
                self.close_picker(window, cx);
            }
            MenuAct::PackRemove(id) => self.remove_pack(id, cx),
            MenuAct::PackEdit(id) => self
                .emoji_picker
                .update(cx, |p, cx| p.open_creator(Some(id), window, cx)),
            MenuAct::CreatorRename(idx) => self
                .emoji_picker
                .update(cx, |p, cx| p.creator_rename(idx, window, cx)),
            MenuAct::CreatorDelete(idx) => self
                .emoji_picker
                .update(cx, |p, cx| p.creator_remove(idx, cx)),
            MenuAct::EditChannel { community, channel, category } => {
                self.open_channel_editor(community, channel, category, window, cx)
            }
            MenuAct::DeleteChannel { community, channel, category } => {
                self.ask_delete_channel(community, channel, category, cx)
            }
            MenuAct::CopyId(id, what) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(id));
                self.toast = Some((format!("Copied {what} ID").into(), std::time::Instant::now()));
            }
            MenuAct::NexusLink(id) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!("https://vectorapp.io/go#nexus/{id}")));
                self.toast = Some(("Copied App Link".into(), std::time::Instant::now()));
            }
            MenuAct::Copy(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.toast = Some(("Copied".into(), std::time::Instant::now()));
            }
            MenuAct::NotifyLevel(scope, level) => {
                let rx = backend::run(backend::prefs::set_notify_level(scope, level));
                self.report(rx, "Notification Settings", cx);
            }
            MenuAct::NotifyMute(scope, ms) => {
                let rx = backend::run(backend::prefs::set_notify_mute(scope, ms));
                self.report(rx, "Mute", cx);
            }
            MenuAct::SuppressEveryone(community, suppress) => {
                let rx = backend::run(backend::prefs::set_suppress_everyone(
                    community,
                    Some(suppress),
                ));
                self.report(rx, "Notification Settings", cx);
            }
            MenuAct::StartFolder(id) => {
                if let Some(sm) = self.start.as_mut() {
                    sm.folder = Some(id);
                }
            }
            MenuAct::StartRename(id) => {
                let current = self.start.as_ref().and_then(|sm| {
                    sm.layout.nodes.iter().find_map(|n| match n {
                        vector_core::rail_layout::RailNode::Folder { id: f, name, .. }
                            if *f == id =>
                        {
                            Some(name.clone())
                        }
                        _ => None,
                    })
                });
                let input = cx.new(|cx| {
                    TextInput::new("Folder", cx)
                        .with_look(kit::field_look())
                        .keep_on_submit()
                        .submit_empty()
                });
                if let Some(name) = current {
                    input.update(cx, |i, cx| i.set_text(&name, cx));
                }
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| {
                    app.answer_popup(true, window, cx)
                })
                .detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
                    app.answer_popup(false, window, cx)
                })
                .detach();
                window.focus(&input.focus_handle(cx), cx);
                let mut popup = kit::Popup::notice("Rename Folder");
                popup.icon = None;
                popup.confirm = "Rename".into();
                popup.cancel = Some("Cancel".into());
                popup.input = Some(input);
                self.popup = Some((popup, PopupAct::RenameFolder { id }));
            }
            MenuAct::StartUngroup(id) => self.start_edit(move |l| l.dissolve_folder(&id), cx),
            MenuAct::StartMoveOut(key) => self.start_edit(
                move |l| {
                    use vector_core::rail_layout::{RailDrag, RailDrop};
                    l.apply_drop(
                        &RailDrag::Item { key: key.clone() },
                        &RailDrop::End,
                        &vector_core::rail_layout::new_folder_id(),
                    )
                },
                cx,
            ),
            MenuAct::StartTogglePin(key) => self.start_pin(key, cx),
            act @ (MenuAct::MemberNickname(..) | MenuAct::MemberMini(..) | MenuAct::MemberTimeout(..) | MenuAct::MemberRemove(..)) => {
                self.member_menu_act(act, window, cx);
            }
            act @ (MenuAct::VoiceVolume(..) | MenuAct::VoiceModerate { .. } | MenuAct::ViewProfile(_)) => {
                self.voice_menu_act(act, cx);
            }
            MenuAct::MemberRole { community, npub, role, on } => {
                acts.insert(ix.min(acts.len()), MenuAct::Nothing);
                self.menu = Some((at, items, acts));
                self.member_role(community, npub, role, on, cx);
            }
            MenuAct::VoiceSlide(npub) => {
                acts.insert(ix.min(acts.len()), MenuAct::VoiceSlide(npub));
                self.menu = Some((at, items, acts));
            }
            act @ (MenuAct::RailRead(_) | MenuAct::RailRename(_) | MenuAct::RailHue(..) | MenuAct::RailUngroup(_)) => {
                self.rail_menu_act(act, window, cx)
            }
            MenuAct::StartUnpin(key) => self.start_edit(
                move |l| {
                    vector_core::start_menu::unpin(l, &key);
                    Ok(())
                },
                cx,
            ),
            MenuAct::Device(kind, name, mut prefs) => {
                if kind == "input" {
                    prefs.input = name;
                } else {
                    prefs.output = name;
                }
                if self.live {
                    let rx = backend::run(async move {
                        vector_core::db::scoped(async move {
                            tokio::task::spawn_blocking(move || backend::mic::set_devices(prefs))
                                .await
                                .map_err(|e| e.to_string())?
                        })
                        .await
                    });
                    self.toast_err(rx, cx);
                }
            }
        }
    }

    /// One edit to the pins, through the synced layout once its relay copy is read.
    fn start_edit(
        &mut self,
        f: impl FnOnce(&mut vector_core::rail_layout::RailLayout) -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if !self.live {
            if let Some(sm) = self.start.as_mut() {
                if let Err(e) = f(&mut sm.layout) {
                    self.show_toast(&e, cx);
                }
            }
            cx.notify();
            return;
        }
        let rx = backend::run(async move {
            vector_core::db::scoped(async move {
                use vector_core::synced_prefs::{self, Pref};
                if !synced_prefs::is_hydrated(Pref::Start) {
                    return Err("Still syncing your pins, try again in a moment".to_string());
                }
                synced_prefs::edit_layout(Pref::Start, f).map(|(layout, _)| layout)
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update(cx, |this, cx| {
                match result {
                    Ok(layout) => {
                        if let Some(sm) = this.start.as_mut() {
                            sm.layout = layout;
                        }
                    }
                    Err(e) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Escape steps back one layer: the query, Add Pin, Accounts, a folder, then the menu.
    fn start_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sm) = self.start.as_mut() else {
            return;
        };
        if !sm.search.read(cx).text().is_empty() {
            let search = sm.search.clone();
            search.update(cx, |i, cx| i.clear(window, cx));
        } else if sm.adding || sm.picking {
            sm.adding = false;
            sm.picking = false;
        } else if sm.folder.is_some() {
            sm.folder = None;
        } else {
            self.start = None;
        }
        cx.notify();
    }

    /// Whether `key` is pinned, from the open menu or else the synced layout.
    pub(super) fn start_pinned(&self, key: &str) -> bool {
        match &self.start {
            Some(sm) => sm.pinned(key),
            None if self.live => crate::startmenu::layout_pins(&vector_core::synced_prefs::load_start(), key),
            None => false,
        }
    }

    fn start_pin(&mut self, key: String, cx: &mut Context<Self>) {
        let pinned = self.start_pinned(&key);
        self.start_edit(
            move |l| {
                if pinned {
                    vector_core::start_menu::unpin(l, &key);
                    Ok(())
                } else {
                    vector_core::start_menu::pin(l, &key).map(drop)
                }
            },
            cx,
        );
    }

    fn on_start(
        &mut self,
        act: crate::startmenu::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::startmenu::Act;
        let Some(sm) = self.start.as_mut() else {
            return;
        };
        cx.notify();
        match act {
            Act::Close => self.start = None,
            Act::View(list) => {
                sm.list = list;
                backend::device::set("start_view", if list { "list" } else { "grid" });
            }
            Act::Folder(id) => sm.folder = id,
            Act::Mute | Act::Deafen => {
                let m = sm.mic;
                sm.mic = match act {
                    // Unmuting while deafened hears again too, as in a call.
                    Act::Mute if m.deafened => vector_core::calls::settings::MicState {
                        muted: false,
                        deafened: false,
                    },
                    Act::Mute => vector_core::calls::settings::MicState {
                        muted: !m.muted,
                        deafened: false,
                    },
                    _ => vector_core::calls::settings::MicState {
                        muted: m.muted,
                        deafened: !m.deafened,
                    },
                };
                if self.live {
                    let next = sm.mic;
                    let _ = backend::run(async move {
                        vector_core::db::scoped(async move {
                            vector_core::calls::settings::remember_mic(next)
                        })
                        .await
                    });
                }
            }
            Act::Devices(kind, at) => {
                let devices = if self.live {
                    backend::mic::devices()
                } else {
                    backend::mic::DeviceList {
                        inputs: vec!["Built-in Microphone".into(), "USB Headset".into()],
                        outputs: vec!["Built-in Speakers".into(), "USB Headset".into()],
                        default_input: "Built-in Microphone".into(),
                        default_output: "Built-in Speakers".into(),
                        prefs: Default::default(),
                    }
                };
                let (names, fallback, current) = if kind == "input" {
                    (
                        devices.inputs,
                        devices.default_input,
                        devices.prefs.input.clone(),
                    )
                } else {
                    (
                        devices.outputs,
                        devices.default_output,
                        devices.prefs.output.clone(),
                    )
                };
                let mut items = vec![kit::MenuItem {
                    label: if fallback.is_empty() {
                        "System Default".into()
                    } else {
                        format!("System Default ({fallback})").into()
                    },
                    icon: None,
                    checked: current.is_none(),
                    ..Default::default()
                }];
                let mut acts = vec![MenuAct::Device(kind, None, devices.prefs.clone())];
                for name in names {
                    items.push(kit::MenuItem {
                        label: name.clone().into(),
                        icon: None,
                        checked: current.as_ref() == Some(&name),
                        ..Default::default()
                    });
                    acts.push(MenuAct::Device(kind, Some(name), devices.prefs.clone()));
                }
                self.menu = Some((at, items, acts));
            }
            Act::Menu(at, key, folder) => {
                let in_folder = sm.folder.clone();
                let (items, acts): (Vec<_>, Vec<_>) = match (key, folder) {
                    (_, Some(id)) => vec![
                        (
                            kit::MenuItem::new("Open", "folder"),
                            MenuAct::StartFolder(id.clone()),
                        ),
                        (
                            kit::MenuItem::new("Rename", "edit"),
                            MenuAct::StartRename(id.clone()),
                        ),
                        (
                            kit::MenuItem::new("Ungroup", "grid-fill"),
                            MenuAct::StartUngroup(id),
                        ),
                    ]
                    .into_iter()
                    .unzip(),
                    (Some(key), None) => {
                        let mut rows = Vec::new();
                        if let Some(app) = key.strip_prefix("app/") {
                            rows.push((kit::MenuItem::new("View Details", "info"), MenuAct::StartDetails(app.to_string())));
                        }
                        if in_folder.is_some() {
                            rows.push((
                                kit::MenuItem::new("Move out of Folder", "chevron-left"),
                                MenuAct::StartMoveOut(key.clone()),
                            ));
                        }
                        if let Some(link) = start_key_link(&key) {
                            rows.push((kit::MenuItem::new("Copy Link", "share"), MenuAct::StartCopyLink(link)));
                        }
                        rows.push((
                            kit::MenuItem::new("Unpin", "pin").danger(),
                            MenuAct::StartUnpin(key),
                        ));
                        rows.into_iter().unzip()
                    }
                    _ => return,
                };
                self.menu = Some((at, items, acts));
            }
            Act::AddPin => {
                sm.adding = true;
                sm.selected = 0;
            }
            Act::Back => {
                sm.adding = false;
                sm.picking = false;
                sm.selected = 0;
                let search = sm.search.clone();
                search.update(cx, |i, cx| i.clear(window, cx));
            }
            Act::Hover(ix) => sm.selected = ix,
            Act::Step(d) => {
                let query = sm.search.read(cx).text().to_string();
                if query.trim().is_empty() && !sm.adding {
                    return;
                }
                let n = crate::startmenu::hits(sm, &self.store.borrow(), &query).len();
                if n > 0 {
                    sm.selected = (sm.selected as i32 + d).rem_euclid(n as i32) as usize;
                }
            }
            Act::Pick(ix) => {
                let query = sm.search.read(cx).text().to_string();
                if query.trim().is_empty() && !sm.adding {
                    return;
                }
                let hits = crate::startmenu::hits(sm, &self.store.borrow(), &query);
                let Some(hit) = hits.into_iter().nth(ix) else {
                    return;
                };
                match (sm.adding, hit.key, hit.nav) {
                    (true, Some(key), _) => self.start_pin(key, cx),
                    (_, Some(key), _)
                        if key.starts_with("dm/")
                            && sm.search.read(cx).text().trim().starts_with("npub1") =>
                    {
                        self.start = None;
                        self.open_profile(key[3..].to_string(), cx);
                    }
                    (_, Some(key), _) => self.on_start(Act::Open(key), window, cx),
                    (_, None, Some(nav)) => self.on_start(Act::Nav(nav), window, cx),
                    _ => {}
                }
            }
            Act::Pin(key) => self.start_pin(key, cx),
            Act::Drop(source, target) => self.start_edit(
                move |l| l.apply_drop(&source, &target, &vector_core::rail_layout::new_folder_id()),
                cx,
            ),
            Act::Switch(npub) => {
                self.start = None;
                self.on_switcher(crate::switcher::Act::Pick(npub), window, cx);
            }
            Act::AddAccount => {
                self.start = None;
                self.on_switcher(crate::switcher::Act::Add, window, cx);
            }
            Act::Nav("accounts") => {
                if sm.accounts.len() > 1 {
                    sm.picking = true;
                    sm.adding = false;
                    let search = sm.search.clone();
                    search.update(cx, |i, cx| i.clear(window, cx));
                } else {
                    self.start = None;
                    self.on_switcher(crate::switcher::Act::Add, window, cx);
                }
            }
            Act::Nav(id) => {
                self.start = None;
                let me = self.store.borrow().my_npub.clone();
                match id {
                    "profile" => self.open_profile(me, cx),
                    "chat" => {
                        self.settings = None;
                        self.profile = None;
                    }
                    "bookmarks" => {
                        self.settings = None;
                        self.store.borrow_mut().ensure_dm(&me);
                        self.open_chat(me.into(), window, cx);
                    }
                    "settings" => self.open_settings("appearance", window, cx),
                    "miniapps" => self.show_nexus(None, window, cx),
                    _ => {}
                }
            }
            Act::Open(key) => {
                self.start = None;
                let parts: Vec<&str> = key.split('/').collect();
                match parts.as_slice() {
                    ["dm", npub] => {
                        self.settings = None;
                        self.store.borrow_mut().ensure_dm(npub);
                        self.open_chat(npub.to_string().into(), window, cx);
                    }
                    ["settings", rest @ ..] => {
                        let cats = crate::settings::categories(&crate::settings::Offers {
                            voice: self.live || std::env::var_os("VECTOR_TRANSCRIPT").is_some(),
                            routing: true,
                            updates: true,
                        });
                        let cat = rest
                            .first()
                            .and_then(|id| cats.iter().find(|c| c.id == *id))
                            .map(|c| c.id)
                            .unwrap_or("appearance");
                        self.open_settings(cat, window, cx);
                    }
                    ["requests"] => {
                        self.settings = None;
                        self.profile = None;
                        self.requests_tab = true;
                    }
                    ["nexus"] => self.show_nexus(None, window, cx),
                    ["app", id] => self.open_app_pin(id.to_string(), window, cx),
                    ["c", id, rest @ ..] if self.store.borrow().community(id).is_some() => {
                        self.open_community(id.to_string(), window, cx);
                        if let Some(ch) = rest.first() {
                            self.open_channel(ch.to_string(), window, cx);
                        }
                    }
                    ["c", ..] => self.show_toast("You're not in this community", cx),
                    _ => self.show_toast("This pin opens something not in this build yet", cx),
                }
            }
        }
    }

    fn rail_mode(&self, window: &Window) -> RailMode {
        let locked = f32::from(window.viewport_size().width) < RAIL_AUTO_COLLAPSE_W;
        RailMode {
            collapsed: self.rail_pref || locked,
            locked,
        }
    }

    fn set_updates(&mut self, f: impl FnOnce(&mut crate::settings::Updates)) {
        f(&mut self.updates);
        if let Some(s) = self.settings.as_mut() {
            s.updates = self.updates.clone();
        }
    }

    /// A silent check only ever announces a find; a manual one says how it went.
    fn check_updates(&mut self, silent: bool, cx: &mut Context<Self>) {
        use crate::settings::UpdPhase;
        if self.updates.phase == UpdPhase::Checking {
            return;
        }
        if !silent {
            self.set_updates(|u| {
                u.phase = UpdPhase::Checking;
                u.latest = false;
            });
            cx.notify();
        }
        let beta = self.updates.beta;
        let rx = backend::run(async move { backend::updates::check(beta).await });
        cx.spawn(async move |this, cx| {
            let result = rx
                .await
                .unwrap_or_else(|_| Err("Failed to check for updates".into()));
            let settled = this
                .update(cx, |this, cx| {
                    cx.notify();
                    if this.updates.beta != beta {
                        return None;
                    }
                    match result {
                        Ok(Some(release)) => {
                            this.update_url = Some(backend::updates::release_page(release.version));
                            this.set_updates(|u| {
                                u.phase = UpdPhase::Available;
                                u.latest = false;
                                u.found = Some((release.version.display(), release.notes));
                            });
                            None
                        }
                        Ok(None) if !silent => {
                            this.set_updates(|u| {
                                u.phase = UpdPhase::NoUpdates;
                                u.latest = true;
                            });
                            Some((UpdPhase::NoUpdates, 3000))
                        }
                        Err(e) if !silent => {
                            this.set_updates(|u| u.phase = UpdPhase::Error(e.clone()));
                            Some((UpdPhase::Error(e), 5000))
                        }
                        _ => None,
                    }
                })
                .ok()
                .flatten();
            let Some((phase, ms)) = settled else { return };
            cx.background_executor()
                .timer(std::time::Duration::from_millis(ms))
                .await;
            this.update(cx, |this, cx| {
                if this.updates.phase == phase {
                    this.set_updates(|u| u.phase = UpdPhase::Idle);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Find the account on disk and open it, or ask how to.
    fn probe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let probe = backend::run(async { account::boot() });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(info) = probe.await else { return };
            this.update_in(cx, |this, window, cx| {
                if let Some(block) = &info.downgrade {
                    this.downgrade = Some(crate::downgrade::versions(block));
                    return cx.notify();
                }
                match (&info.account, info.encrypted) {
                    (Some(_), false) => this.unlock_plain(window, cx),
                    (Some(_), true) => {
                        this.show_login(Some(info.security_type.as_str()), window, cx)
                    }
                    (None, _) => this.show_login(None, window, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Start over on whichever account boot now finds, as a fresh launch would.
    fn reboot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut fresh = Self::base(Store::live(), true, window, cx);
        fresh.screen = Screen::Booting;
        *self = fresh;
        self.probe(window, cx);
        cx.notify();
    }

    fn show_login(&mut self, unlock: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| LoginView::new(unlock, window, cx));
        self.attach_login(view.clone(), cx, window);
        if !self.live {
            let rows = self.fixture_accounts();
            let me = self.store.borrow().my_npub.clone();
            let open = std::env::var("VECTOR_LOGIN").is_ok_and(|m| m == "picker");
            view.update(cx, |v, cx| v.set_accounts(rows, Some(me), open, cx));
            return;
        }
        let rx = backend::run(async {
            (
                account::accounts(),
                vector_core::db::read_active_account_file().ok().flatten(),
            )
        });
        cx.spawn(async move |_, cx| {
            let Ok((accounts, active)) = rx.await else {
                return;
            };
            let rows = accounts
                .into_iter()
                .map(|a| crate::switcher::Row {
                    npub: a.npub,
                    name: a.name,
                    avatar: a.avatar.map(Into::into),
                })
                .collect();
            view.update(cx, |v, cx| v.set_accounts(rows, active, false, cx));
        })
        .detach();
    }

    fn attach_login(
        &mut self,
        view: Entity<LoginView>,
        cx: &mut Context<Self>,
        window: &mut Window,
    ) {
        cx.subscribe_in(&view, window, |this, _, e: &LoggedIn, window, cx| {
            // A profile added over another account starts on a clean slate.
            if this.adding.take().is_some() {
                let mut fresh = Self::base(Store::live(), true, window, cx);
                fresh.screen = Screen::Booting;
                *this = fresh;
            }
            this.logged_in(e.npub.clone(), window, cx)
        })
        .detach();
        cx.subscribe_in(
            &view,
            window,
            |this, _, e: &crate::login::AddProfile, window, cx| this.on_add_profile(e, window, cx),
        )
        .detach();
        cx.subscribe(&view, |this, _, _: &crate::login::MethodInfo, cx| {
            let mut p = kit::Popup::notice("Ways to Sign In");
            p.icon = None;
            p.text = Some(
                "<b>QR Code</b><br>Use a device you're already signed in on. There, open Settings and choose Sign in on Another Device, then scan the code this device shows or type it in. Both devices confirm a number, so your keys pass between them without anyone else being able to read them.\
                 <br><br><b>Private Key</b><br>Paste your nsec or type your seed phrase. Good when you have your key backed up and no other device to hand.\
                 <br><br><b>Remote Signer</b><br>Keep your key in a signer app such as Amber or nsec.app, which approves what Vector signs. Paste its bunker:// link, or pair it by scanning the code Vector shows."
                    .into(),
            );
            this.popup = Some((p, PopupAct::Notice));
            cx.notify();
        })
        .detach();
        cx.subscribe(&view, |this, _, _: &crate::login::OpenTransfer, cx| {
            this.security.update(cx, |f, cx| f.open_transfer(false, cx))
        })
        .detach();
        cx.subscribe_in(
            &view,
            window,
            |this, _, crate::login::PickAccount(npub), window, cx| {
                if !this.live {
                    return;
                }
                let rx = backend::run(account::switch(npub.clone()));
                cx.spawn_in(window, async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
                    this.update_in(cx, |this, window, cx| match result {
                        Ok(()) => this.reboot(window, cx),
                        Err(e) => log::error!("account switch failed: {e}"),
                    })
                    .ok();
                })
                .detach();
            },
        )
        .detach();
        let fixture = (!self.live).then(|| self.store.borrow().fixture_net.as_ref().and_then(|n| n.get("view").cloned())).flatten();
        let live = self.live;
        view.update(cx, |v, cx| v.net_setup(live, fixture, cx));
        self.screen = Screen::Login(view);
        cx.notify();
    }

    /// The signer form over the app, re-pairing this account's signer in place.
    fn open_reauth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| LoginView::new(None, window, cx));
        cx.subscribe_in(&view, window, |this, _, _: &ReauthClosed, _, cx| {
            this.reauth = None;
            if let Some(s) = this.settings.as_mut()
                && let Some((_, dot)) = s.signer.as_mut()
            {
                *dot = vector_core::bunker_state().as_label();
            }
            cx.notify();
        })
        .detach();
        view.update(cx, |v, cx| v.start_reauth(false, window, cx));
        self.reauth = Some(view);
        cx.notify();
    }

    /// The signer asks for a page to approve on. Only http(s), and off Clearnet only once
    /// confirmed, since the browser loads it outside the account's network.
    /// A link card's video or track, in the player window.
    fn play_link(&mut self, msg: String, p: crate::linkcard::Play, cx: &mut Context<Self>) {
        let audio = p.provider == "soundcloud";
        let title = p.title.clone();
        let rx = backend::run(async move {
            vector_media::players::player_url(p.provider.to_string(), p.id, (p.start > 0).then_some(p.start)).await
        });
        cx.spawn(async move |this, cx| {
            let Ok(got) = rx.await else { return };
            this.update(cx, |this, cx| match got.and_then(|url| crate::miniapp::play(msg, title, url, audio)) {
                Ok(()) => {}
                Err(e) => this.show_toast(&e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn signer_asks_to_open(&mut self, url: &str, cx: &mut Context<Self>) {
        let Ok(parsed) = url::Url::parse(url) else { return };
        if !matches!(parsed.scheme(), "http" | "https") {
            return;
        }
        let target = parsed.to_string();
        let off_clearnet = vector_core::transport::preference().is_some_and(|k| k != vector_core::transport::Kind::Clearnet);
        if !off_clearnet {
            return cx.open_url(&target);
        }
        let mut popup = kit::Popup::notice("Approve in your signer");
        popup.icon = None;
        popup.text = Some(format!("Your signer asked you to open this page. It opens in your browser, outside your private network.<br><code>{}</code>", target.replace('&', "&amp;").replace('<', "&lt;")).into());
        popup.confirm = "Open".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::OpenWeb(target)));
        cx.notify();
    }

    /// An account kept without encryption opens straight away.
    fn unlock_plain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = backend::run(async {
            let npub = account::unlock(None).await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(npub) => this.logged_in(npub, window, cx),
                Err(e) => {
                    log::error!("unlock failed: {e}");
                    this.show_login(None, window, cx);
                    if let Screen::Login(view) = &this.screen {
                        view.clone().update(cx, |v, cx| v.offer_reauth(&e, cx));
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn logged_in(&mut self, npub: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.store.borrow_mut().my_npub = npub;
        self.screen = Screen::Main;
        if let Some(v) = self.net_initial() {
            self.on_transport_view(v, cx);
        }
        self.syncing = true;
        let _ = backend::run(async {
            sync::fetch_messages(true).await;
            sync::notifs();
            backend::selfsync::start();
        });
        let rx = backend::run(async {
            let b = backend::prefs::sql_bools(&[
                ("web_previews", true),
                ("emoticon_suggestions", true),
                ("time_suggestions", true),
                ("chat_bg_enabled", true),
                ("floating_player", true),
            ]);
            (
                backend::storage::auto_download(),
                b[0],
                (b[1], b[2], b[3], b[4]),
                (backend::actions::emoji_usage(), backend::prefs::synced_settings()),
            )
        });
        cx.spawn(async move |this, cx| {
            let Ok(((on, limit), previews, (emoticons, times, chat_bg, floating), (used, synced))) = rx.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let mut media = this.media.borrow_mut();
                media.auto_download = crate::media::AutoDownload { on, limit };
                media.web_previews_off = !previews;
                drop(media);
                this.emoticons = emoticons;
                this.time_suggestions = times;
                this.chat_bg = chat_bg;
                this.popout.enabled = floating;
                this.store.borrow_mut().streamer.set(&synced.streamer);
                this.store.borrow_mut().players_off = synced.players_off;
                this.emoji_picker.update(cx, |p, cx| p.set_usage(used, cx));
            })
            .ok();
        })
        .detach();
        self.reload_subscribed_packs(cx);
        cx.notify();
    }

    fn refresh_unread(&mut self, cx: &mut Context<Self>) {
        let rx = backend::run(messaging::unread_counts());
        cx.spawn(async move |this, cx| {
            let Ok(counts) = rx.await else { return };
            this.update(cx, |this, cx| {
                this.store.borrow_mut().unread = Some(counts);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_event(&mut self, ev: UiEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            UiEvent::Booted(boot) => {
                if let Some(state) = backend::calls::status() {
                    self.call.apply_state(&state);
                    self.call_started(cx);
                }
                {
                    let mut store = self.store.borrow_mut();
                    store.my_npub = boot.npub;
                    store.profiles = boot
                        .profiles
                        .into_iter()
                        .map(|p| (p.id.clone(), p))
                        .collect();
                    store.chats = boot.chats;
                    store.resort();
                }
                self.load_chatlist_prefs();
                self.load_rail();
                self.load_voice_calls(cx);
                self.refresh_unread(cx);
                self.reload_communities(cx);
                self.reload_invites(cx);
            }
            UiEvent::Core(name, payload) => self.on_core_event(&name, payload, window, cx),
        }
        self.sync_request_mutuals(cx);
        cx.notify();
    }

    fn on_core_event(
        &mut self,
        name: &str,
        payload: Value,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat_id = payload
            .get("chat_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match name {
            "encryption_upgrading" => {
                if let Screen::Login(view) = &self.screen {
                    let n = |k| payload.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let (total, completed) = (n("total"), n("completed"));
                    view.update(cx, |v, cx| v.upgrading(total, completed, cx));
                }
            }
            "bunker_state" => {
                let state = match payload.get("state").and_then(Value::as_str) {
                    Some("online") => "online",
                    Some("offline") => "offline",
                    Some("connecting") => "connecting",
                    _ => "idle",
                };
                if let Some((_, dot)) = self.settings.as_mut().and_then(|s| s.signer.as_mut()) {
                    *dot = state;
                }
                // Steady-state health only: pairing on the login screen reports its own status.
                match state {
                    "offline" if self.live && !self.signer_offline_told => {
                        self.signer_offline_told = true;
                        self.show_toast("Remote signer offline. Please check your signer app.", cx);
                    }
                    "online" if self.live && self.signer_offline_told => {
                        self.signer_offline_told = false;
                        self.show_toast("Remote signer back online.", cx);
                    }
                    "offline" | "online" => {}
                    _ => self.signer_offline_told = false,
                }
            }
            "bunker_auth_url" => {
                if let Some(url) = payload.get("url").and_then(Value::as_str) {
                    self.signer_asks_to_open(url, cx);
                }
            }
            "webxdc_url_progress" => {
                let (Some(url), Some(pct)) = (payload.get("url").and_then(Value::as_str), payload.get("progress").and_then(Value::as_f64)) else { return };
                if let Some(t) = self.media.borrow_mut().transfers.get_mut(url) {
                    t.pct = Some(pct as f32);
                }
                self.refresh_rows(false);
            }
            "typing-update" => {
                let Some(chat) = payload.get("conversation_id").and_then(Value::as_str) else { return };
                let typers: Vec<String> = payload.get("typers").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                let mut store = self.store.borrow_mut();
                let now = store.now_ms();
                store.typing.insert(chat.to_string(), (typers, now));
                drop(store);
                cx.notify();
            }
            "embed_video_progress" => {
                let (Some(url), Some(pct)) = (payload.get("url").and_then(Value::as_str), payload.get("progress").and_then(Value::as_i64)) else { return };
                let mut media = self.media.borrow_mut();
                if let Some(crate::embeds::VideoState::Loading(p)) = media.embed_videos.get_mut(url)
                    && *p != pct
                    && pct < 100
                {
                    *p = pct;
                    drop(media);
                    self.refresh_rows(false);
                }
            }
            "media_import_progress" => {
                let n = |k: &str| payload.get(k).and_then(Value::as_u64).unwrap_or(0);
                let plain = payload.get("plain").and_then(Value::as_bool).unwrap_or(false);
                if let Some(s) = self.settings.as_mut() {
                    s.moving = (n("done") < n("total")).then(|| (format!("{} of {}", crate::security::thousands(n("done") as usize), crate::security::thousands(n("total") as usize)), plain));
                    cx.notify();
                }
            }
            "communities_changed"
            | "community_refreshed"
            | "community_kicked"
            | "community_surfaced" => {
                let id = payload
                    .as_str()
                    .or_else(|| payload.get("community_id").and_then(Value::as_str))
                    .map(String::from);
                if name == "community_refreshed" && id.is_some() {
                    self.resolve_art(id, cx);
                }
                self.reload_communities(cx);
            }
            "community_timeouts" => {
                if let Some(id) = payload.get("community_id").and_then(Value::as_str) {
                    self.refresh_timeouts(id, cx);
                }
            }
            "community_nicknames" if self.live => {
                if let Some(id) = payload.get("community_id").and_then(Value::as_str) {
                    let named = backend::community::nicknames(id);
                    self.store.borrow_mut().nicknames.insert(id.to_string(), named);
                    self.refresh_rows(false);
                }
            }
            "community_invite_received" | "community_invites_purged" => self.reload_invites(cx),
            "emoji_packs_updated" => self.reload_subscribed_packs(cx),
            "pinned_chats_updated" | "archived_chats_updated" => self.chatlist_event(name, &payload, cx),
            "chat_muted" => {
                let muted = payload.get("value").and_then(Value::as_bool).unwrap_or(false);
                if let Some(chat) = self.store.borrow_mut().chat_mut(&chat_id) {
                    chat.muted = muted;
                }
            }
            "profile_nick_changed" => {
                let id = payload.get("profile_id").and_then(Value::as_str).unwrap_or_default();
                let nick = payload.get("value").and_then(Value::as_str).unwrap_or_default().to_string();
                if let Some(p) = self.store.borrow_mut().profiles.get_mut(id) {
                    p.nickname = nick;
                }
            }
            "hidden_banners_updated" => {
                if let Some(ids) = payload.get("ids").and_then(|v| serde_json::from_value(v.clone()).ok()) {
                    self.store.borrow_mut().hidden_banners = ids;
                }
            }
            "rail_layout_updated" => self.rail_event(&payload, cx),
            "voice_state" | "voice_levels" | "voice_presence" => self.voice_event(name, &payload, cx),
            "community_pins_updated" => {
                if payload.get("channel_id").and_then(Value::as_str) == Some(self.pins.channel.as_str()) {
                    self.pins_refresh(cx);
                }
            }
            "start_layout_updated" => {
                if let (Some(sm), Ok(layout)) = (self.start.as_mut(), serde_json::from_value(payload.clone())) {
                    sm.layout = layout;
                }
            }
            "transfer_state" => self
                .security
                .update(cx, |f, cx| f.transfer_state(&payload, cx)),
            "mic_level" => {
                if let Some(a) = self
                    .settings
                    .as_mut()
                    .and_then(|s| s.audio.as_mut())
                    .filter(|a| a.testing)
                {
                    a.level = payload.get("mic").and_then(Value::as_f64).unwrap_or(0.) as f32;
                }
            }
            "call_state" => {
                self.call.apply_state(&payload);
                self.call_started(cx);
                self.sync_call_meter();
                // Once per call: it went ahead without a microphone, so say why it is muted.
                if self.call.mic_off
                    && self.call.phase == Some(crate::call::Phase::Active)
                    && self.call.id.is_some()
                    && self.call_mic_told != self.call.id
                {
                    self.call_mic_told = self.call.id.clone();
                    self.show_toast("You're muted: Vector doesn't have access to your microphone.", cx);
                }
            }
            "call_level" => {
                self.call.apply_level(&payload);
                self.sync_call_meter();
            }
            "call_stats" => self.call.apply_stats(&payload),
            "community_relay_move" => {
                if let Some(r) = self.rekey.as_mut() {
                    let pct = payload.get("pct").and_then(Value::as_f64).unwrap_or(0.) as f32;
                    let label = payload.get("label").and_then(Value::as_str).map(str::to_string);
                    r.set(pct, label);
                }
            }
            "call_video_refused" => {
                use crate::callvideo::VideoKind;
                for (k, kind) in [VideoKind::Camera, VideoKind::Screen].into_iter().enumerate() {
                    self.video.stop(kind);
                    self.call.video_mine[k] = false;
                }
                self.show_toast("They cannot receive video from this device", cx);
            }
            "audio_devices_changed" => self.reload_devices(cx),
            "player_refused" => {
                let msg = payload.get("msg").and_then(Value::as_str).unwrap_or_default().to_string();
                let why = match payload.get("code").and_then(Value::as_i64).unwrap_or(0) {
                    101 | 150 | 152 => "Its owner only allows it on YouTube",
                    100 => "Removed or made private on YouTube",
                    _ => "YouTube won't play it in other apps",
                };
                self.store.borrow_mut().player_refused.insert(msg, why.into());
                cx.notify();
            }
            "player_browse" => {
                if let Some(url) = payload.as_str().filter(|u| u.starts_with("https://")) {
                    cx.open_url(url);
                }
            }
            "synced_settings_updated" => {
                if let Some(on) = payload.get("players").and_then(Value::as_bool) {
                    self.store.borrow_mut().players_off = !on;
                }
                if let Some(view) = payload
                    .get("streamer")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<vector_core::synced_prefs::StreamerSettings>(v).ok())
                {
                    self.store.borrow_mut().streamer.set(&view);
                }
            }
            "wallpaper_updated" => {
                if let Some(chat) = self.store.borrow_mut().chat_mut(&chat_id) {
                    chat.wallpaper_path = payload.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
                    chat.wallpaper_ts = payload.get("ts").and_then(Value::as_u64).unwrap_or(0);
                    if let Some(b) = payload.get("blur").and_then(Value::as_u64) {
                        chat.wallpaper_blur = b as u8;
                    }
                    if let Some(d) = payload.get("dim").and_then(Value::as_u64) {
                        chat.wallpaper_dim = d as u8;
                    }
                }
            }
            "wallpaper_upload_progress" => {
                if self.wallpaper.busy {
                    let pct = payload.get("progress").and_then(Value::as_f64).unwrap_or(0.).clamp(0., 100.).round();
                    self.wallpaper.label = if pct > 0. { format!("Uploading\u{2026} {pct}%") } else { "Uploading\u{2026}".into() };
                }
            }
            "message_removed" => {
                let id = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(chat) = self.store.borrow_mut().chat_mut(&chat_id) {
                    chat.messages.retain(|m| m.id != id);
                }
                if matches!(&self.mode, Mode::Reply { id: r, .. } | Mode::Edit { id: r } if *r == id)
                {
                    self.mode = Mode::Normal;
                }
                self.store.borrow_mut().resort();
                if self.open.as_deref() == Some(chat_id.as_str()) {
                    self.refresh_rows(false);
                }
                if payload.get("reason").and_then(Value::as_str) == Some("cancelled") {
                    self.show_toast("Upload Cancelled", cx);
                }
            }
            "message_new" | "message_update" => {
                if let (Some(reason), Some(id)) = (
                    payload.get("reason").and_then(Value::as_str),
                    payload.pointer("/message/id").and_then(Value::as_str),
                ) {
                    self.fail_reasons
                        .borrow_mut()
                        .insert(id.to_string(), reason.to_string());
                }
                let Some(msg) = payload
                    .get("message")
                    .cloned()
                    .and_then(|m| serde_json::from_value::<Message>(m).ok())
                else {
                    return;
                };
                let below = name == "message_new"
                    && !msg.mine
                    && self.open.as_deref() == Some(chat_id.as_str())
                    && (self.gap.is_some() || !self.timeline.is_following_tail());
                if below {
                    self.unread_below += 1;
                }
                let known = if name == "message_new" {
                    self.store.borrow_mut().insert_message(&chat_id, msg)
                } else {
                    let old = payload
                        .get("old_id")
                        .and_then(Value::as_str)
                        .unwrap_or(&msg.id)
                        .to_string();
                    self.store.borrow_mut().replace_message(&chat_id, &old, msg);
                    true
                };
                if !known {
                    let id = chat_id.clone();
                    let rx = backend::run(messaging::chat_snapshot(id));
                    cx.spawn(async move |this, cx| {
                        let Ok(Some(chat)) = rx.await else { return };
                        this.update(cx, |this, cx| {
                            let mut store = this.store.borrow_mut();
                            if store.chat(&chat.id).is_none() {
                                store.chats.push(chat);
                                store.resort();
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                if self.open.as_deref() == Some(chat_id.as_str()) {
                    self.refresh_rows(true);
                }
            }
            "marketplace_install_progress" | "marketplace_apps_updated" => self.nexus_event(name, &payload, cx),
            "pivx_payment_received" => self.pivx_received(&payload, cx),
            "show_toast" => {
                if let Some(text) = payload.as_str() {
                    self.show_toast(text, cx);
                }
            }
            "miniapp_realtime_status" => {
                let topic = payload.get("topic").and_then(Value::as_str).unwrap_or_default();
                let peers: Vec<String> = payload
                    .get("peers")
                    .and_then(|p| serde_json::from_value(p.clone()).ok())
                    .unwrap_or_default();
                let active = payload.get("is_active").and_then(Value::as_bool).unwrap_or(false);
                self.media
                    .borrow_mut()
                    .xdc_live
                    .insert(topic.to_string(), crate::media::XdcLive { active, peers });
                self.refresh_rows(false);
                cx.notify();
            }
            "profile_update" => {
                if let Ok(p) = serde_json::from_value::<SlimProfile>(payload) {
                    self.store.borrow_mut().profiles.insert(p.id.clone(), p);
                }
            }
            "chat_mark_read" => {
                let last = payload
                    .get("last_read")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(chat) = self.store.borrow_mut().chat_mut(&chat_id) {
                    chat.last_read = last;
                }
            }
            "attachment_download_progress" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else {
                    return;
                };
                let mut media = self.media.borrow_mut();
                let t = media.transfers.entry(id.to_string()).or_default();
                if let Some(p) = payload
                    .get("progress")
                    .and_then(Value::as_f64)
                    .filter(|&p| p >= 0.)
                {
                    t.pct = Some(p as f32);
                    t.stage = None;
                }
                t.bps = payload.get("bytesPerSec").and_then(Value::as_f64);
            }
            "inline_image_progress" => {
                let (Some(url), Some(p)) = (
                    payload.get("url").and_then(Value::as_str),
                    payload.get("progress").and_then(Value::as_f64),
                ) else {
                    return;
                };
                if p >= 0. {
                    self.media.borrow_mut().linked_progress(url, p as f32);
                }
            }
            "attachment_upload_progress" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else {
                    return;
                };
                let p = payload
                    .get("progress")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.);
                let mut media = self.media.borrow_mut();
                let t = media.transfers.entry(id.to_string()).or_default();
                t.pct = Some(p as f32);
                t.stage = None;
            }
            "attachment_upload_stage" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else {
                    return;
                };
                let stage = payload
                    .get("stage")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.media
                    .borrow_mut()
                    .transfers
                    .entry(id.to_string())
                    .or_default()
                    .stage = stage;
            }
            "attachment_download_stage" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else {
                    return;
                };
                let stage = payload
                    .get("stage")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.media
                    .borrow_mut()
                    .transfers
                    .entry(id.to_string())
                    .or_default()
                    .stage = stage;
            }
            "attachment_download_result" => {
                let id = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let matched = payload
                    .get("old_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| id.clone());
                let success = payload
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let cancelled = payload
                    .get("cancelled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut media = self.media.borrow_mut();
                for k in [&id, &matched] {
                    media.transfers.remove(k);
                    media.started.remove(k);
                }
                if !success && !cancelled {
                    let reason = payload
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    media.failed.insert(matched, reason);
                } else {
                    media.failed.remove(&matched);
                }
            }
            "unread_counts_stale" => self.refresh_unread(cx),
            "chat_commands_updated" => self.on_commands_updated(&payload, cx),
            "sync_progress" => self.syncing = true,
            "transport_state" => self.on_transport_view(payload, cx),
            "sync_finished" => {
                self.syncing = false;
                self.refresh_unread(cx);
            }
            _ => {}
        }
    }

    fn rebuild_rows(&mut self) -> usize {
        let names = self.command_names();
        let store = self.store.borrow();
        let mut rows = self
            .open
            .as_ref()
            .and_then(|id| store.chat(id))
            .map(|c| data::timeline(c, store.now_ms(), self.read_anchor.as_ref(), names.as_deref(), &|m| store.hidden(c, m) == Some(data::Hidden::Blocked)))
            .unwrap_or_default();
        let gap = self.gap_index(&store);
        if let Some(g) = gap {
            if let Some(r) = rows.iter().position(|r| matches!(r, Row::Message { index, .. } if *index == g)) {
                rows.truncate(r + 1);
            }
        }
        if gap.is_none()
            && let Some(notice) = self.open.as_deref().and_then(|id| store.end_notice(id))
        {
            rows.push(Row::Notice(notice));
        }
        drop(store);
        if let Some(edge) = self.edge() {
            rows.insert(0, Row::Edge(edge));
        }
        self.rows = Rc::new(rows);
        self.rows.len()
    }

    /// `updateHistoryEdge`: what sits above the oldest message, once the database has no older.
    fn edge(&self) -> Option<data::Edge> {
        let id = self.open.as_deref()?;
        if !self.live {
            // Fixtures hold every chat's whole history.
            return Some(data::Edge::Start);
        }
        if !self.history_done.contains(id) {
            return None;
        }
        let community = self.store.borrow().community_of(id).is_some();
        Some(match (community, self.history_net.get(id)) {
            (true, Some(false)) => data::Edge::Retry,
            (true, None) => data::Edge::Loading,
            (false, _) if self.syncing => data::Edge::Loading,
            _ => data::Edge::Start,
        })
    }

    /// A Community channel's older page from the relays, once the database has none.
    fn load_older_net(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.open.clone() else { return };
        if !self.live || self.history_net.contains_key(id.as_ref()) || !self.history_net_busy.insert(id.to_string()) {
            return;
        }
        let before = self.store.borrow().chat(&id).and_then(|c| c.messages.first()).map(|m| m.at);
        let rx = backend::run(backend::community::sync_channel(id.to_string(), before));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.history_net_busy.remove(id.as_ref());
                match res {
                    Ok(Ok(true)) => {
                        this.history_net.insert(id.to_string(), true);
                    }
                    Ok(Ok(false)) => {
                        this.history_done.remove(id.as_ref());
                        if this.open.as_ref() == Some(&id) {
                            this.load_older(cx);
                        }
                    }
                    _ => {
                        this.history_net.insert(id.to_string(), false);
                    }
                }
                if this.open.as_ref() == Some(&id) {
                    this.relayout_keeping_top();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The open chat's rows changed: re-laid, following the end when `follow`.
    fn refresh_rows(&mut self, follow: bool) {
        let at_end = self.timeline.logical_scroll_top().item_ix + 12 >= self.timeline.item_count();
        let old = self.timeline.item_count();
        let new = self.rebuild_rows();
        self.timeline.splice(0..old, new);
        if follow && at_end {
            self.timeline.scroll_to_end();
        }
    }

    fn open_chat(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.as_ref() != Some(&id) {
            self.store.borrow_mut().unveiled.clear();
        }
        let elsewhere = self.community.as_ref().is_some_and(|v| {
            self.store
                .borrow()
                .community_of(&id)
                .is_none_or(|c| c.community_id != v.id)
        });
        if elsewhere {
            self.community = None;
        }
        self.self_destruct = if self.live {
            vector_core::self_destruct::chat_duration_secs(&id)
        } else {
            // Fixtures: VECTOR_SELF_DESTRUCT=<secs> sets every chat's timer.
            std::env::var("VECTOR_SELF_DESTRUCT").ok().and_then(|v| v.parse().ok())
        };
        // Fixtures: VECTOR_WALLPAPER_EDIT=<media path> stages that picture on the chat opened.
        if let Some(pick) = std::env::var("VECTOR_WALLPAPER_EDIT").ok().filter(|_| !self.live) {
            let path = self.store.borrow().media_root.join(pick);
            self.stage_wallpaper(id.to_string(), path, false, 60, cx);
        }
        self.profile = None;
        self.embed_modal = None;
        self.new_chat = None;
        self.edit_history = None;
        self.reaction_tip = None;
        self.reaction_details = None;
        self.rx_hover = None;
        if self.open.as_ref() != Some(&id) && self.rec.state != crate::recorder::State::Idle {
            if let Some(src) = self.rec.source {
                let _ = crate::audio::engine().map(|e| e.stop(src));
            }
            vector_media::voice::AudioRecorder::global().cancel();
            self.rec.idle();
        }
        self.drop_gap();
        self.bar.reset();
        self.unread_below = 0;
        self.return_shown = false;
        self.read_anchor = self.store.borrow().chat(&id).map(|c| {
            if c.unread_from.is_empty() { data::Unread::After(c.last_read.clone()) } else { data::Unread::At(c.unread_from.clone()) }
        });
        if self.open.as_ref() != Some(&id) {
            self.leave_media(id.as_ref(), window);
        }
        self.open = Some(id.clone());
        self.commands.close();
        self.load_commands(false, cx);
        self.pivx_merge(id.to_string(), cx);
        let count = self.rebuild_rows();
        self.timeline = timeline_state(count, cx.entity().downgrade());
        self.jumped = None;
        self.opening = self.live.then_some(None);
        self.tracked.clear();
        self.sync_mentions(cx);
        let community = self.store.borrow().community_of(&id).map(|c| c.community_id.clone());
        if let Some(cid) = community.filter(|c| self.live && !self.mention_rules.contains_key(c)) {
            self.load_mention_rules(cid, cx);
        }
        self.chip_passed.clear();
        self.close_popups(cx);
        self.pins_opened(cx);
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        if !self.live {
            return;
        }
        // History beyond the boot's last message comes from the database.
        let chat_id = id.to_string();
        messaging::set_active_chat(Some(chat_id.clone()));
        let rx = backend::run(async move {
            let views = messaging::message_views(chat_id.clone(), 200, 0).await;
            messaging::mark_as_read(chat_id, None).await;
            views
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                if this.open.as_ref() != Some(&id) {
                    return;
                }
                let Ok(Ok(mut views)) = res else {
                    this.opening = None;
                    return;
                };
                views.reverse();
                if views.len() < 200 {
                    this.history_done.insert(id.to_string());
                }
                if let Some(chat) = this.store.borrow_mut().chat_mut(&id) {
                    // Anything newer that landed meanwhile stays.
                    let newest = views.last().map(|m| m.at).unwrap_or(0);
                    let later: Vec<Message> = chat
                        .messages
                        .drain(..)
                        .filter(|m| m.at > newest && !views.iter().any(|v| v.id == m.id))
                        .collect();
                    chat.messages = views;
                    chat.messages.extend(later);
                }
                let count = this.rebuild_rows();
                this.timeline = timeline_state(count, cx.entity().downgrade());
                this.timeline.scroll_to_end();
                if let Some(Some(jump)) = this.opening.take() {
                    this.jump_to_message(jump, cx);
                }
                if this.history_done.contains(id.as_ref()) && this.timeline.logical_scroll_top().item_ix < 4 {
                    this.load_older(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The open chat's rows re-laid with the row at the top staying put.
    fn relayout_keeping_top(&mut self) {
        let top = self.timeline.logical_scroll_top();
        let store = self.store.borrow();
        let chat = self.open.as_ref().and_then(|id| store.chat(id));
        let held = chat.and_then(|c| {
            self.rows.get(top.item_ix..)?.iter().find_map(|r| match r {
                Row::Message { index, .. } => c.messages.get(*index).map(|m| m.id.clone()),
                _ => None,
            })
        });
        drop(store);
        let old = self.timeline.item_count();
        let new = self.rebuild_rows();
        self.timeline.splice(0..old, new);
        if let Some(ix) = held.and_then(|id| self.row_of(&id)) {
            self.timeline.scroll_to(gpui::ListOffset {
                item_ix: ix,
                offset_in_item: top.offset_in_item,
            });
        }
    }

    /// The timeline row of a message in the open chat.
    fn row_of(&self, id: &str) -> Option<usize> {
        let store = self.store.borrow();
        let chat = store.chat(self.open.as_deref()?)?;
        self.rows.iter().position(|r| matches!(r, Row::Message { index, .. } if chat.messages.get(*index).is_some_and(|m| m.id == id)))
    }

    /// Messages from the database folded into the open chat, in order; how many were new.
    fn merge_messages(&mut self, chat: &str, views: Vec<Message>) -> usize {
        let mut store = self.store.borrow_mut();
        let Some(c) = store.chat_mut(chat) else {
            return 0;
        };
        let have: HashSet<String> = c.messages.iter().map(|m| m.id.clone()).collect();
        let fresh: Vec<Message> = views
            .into_iter()
            .filter(|m| !have.contains(&m.id))
            .collect();
        let n = fresh.len();
        if n > 0 {
            c.messages.extend(fresh);
            c.messages.sort_by_key(|m| m.at);
        }
        n
    }

    /// The page of history before the oldest message held.
    pub(super) fn load_older(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.open.clone() else { return };
        if !self.live || self.paging {
            return;
        }
        if self.history_done.contains(id.as_ref()) {
            if self.store.borrow().community_of(&id).is_some() {
                self.load_older_net(cx);
            }
            return;
        }
        let Some(anchor) = self
            .store
            .borrow()
            .chat(&id)
            .and_then(|c| c.messages.first())
            .map(|m| m.id.clone())
        else {
            self.history_done.insert(id.to_string());
            self.relayout_keeping_top();
            return self.load_older(cx);
        };
        self.paging = true;
        let chat_id = id.to_string();
        let rx = backend::run(messaging::messages_around(chat_id, anchor, 101, 0));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.paging = false;
                if this.open.as_ref() != Some(&id) {
                    return;
                }
                let views = match res {
                    Ok(Ok(v)) => v,
                    _ => return,
                };
                if this.merge_messages(&id, views) == 0 {
                    this.history_done.insert(id.to_string());
                    this.relayout_keeping_top();
                    this.load_older(cx);
                    cx.notify();
                    return;
                }
                this.relayout_keeping_top();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `jumpToMessage`: centre the open chat on a message and flash it, loading it first if needed.
    fn jump_to_message(&mut self, id: String, cx: &mut Context<Self>) {
        self.jump_inner(id, None, cx);
    }

    /// A jump that says `miss` when the message can't be put on screen.
    pub(crate) fn jump_or_say(&mut self, id: String, miss: &'static str, cx: &mut Context<Self>) {
        self.jump_inner(id, Some(miss), cx);
    }

    fn jump_inner(&mut self, id: String, miss: Option<&'static str>, cx: &mut Context<Self>) {
        if miss.is_some() {
            self.jump_miss = miss.map(|m| (id.clone(), m));
        }
        if let Some(wait) = self.opening.as_mut() {
            *wait = Some(id);
            return;
        }
        if let Some(ix) = self.row_of(&id) {
            self.jump_miss = None;
            self.land_on(ix, id, cx);
            return;
        }
        let Some(chat) = self.open.clone() else {
            return;
        };
        if !self.live {
            return self.jump_missed(&id);
        }
        let rx = backend::run(messaging::messages_around(
            chat.to_string(),
            id.clone(),
            51,
            5000,
        ));
        cx.spawn(async move |this, cx| {
            let views = rx.await.ok().and_then(Result::ok).unwrap_or_default();
            this.update(cx, |this, cx| {
                if this.open.as_ref() != Some(&chat) {
                    return;
                }
                if !views.is_empty() {
                    this.merge_messages(&chat, views);
                    this.history_done.remove(chat.as_ref());
                    this.relayout_keeping_top();
                }
                if let Some(ix) = this.row_of(&id) {
                    this.jump_miss = None;
                    this.land_on(ix, id, cx);
                } else {
                    this.jump_missed(&id);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn jump_missed(&mut self, id: &str) {
        if let Some((_, say)) = self.jump_miss.take_if(|(j, _)| j == id) {
            self.toast = Some((say.into(), std::time::Instant::now()));
        }
    }

    fn land_on(&mut self, ix: usize, id: String, cx: &mut Context<Self>) {
        self.timeline.pause_following_tail();
        self.timeline.scroll_to(gpui::ListOffset {
            item_ix: ix,
            offset_in_item: px(0.),
        });
        // Centred once a layout has measured the rows above it.
        let list = self.timeline.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(32))
                .await;
            let h = list.viewport_bounds().size.height;
            let row = list.bounds_for_item(ix).map_or(px(0.), |b| b.size.height);
            if list.logical_scroll_top().item_ix == ix {
                list.scroll_by(-((h - row) / 2.).max(px(0.)));
            }
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        if self
            .jumped
            .as_ref()
            .is_none_or(|(j, at)| *j != id || at.elapsed().as_secs_f32() >= 1.5)
        {
            self.jumped = Some((id, std::time::Instant::now()));
        }
        cx.notify();
    }

    /// A click on a message's text.
    fn on_text(
        &mut self,
        msg_id: String,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            Target::Spoiler(id) => {
                self.revealed
                    .borrow_mut()
                    .entry(msg_id)
                    .or_default()
                    .insert(id);
                cx.notify();
            }
            Target::Link(url) => self.follow_url(&url, window, cx),
            Target::Code(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()))
            }
            Target::Mention(npub) => {
                let at = gpui::Bounds::new(window.mouse_position(), gpui::size(px(0.), px(0.)));
                self.show_mini(npub.to_string(), None, Some(at), cx)
            }
            Target::Action(a) if a.as_ref() == "edited" => self.show_edit_history(msg_id, window, cx),
            Target::Action(a) if a.starts_with(crate::inline::EMOJI_MISSING) => {
                let why = &a[crate::inline::EMOJI_MISSING.len()..];
                let text = if why.is_empty() {
                    "This emoji couldn't be loaded (the host may be offline, or the file was removed).".to_string()
                } else {
                    format!("This emoji couldn't be loaded ({}).", kit::escape(why))
                };
                let mut p = kit::Popup::notice("Emoji unavailable");
                p.text = Some(text.into());
                self.popup = Some((p, PopupAct::Notice));
            }
            Target::Action(_) => {}
        }
    }

    /// `showEditHistory`: against the message's row, its revisions' emoji from its tags and
    /// the equipped packs.
    fn reaction_popups(&self, store: &crate::data::Store, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let pane = self.chat_box.get().unwrap_or_else(|| gpui::Bounds::new(gpui::Point::default(), window.viewport_size()));
        let custom = |pop: &crate::reactpop::Pop| {
            let url = self.message(&pop.msg)?.reactions.iter().find(|r| r.emoji == pop.emoji)?.emoji_url.clone()?;
            match self.media.borrow_mut().custom(&timeline::emoji_source(store, &url), false) {
                crate::media::Custom::Ready(i) => Some(gpui::ImageSource::from(i)),
                _ => None,
            }
        };
        let anchor = |pop: &crate::reactpop::Pop| self.anchors.borrow().get(&pop.key).copied();
        let mut out = Vec::new();
        if let Some(pop) = &self.reaction_details {
            if let Some(a) = anchor(pop) {
                let app = cx.entity().downgrade();
                let close: Rc<dyn Fn(&mut Window, &mut gpui::App)> = Rc::new(move |_, cx| {
                    app.update(cx, |app, cx| {
                        app.reaction_details = None;
                        cx.notify();
                    })
                    .ok();
                });
                out.extend(crate::reactpop::details(pop, store, custom(pop), a, pane, close));
            }
        } else if let Some(pop) = &self.reaction_tip {
            if let Some(a) = anchor(pop) {
                out.extend(crate::reactpop::tip(pop, store, custom(pop), a, pane));
            }
        }
        out
    }

    fn show_edit_history(&mut self, id: String, window: &Window, cx: &mut Context<Self>) {
        let Some(m) = self.message(&id) else { return };
        let Some(entries) = m.edit_history.clone().filter(|e| !e.is_empty()) else { return };
        let anchor = self
            .row_of(&id)
            .and_then(|ix| self.timeline.bounds_for_item(ix))
            .unwrap_or_else(|| gpui::Bounds::new(window.mouse_position(), gpui::size(px(0.), px(0.))));
        let mut tags: Vec<(String, String)> = m.emoji_tags.iter().map(|t| (t.shortcode.clone(), t.url.clone())).collect();
        let picker = self.emoji_picker.read(cx);
        for e in &entries {
            for (code, url) in picker.customs_in(&e.content) {
                if !tags.iter().any(|(c, _)| *c == code) {
                    tags.push((code, url));
                }
            }
        }
        // Reopened by the same press that closed it: keep the measured size, so no blank frame.
        let size = self.edit_history.take().filter(|h| h.msg == id).map(|h| h.size).unwrap_or_default();
        self.edit_history = Some(crate::edithistory::EditHistory {
            msg: id,
            entries,
            anchor,
            tags,
            size,
            scroll: Default::default(),
        });
        cx.notify();
    }

    /// A message in the open chat, or in the one the floating player came from.
    fn set_send_state(&mut self, chat: &str, id: &str, failed: bool, pending: bool) {
        if let Some(m) = self
            .store
            .borrow_mut()
            .chat_mut(chat)
            .and_then(|c| c.messages.iter_mut().find(|m| m.id == id))
        {
            m.failed = failed;
            m.pending = pending;
        }
        if self.open.as_deref() == Some(chat) {
            self.refresh_rows(false);
        }
    }

    fn message(&self, id: &str) -> Option<Message> {
        let store = self.store.borrow();
        let find = |chat: &str| store.chat(chat)?.messages.iter().find(|m| m.id == id).cloned();
        self.open
            .as_deref()
            .and_then(find)
            .or_else(|| self.popout.item.as_ref().and_then(|i| find(&i.chat)))
    }

    fn on_act(
        &mut self,
        id: String,
        act: timeline::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use timeline::Act;
        match act {
            Act::Unveil(ids) => {
                self.store.borrow_mut().unveiled.extend(ids);
                self.refresh_rows(false);
            }
            Act::Mini(npub, key) => {
                let at = self.anchors.borrow().get(&key).copied();
                self.show_mini(npub, None, at, cx);
            }
            Act::AuthorMenu(npub, key, at) => self.author_menu(npub, key, at, cx),
            Act::MessageMenu(at) => self.message_menu(id, at, cx),
            Act::RetryHistory => {
                if let Some(open) = self.open.clone() {
                    self.history_net.remove(open.as_ref());
                    self.relayout_keeping_top();
                    self.load_older(cx);
                }
            }
            Act::Invite(key) => {
                let Some(crate::newchat::Preview::Ready(info)) =
                    self.invite_previews.get(&key).cloned()
                else {
                    return;
                };
                if self.store.borrow().community(&info.community_id).is_some() {
                    self.open_community(info.community_id, window, cx);
                } else if self.live {
                    self.join_by_link(key, info.name, window, cx);
                }
            }
            Act::Text(t) => self.on_text(id, t, window, cx),
            Act::Play(p) => self.play_link(id.to_string(), p, cx),
            Act::Jump(target) => self.jump_to_message(target, cx),
            Act::PivxClaim(code) => self.pivx_claim(code, cx),
            Act::Rerun(name) => self.rerun_command(name, window, cx),
            Act::Embed(a) => self.on_embed(a, window, cx),
            Act::Pack(a) => self.on_pack(a, cx),
            Act::Hover(true) => {
                self.hover_gen += 1;
                if self.hovered.as_deref() != Some(id.as_str()) {
                    self.hovered = Some(id);
                    cx.notify();
                }
            }
            Act::Hover(false) => {
                if self.hovered.as_deref() == Some(id.as_str())
                    && self.picker.as_deref() != Some(id.as_str())
                {
                    // The toolbar overhangs the row: give the pointer a moment to reach it.
                    let gen_id = id.clone();
                    let generation = self.hover_gen;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(100))
                            .await;
                        this.update(cx, |this, cx| {
                            if this.hovered.as_deref() == Some(gen_id.as_str())
                                && this.hover_gen == generation
                            {
                                this.hovered = None;
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            Act::Download(att) => self.start_download(id, att, false, cx),
            Act::CancelUpload(pending) => {
                if self.live {
                    messaging::cancel_upload(&pending);
                }
            }
            Act::CancelDownload(att) => {
                if self.live {
                    backend::attachments::cancel(&att);
                } else {
                    self.media.borrow_mut().started.remove(&att);
                }
                cx.notify();
            }
            Act::RevealMedia(att) => {
                self.media.borrow_mut().revealed.insert(att);
                cx.notify();
            }
            Act::OpenFile(att) => self.open_file(id, att, cx),
            Act::XdcUrl(url) => self.fetch_xdc_url(id, url, cx),
            Act::OpenXdcUrl(url) => self.open_xdc_url(id, url, cx),
            Act::Audio(att, crate::audio::Act::Art | crate::audio::Act::Album(crate::audio::AlbumAct::Art)) => {
                self.view_art(&att, window, cx)
            }
            Act::Audio(att, a) => {
                if matches!(a, crate::audio::Act::Toggle) {
                    self.yield_popout(crate::popout::Kind::Audio, &att);
                }
                self.audio_act(id, att, a, cx)
            }
            Act::Video(att, a) => {
                if matches!(a, crate::video::Act::Toggle) {
                    self.yield_popout(crate::popout::Kind::Video, &att);
                }
                self.video_act(id, att, a, cx)
            }
            Act::Model(att, a) => {
                if self.media.borrow_mut().models.act(&att, &a) {
                    self.open_model(id, att, window, cx);
                }
                cx.notify();
            }
            Act::View(open) => {
                let live = self.live;
                let viewer = cx.new(|cx| crate::viewer::Viewer::new(open, live, window, cx));
                cx.subscribe_in(
                    &viewer,
                    window,
                    |app, _, _: &crate::viewer::Closed, window, cx| {
                        app.viewer = None;
                        window.focus(&app.input.focus_handle(cx), cx);
                        cx.notify();
                    },
                )
                .detach();
                self.viewer = Some(viewer);
                cx.notify();
            }
            Act::React => self.toggle_picker(Some(id), window, cx),
            Act::ReactionHover(emoji, on) => {
                self.rx_gen += 1;
                if !on {
                    self.rx_hover = None;
                    if self.reaction_tip.take().is_some() {
                        cx.notify();
                    }
                    return;
                }
                self.rx_hover = Some((id.clone(), emoji.clone()));
                let at = self.rx_gen;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                    this.update(cx, |this, cx| {
                        if this.rx_gen == at && this.reaction_details.is_none() && this.rx_hover.as_ref() == Some(&(id.clone(), emoji.clone())) {
                            this.reaction_tip = Some(crate::reactpop::Pop::new(id, emoji));
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Act::ReactionDetails(emoji) => {
                self.rx_gen += 1;
                self.reaction_tip = None;
                self.reaction_details = Some(crate::reactpop::Pop::new(id, emoji));
                cx.notify();
            }
            Act::ReactionPress(emoji, down) => {
                self.rx_gen += 1;
                if !down {
                    return;
                }
                self.rx_long = false;
                let at = self.rx_gen;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                    this.update(cx, |this, cx| {
                        if this.rx_gen == at {
                            this.rx_long = true;
                            this.reaction_tip = None;
                            this.reaction_details = Some(crate::reactpop::Pop::new(id, emoji));
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Act::Unreact(emoji) => {
                if std::mem::take(&mut self.rx_long) {
                    return;
                }
                self.unreact(id, emoji, cx);
            }
            Act::ReactWith(emoji) => {
                if std::mem::take(&mut self.rx_long) {
                    return;
                }
                // Joining a custom reaction sends the picture it was made with.
                let url = self.message(&id).and_then(|m| {
                    m.reactions
                        .iter()
                        .find(|r| r.emoji == emoji)
                        .and_then(|r| r.emoji_url.clone())
                });
                self.react(id, emoji, url, cx);
            }
            Act::Reply => {
                let Some(m) = self.message(&id) else { return };
                let author = if m.mine {
                    self.store.borrow().my_npub.clone()
                } else {
                    m.npub
                        .clone()
                        .unwrap_or_else(|| self.open.as_deref().unwrap_or_default().to_string())
                };
                let name = self.store.borrow().name(&author);
                let snippet = if !m.content.is_empty() {
                    self.store.borrow().resolve_mentions(&m.content, None)
                } else if let Some(a) = m.attachments.first() {
                    data::file_type(&a.extension).1.to_string()
                } else {
                    String::new()
                };
                self.mode = Mode::Reply { id, name, snippet };
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
                cx.notify();
            }
            Act::Edit => {
                let Some(m) = self.message(&id) else { return };
                self.mode = Mode::Edit { id };
                self.input
                    .update(cx, |input, cx| input.set_text(&m.content, cx));
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
                cx.notify();
            }
            Act::Retry => {
                let Some(chat) = self.open.clone() else {
                    return;
                };
                if !self.live {
                    return;
                }
                let Some(m) = self.message(&id) else { return };
                // Back to pending at once, so a slow resend doesn't invite another tap.
                self.set_send_state(&chat, &id, false, true);
                let chat = chat.to_string();
                let rx = backend::run(backend::actions::retry(chat.clone(), id.clone()));
                cx.spawn(async move |this, cx| {
                    let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update(cx, |this, cx| match res {
                        Ok(true) => {}
                        // Nothing retained: drop the row and send the text fresh.
                        Ok(false) => {
                            let _ = backend::run(async move {
                                backend::actions::delete_failed(id).await?;
                                messaging::send(chat, m.content, m.replied_to, None).await
                            });
                        }
                        Err(e) => {
                            this.set_send_state(&chat, &id, true, false);
                            if e.contains("no longer on this device") {
                                this.notice("Cannot retry", e, cx);
                            }
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
                cx.notify();
            }
            Act::Delete => {
                let failed = self.message(&id).is_some_and(|m| m.mine && m.failed);
                let mut p = kit::Popup::notice("Delete this message?");
                p.text = Some("Are you sure you want to delete this message?".into());
                p.confirm = "Confirm".into();
                p.cancel = Some("Cancel".into());
                self.popup = Some((p, PopupAct::Delete { id, failed }));
                cx.notify();
            }
        }
    }

    /// A backend result whose failure the user should see.
    fn report(
        &self,
        rx: futures::channel::oneshot::Receiver<Result<(), String>>,
        title: &'static str,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = rx.await {
                this.update(cx, |this, cx| this.notice(title, e, cx)).ok();
            }
        })
        .detach();
    }

    /// Open the emoji picker for a reaction to `target`, or for the composer; again closes it.
    fn toggle_picker(
        &mut self,
        target: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The press that dismissed it by landing outside is the same one that would reopen it.
        if self.picker_open || self.picker_closed.elapsed() < std::time::Duration::from_millis(200)
        {
            self.close_picker(window, cx);
            return;
        }
        self.picker_open = true;
        self.picker = target.clone();
        let picker = self.emoji_picker.clone();
        let reaction = target.is_some();
        picker.update(cx, |p, cx| {
            p.reset(reaction, std::collections::HashMap::new(), window, cx);
            p.prefetch_gifs(cx);
        });
        if self.live {
            self.load_equipped(true, cx);
            let rx = backend::run(async {
                (
                    backend::actions::emoji_usage(),
                    backend::actions::kaomoji_recents(),
                )
            });
            cx.spawn(async move |_, cx| {
                let Ok((used, kaomoji)) = rx.await else {
                    return;
                };
                picker.update(cx, |p, cx| {
                    p.set_usage(used, cx);
                    p.set_kaomoji_recents(kaomoji, cx);
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// The status dialog's emoji button: the picker without GIFs, the dialog yielding the bottom.
    fn toggle_status_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker_open {
            self.close_picker(window, cx);
            return;
        }
        self.toggle_picker(None, window, cx);
        self.picker_status = true;
        self.emoji_picker.update(cx, |p, cx| {
            p.no_gifs = true;
            cx.notify();
        });
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.picker_open {
            return;
        }
        self.picker_open = false;
        self.picker = None;
        self.picker_closed = std::time::Instant::now();
        self.emoji_picker.update(cx, |p, cx| p.leave_creator(cx));
        let field = if std::mem::take(&mut self.picker_status) {
            self.status.clone()
        } else {
            None
        };
        window.focus(
            &field
                .unwrap_or_else(|| self.input.clone())
                .read(cx)
                .focus_handle(cx),
            cx,
        );
        cx.notify();
    }

    fn react(&mut self, id: String, emoji: String, url: Option<String>, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone() else {
            return;
        };
        if self.live {
            let rx = backend::run(backend::actions::react(chat.to_string(), id, emoji, url));
            self.report(rx, "Reaction Failed", cx);
        } else {
            let me = self.store.borrow().my_npub.clone();
            if let Some(m) = self
                .store
                .borrow_mut()
                .chat_mut(&chat)
                .and_then(|c| c.messages.iter_mut().find(|m| m.id == id))
            {
                m.reactions.push(vector_core::types::Reaction {
                    id: format!("local-{}", m.reactions.len()),
                    reference_id: id.clone(),
                    author_id: me,
                    emoji,
                    emoji_url: url,
                });
            }
        }
        cx.notify();
    }

    fn unreact(&mut self, id: String, emoji: String, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone() else { return };
        let me = self.store.borrow().my_npub.clone();
        let mut store = self.store.borrow_mut();
        let Some(m) = store.chat_mut(&chat).and_then(|c| c.messages.iter_mut().find(|m| m.id == id)) else { return };
        let Some(at) = m.reactions.iter().position(|r| r.emoji == emoji && r.author_id == me) else { return };
        if self.live {
            let rid = m.reactions[at].id.clone();
            drop(store);
            let rx = backend::run(backend::actions::unreact(rid));
            self.report(rx, "Reaction Failed", cx);
        } else {
            m.reactions.remove(at);
            drop(store);
            self.refresh_rows(false);
        }
        cx.notify();
    }

    /// A custom emoji: `:code:` into the draft, or a reaction carrying its picture.
    fn picked_custom(
        &mut self,
        code: String,
        url: String,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let literal = format!(":{code}:");
        self.emoji_picker.update(cx, |p, _| p.bump(&literal));
        if self.live {
            let (c, u) = (code.clone(), url.clone());
            let _ = backend::run(async move { backend::actions::bump_custom_emoji(&c, &u) });
        }
        if self.picker_status {
            if let Some(field) = self.status.clone() {
                field.update(cx, |input, cx| input.insert_spaced(&literal, window, cx));
            }
            if !shift {
                self.close_picker(window, cx);
            }
            return;
        }
        match self.picker.clone() {
            Some(id) => {
                let me = self.store.borrow().my_npub.clone();
                let already = self.message(&id).is_some_and(|m| {
                    m.reactions
                        .iter()
                        .any(|r| r.author_id == me && r.emoji == literal)
                });
                if !already {
                    self.react(id, literal, Some(url), cx);
                }
            }
            None => self
                .input
                .update(cx, |input, cx| input.insert_spaced(&literal, window, cx)),
        }
        if !shift {
            self.close_picker(window, cx);
        }
    }

    fn picked(&mut self, emoji: String, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji_picker.update(cx, |p, _| p.bump(&emoji));
        if self.live {
            let e = emoji.clone();
            let _ = backend::run(async move { backend::actions::bump_emoji(&e) });
        }
        if self.picker_status {
            if let Some(field) = self.status.clone() {
                field.update(cx, |input, cx| input.insert_spaced(&emoji, window, cx));
            }
            if !shift {
                self.close_picker(window, cx);
            }
            return;
        }
        match self.picker.clone() {
            Some(id) => {
                let me = self.store.borrow().my_npub.clone();
                let already = self.message(&id).is_some_and(|m| {
                    m.reactions
                        .iter()
                        .any(|r| r.author_id == me && r.emoji == emoji)
                });
                if !already {
                    self.on_act(id, timeline::Act::ReactWith(emoji), window, cx);
                }
            }
            None => self
                .input
                .update(cx, |input, cx| input.insert_spaced(&emoji, window, cx)),
        }
        if !shift {
            self.close_picker(window, cx);
        }
    }

    /// Fetch an attachment; its events paint the progress and the result.
    pub fn start_download(
        &mut self,
        msg_id: String,
        att_id: String,
        auto: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(chat) = self.open.clone() else {
            return;
        };
        if self.store.borrow().chat(&chat).is_some_and(|c| self.store.borrow().is_request(c)) {
            if !auto {
                self.show_toast("Accept the request to download files", cx);
            }
            return;
        }
        {
            let mut media = self.media.borrow_mut();
            if !media.started.insert(att_id.clone()) {
                return;
            }
            media.failed.remove(&att_id);
        }
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(backend::attachments::download(
            chat.to_string(),
            msg_id,
            att_id.clone(),
            auto,
        ));
        cx.spawn(async move |this, cx| {
            let _ = rx.await;
            this.update(cx, |this, cx| {
                this.media.borrow_mut().started.remove(&att_id);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A file on this device: shown in its folder, or a held one saved where the user picks.
    fn audio_input(&self, msg_id: &str, att_id: &str) -> Option<crate::audio::AudioInputSpec> {
        let att = self
            .message(msg_id)?
            .attachments
            .into_iter()
            .find(|a| a.id == att_id)?;
        if att.resident || att.path.is_empty() {
            return vector_core::simd::hex::hex_to_bytes_32_checked(&att.id)
                .map(crate::audio::AudioInputSpec::Held);
        }
        let path = std::path::PathBuf::from(&att.path);
        Some(crate::audio::AudioInputSpec::File(if path.is_absolute() {
            path
        } else {
            self.store.borrow().media_root.join(path)
        }))
    }

    fn audio_act(
        &mut self,
        msg: String,
        att: String,
        act: crate::audio::Act,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.audio_input(&msg, &att) else {
            return;
        };
        let work = self.media.borrow_mut().audio.act(&msg, &att, input, act);
        if let Some(w) = work {
            self.audio_work(vec![w], cx);
        }
        cx.notify();
    }

    /// A track's cover art, full size in the viewer.
    pub(crate) fn view_art(&mut self, att: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(art) = self.media.borrow().audio.tags(att).and_then(|t| t.cover.clone()) else { return };
        let s = art.size(0);
        let open = crate::viewer::Open { image: art, w: s.width.0 as u32, h: s.height.0 as u32, full: None };
        self.on_act(String::new(), timeline::Act::View(open), window, cx);
    }

    /// A model's source for a new view: its file, or what holds it for this account.
    pub(crate) fn model_src(&self, att: &vector_core::types::Attachment) -> Option<crate::model3d::Src> {
        if (att.resident || att.path.is_empty())
            && let Some(h) = vector_core::simd::hex::hex_to_bytes_32_checked(&att.id)
        {
            return vector_core::media::open(&h).map(crate::model3d::Src::Held);
        }
        let p = std::path::PathBuf::from(&att.path);
        Some(crate::model3d::Src::File(if p.is_absolute() { p } else { self.store.borrow().media_root.join(p) }))
    }

    fn open_model(&mut self, msg: String, att_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(att) = self.message(&msg).and_then(|m| m.attachments.into_iter().find(|a| a.id == att_id)) else { return };
        let Some(src) = self.model_src(&att) else { return };
        let file = Some(if att.resident { crate::model3d::viewer::FileAction::Save } else { crate::model3d::viewer::FileAction::Reveal });
        let ext = att.extension.to_ascii_lowercase();
        let viewer = cx.new(|cx| crate::model3d::viewer::ModelViewer::new(src, &ext, att.name.clone(), file, window, cx));
        cx.subscribe_in(&viewer, window, |app, _, e: &crate::model3d::viewer::Event, window, cx| match e {
            crate::model3d::viewer::Event::Closed => {
                app.model_viewer = None;
                window.focus(&app.input.focus_handle(cx), cx);
                cx.notify();
            }
            crate::model3d::viewer::Event::File => {
                if let Some((_, msg, att)) = app.model_viewer.clone() {
                    app.reveal_attachment(msg, att, cx);
                }
            }
        })
        .detach();
        self.model_viewer = Some((viewer, msg, att_id));
        cx.notify();
    }

    fn video_act(&mut self, msg: String, att: String, act: crate::video::Act, cx: &mut Context<Self>) {
        let input = self.audio_input(&msg, &att);
        self.media.borrow_mut().videos.act(&att, input, act);
        cx.notify();
    }

    fn video_work(&mut self, work: Vec<crate::video::Work>, cx: &mut Context<Self>) {
        for w in work {
            let rx = crate::video::run(w);
            cx.spawn(async move |this, cx| {
                let Ok(crate::video::WorkDone::Probed(att, info)) = rx.await else { return };
                this.update(cx, |this, cx| {
                    let mut media = this.media.borrow_mut();
                    let ok = info.is_some();
                    media.videos.probed(att.clone(), info);
                    // A video fetched by a tap plays as soon as it can.
                    if let Some(url) = att.strip_prefix("embed:")
                        && media.embed_autoplay.remove(url)
                        && ok
                        && let Some(crate::embeds::VideoState::Ready(spec)) = media.embed_videos.get(url)
                    {
                        let spec = spec.clone();
                        media.videos.act(&att, Some(spec), crate::video::Act::Toggle);
                    }
                    drop(media);
                    this.refresh_rows(false);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn audio_work(&mut self, work: Vec<crate::audio::Work>, cx: &mut Context<Self>) {
        for w in work {
            let rx = crate::audio::run(w);
            cx.spawn(async move |this, cx| {
                let Ok(done) = rx.await else { return };
                this.update(cx, |this, cx| {
                    let mut media = this.media.borrow_mut();
                    match done {
                        crate::audio::WorkDone::Probed(att, ms) => media.audio.probed(att, ms),
                        crate::audio::WorkDone::Loaded(att, res) => media.audio.loaded(&att, res),
                        crate::audio::WorkDone::Tags(att, tags) => media.audio.read_tags(att, tags),
                        crate::audio::WorkDone::Transcribed(att, res) => media.audio.transcribed(&att, res),
                    }
                    drop(media);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// A voice message that ended hands on to the next one in its chat.
    fn on_audio_event(&mut self, name: &str, v: serde_json::Value, cx: &mut Context<Self>) {
        if name == "audio_ended"
            && self.rec.source.is_some()
            && v.get("id").and_then(Value::as_u64) == self.rec.source.map(u64::from)
        {
            self.rec.ended();
        }
        if name == "whisper_download_progress"
            && let Some(d) = self.settings.as_mut().and_then(|s| s.voice.as_mut()).and_then(|v| v.download.as_mut())
        {
            let mb = |k: &str| v.get(k).and_then(Value::as_f64).filter(|b| *b > 0.).map(|b| b / (1024. * 1024.));
            *d = match (mb("downloaded_bytes"), mb("total_bytes")) {
                (Some(a), Some(b)) => format!("{a:.1}/{b:.1} MB"),
                _ => format!("{}%", v.get("progress").and_then(Value::as_f64).unwrap_or(0.).round()),
            };
        }
        let ended = self.media.borrow_mut().audio.on_event(name, &v);
        cx.notify();
        let Some(att) = ended else { return };
        let floating = self.popout.item.as_ref().filter(|i| i.att == att).map(|i| i.chat.clone());
        let Some(chat) = floating.clone().or_else(|| self.open.as_ref().map(|s| s.to_string())) else { return };
        let next = (|| {
            let store = self.store.borrow();
            let chat = store.chat(&chat)?;
            let at = chat
                .messages
                .iter()
                .position(|m| m.attachments.iter().any(|a| a.id == att))?;
            let voice = |a: &vector_core::Attachment| {
                a.name.is_empty()
                    && (a.downloaded || a.resident)
                    && crate::audio::EXTENSIONS.contains(&a.extension.to_ascii_lowercase().as_str())
            };
            if !chat.messages[at].attachments.iter().any(|a| a.id == att && voice(a)) {
                return None;
            }
            chat.messages.get(at + 1).and_then(|m| {
                m.attachments
                    .iter()
                    .find(|a| voice(a))
                    .map(|a| (m.id.clone(), a.id.clone()))
            })
        })();
        if floating.is_some() {
            match &next {
                Some((msg, att)) => {
                    if let Some(i) = self.popout.item.as_mut() {
                        i.msg = msg.clone();
                        i.att = att.clone();
                    }
                }
                None => {
                    self.popout.close();
                }
            }
        }
        if let Some((msg, att)) = next {
            self.audio_act(msg, att, crate::audio::Act::Toggle, cx);
        }
    }

    fn open_file(&mut self, msg_id: String, att_id: String, cx: &mut Context<Self>) {
        let Some(att) = self
            .message(&msg_id)
            .and_then(|m| m.attachments.into_iter().find(|a| a.id == att_id))
        else {
            return;
        };
        if att.extension.eq_ignore_ascii_case("xdc") {
            self.launch_miniapp(msg_id, att, cx);
            return;
        }
        if !att.resident && !att.path.is_empty() {
            let path = std::path::PathBuf::from(&att.path);
            let path = if path.is_absolute() {
                path
            } else {
                self.store.borrow().media_root.join(path)
            };
            cx.reveal_path(&path);
            return;
        }
        if !self.live {
            return;
        }
        let name = if att.name.is_empty() {
            format!("{}.{}", &att.id[..16.min(att.id.len())], att.extension)
        } else {
            att.name.clone()
        };
        self.save_resident(att.id, name, cx);
    }

    /// A file held in the app's own storage, saved where the user picks.
    fn save_resident(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        let Some(hash) = vector_core::simd::hex::hex_to_bytes_32_checked(&id) else {
            return;
        };
        let dir = dirs::download_dir().unwrap_or_else(std::env::temp_dir);
        let pick = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(dest))) = pick.await else {
                return;
            };
            let rx = backend::run(async move {
                let bytes = backend::attachments::bytes(Some(hash), String::new())
                    .await
                    .ok_or("This file isn't on this device any more")?;
                tokio::fs::write(&dest, bytes)
                    .await
                    .map_err(|e| e.to_string())
            });
            if let Ok(Err(e)) = rx.await {
                this.update(cx, |this, cx| this.notice("Save Failed", e.to_string(), cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Mini App cards' names and icons, read from their packages.
    fn read_packages(&mut self, list: Vec<(String, crate::media::Source)>, cx: &mut Context<Self>) {
        for (att, src) in list {
            let rx = backend::run(async move {
                let file = match src {
                    crate::media::Source::File(p) => p.to_string_lossy().into_owned(),
                    crate::media::Source::Held(h) => vector_core::media::MediaRef::held(&h),
                };
                crate::miniapp::info(file).await
            });
            cx.spawn(async move |this, cx| {
                let Ok(Ok(info)) = rx.await else { return };
                this.update(cx, |this, cx| {
                    this.media.borrow_mut().xdc.insert(att, Some(info));
                    this.refresh_rows(false);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Mini App links' cache probes: a link already fetched for its message shows its card.
    fn probe_xdc_urls(&mut self, list: Vec<(String, String)>, cx: &mut Context<Self>) {
        for (msg, url) in list {
            let key = crate::attachments::xdc_url_key(&msg, &url);
            let rx = backend::run(backend::xdcurl::resolve(url, msg, false));
            cx.spawn(async move |this, cx| {
                let found = rx.await.ok().and_then(Result::ok).flatten();
                this.update(cx, |this, cx| {
                    let mut media = this.media.borrow_mut();
                    if matches!(media.xdc_urls.get(&key), Some(crate::attachments::XdcUrl::Probing)) {
                        media.xdc_urls.insert(key, found.map_or(crate::attachments::XdcUrl::Absent, crate::attachments::XdcUrl::Ready));
                        drop(media);
                        this.refresh_rows(false);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn fetch_xdc_url(&mut self, msg: String, url: String, cx: &mut Context<Self>) {
        use crate::attachments::XdcUrl;
        if !self.live {
            return;
        }
        let key = crate::attachments::xdc_url_key(&msg, &url);
        {
            let mut media = self.media.borrow_mut();
            if matches!(media.xdc_urls.get(&key), Some(XdcUrl::Fetching | XdcUrl::Ready(_))) {
                return;
            }
            media.xdc_urls.insert(key.clone(), XdcUrl::Fetching);
            media.transfers.insert(url.clone(), Default::default());
        }
        self.refresh_rows(false);
        cx.notify();
        let rx = backend::run(backend::xdcurl::resolve(url.clone(), msg, true));
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update(cx, |this, cx| {
                let mut media = this.media.borrow_mut();
                media.transfers.remove(&url);
                let state = match result {
                    Ok(Some(r)) => XdcUrl::Ready(r),
                    Ok(None) => XdcUrl::Failed("Not found".into()),
                    Err(e) => XdcUrl::Failed(e),
                };
                media.xdc_urls.insert(key, state);
                drop(media);
                this.refresh_rows(false);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A fetched Mini App link opens as an attachment would, its session keyed to the message.
    fn open_xdc_url(&mut self, msg: String, url: String, cx: &mut Context<Self>) {
        let key = crate::attachments::xdc_url_key(&msg, &url);
        let Some(crate::attachments::XdcUrl::Ready(r)) = self.media.borrow().xdc_urls.get(&key).cloned() else { return };
        if !self.live {
            return;
        }
        let att = vector_core::Attachment {
            id: format!("xdcurl:{key}"),
            extension: "xdc".into(),
            webxdc_topic: Some(r.topic.clone()),
            ..Default::default()
        };
        let src = r.path.to_string_lossy().into_owned();
        self.perm_gate(
            backend::market::Ask::File(src.clone()),
            move |this, cx| this.realtime_gate(src.clone(), move |this, cx| this.launch_miniapp_now(msg, src, att, cx), cx),
            cx,
        );
    }

    /// A Mini App attachment opens in its own window, keyed to this chat and message.
    fn launch_miniapp(&mut self, msg_id: String, att: vector_core::Attachment, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let file = (!att.resident && !att.path.is_empty()).then(|| {
            let path = std::path::PathBuf::from(&att.path);
            if path.is_absolute() {
                path
            } else {
                self.store.borrow().media_root.join(path)
            }
        });
        let hash = vector_core::simd::hex::hex_to_bytes_32_checked(&att.id);
        let src = match (&file, hash) {
            (Some(f), _) => f.to_string_lossy().into_owned(),
            (None, Some(h)) => vector_core::media::MediaRef::held(&h),
            (None, None) => return self.show_toast("This app isn't on this device any more", cx),
        };
        self.perm_gate(
            backend::market::Ask::File(src.clone()),
            move |this, cx| {
                this.realtime_gate(src.clone(), move |this, cx| this.launch_miniapp_now(msg_id, src, att, cx), cx)
            },
            cx,
        );
    }

    fn launch_miniapp_now(&mut self, msg_id: String, file: String, att: vector_core::Attachment, cx: &mut Context<Self>) {
        let chat = self.open.as_ref().map(|s| s.to_string()).unwrap_or_default();
        let topic = att.webxdc_topic.clone();
        let key = topic.clone().unwrap_or_else(|| format!("att:{}", att.id));
        let rx = backend::run(crate::miniapp::open(file, chat, msg_id, None, topic));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Err(e)) => this.show_toast(&e, cx),
                Ok(Ok(())) => {
                    this.media.borrow_mut().xdc_live.entry(key).or_default().active = true;
                    this.refresh_rows(false);
                    cx.notify();
                }
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    fn sync_call_meter(&mut self) {
        let live = self.call.live_mic();
        if let Some(a) = self.settings.as_mut().and_then(|s| s.audio.as_mut()) {
            a.call = live;
        }
    }

    /// `promptMicAccess`: where to allow the microphone, and a way there where the OS has one.
    pub(super) fn mic_access(&mut self, cx: &mut Context<Self>) {
        let (where_, can_open) = if cfg!(target_os = "macos") {
            ("System Settings → Privacy & Security → Microphone", true)
        } else if cfg!(windows) {
            ("Settings → Privacy → Microphone", true)
        } else {
            ("your device's app settings", false)
        };
        let mut p = kit::Popup::notice("Microphone Access");
        p.text = Some(format!("Vector doesn't have access to your microphone. Allow it in {where_}, then try again.").into());
        if can_open {
            p.confirm = "Open Settings".into();
            p.cancel = Some("Cancel".into());
        }
        self.popup = Some((p, if can_open { PopupAct::OpenMicSettings } else { PopupAct::Notice }));
        cx.notify();
    }

    /// `humanizeUploadError`: a refused upload, titled by the kind of refusal.
    fn upload_failed(&mut self, raw: &str, cx: &mut Context<Self>) {
        let u = crate::uploaderr::humanize(raw);
        let mut p = kit::Popup::notice(u.title);
        p.text = Some(u.body.into());
        p.fine = u.fine.map(Into::into);
        if u.routing {
            let app = cx.entity().downgrade();
            p.link = Some((
                "Open Routing".into(),
                Rc::new(move |window, cx| {
                    app.update(cx, |app, cx| {
                        app.popup = None;
                        app.open_settings_path("privacy/routing", window, cx);
                    })
                    .ok();
                }),
            ));
        }
        self.popup = Some((p, PopupAct::Notice));
        cx.notify();
    }

    /// A notice with no icon, escaping `text`: `popupConfirm(title, escapeHtml(e), true)`.
    fn notice_plain(&mut self, title: &str, text: &str, cx: &mut Context<Self>) {
        let mut p = kit::Popup::notice(title.to_string());
        p.icon = None;
        p.text = Some(kit::escape(text).into());
        self.popup = Some((p, PopupAct::Notice));
        cx.notify();
    }

    fn notice(&mut self, title: &str, text: String, cx: &mut Context<Self>) {
        let mut p = kit::Popup::notice(title.to_string());
        p.text = Some(text.into());
        self.popup = Some((p, PopupAct::Notice));
        cx.notify();
    }

    fn answer_popup(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((popup, act)) = self.popup.take() else {
            return;
        };
        cx.notify();
        if let PopupAct::NetKeepRealtime(kind) = act {
            self.use_network(kind, Some(yes), true, cx);
            return;
        }
        if !yes {
            return;
        }
        match act {
            PopupAct::Notice => {}
            PopupAct::OpenMicSettings => {
                if let Err(e) = vector_media::mic_access::open_settings() {
                    self.show_toast(&e, cx);
                }
            }
            PopupAct::MoveRelays { id, relays } => self.move_relays(id, relays, cx),
            PopupAct::NetKeepRealtime(_) => {}
            PopupAct::NetUse(kind) => self.use_network(kind, None, true, cx),
            PopupAct::NetI2pOnly => self.set_i2p_only(true, cx),
            PopupAct::NetOpRemove(i) => self.remove_outproxy(i, cx),
            PopupAct::NetOpReset => self.save_outproxies(None, false, cx),
            PopupAct::OpenWeb(url) => cx.open_url(&url),
            PopupAct::DeleteChannel { community, channel, category } => {
                self.delete_channel(community, channel, category, cx)
            }
            PopupAct::LeaveCommunity { id, name, owner } => {
                if owner
                    && popup
                        .input
                        .as_ref()
                        .is_none_or(|i| i.read(cx).text().trim() != name)
                {
                    self.notice(
                        "Not Deleted",
                        "The name did not match, so nothing was changed.".into(),
                        cx,
                    );
                    return;
                }
                if !self.live {
                    self.store
                        .borrow_mut()
                        .communities
                        .retain(|c| c.community_id != id);
                    self.leave_community();
                    return;
                }
                let rx = backend::run(backend::community::leave_or_delete(id, owner));
                cx.spawn(async move |this, cx| {
                    let result = rx.await;
                    this.update(cx, |this, cx| match result {
                        Ok(Err(e)) => this.notice(
                            if owner {
                                "Failed to Delete"
                            } else {
                                "Failed to Leave"
                            },
                            e,
                            cx,
                        ),
                        _ => {
                            this.leave_community();
                            this.reload_communities(cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
            PopupAct::InvitePublic => self.ask_invite_label(window, cx),
            PopupAct::InviteLabel => {
                let label = popup
                    .input
                    .as_ref()
                    .map(|i| i.read(cx).text().trim().to_string())
                    .filter(|l| !l.is_empty());
                self.create_invite_link(label, cx);
            }
            PopupAct::InvitePrivate { token } => self.revoke_invite_link(token, true, cx),
            PopupAct::DeleteRole(role_id) => self.role_op(true, role_id, cx),
            PopupAct::ModRevoke => self.mod_revoke(cx),
            PopupAct::ModRotate => self.mod_rotate(false, cx),
            PopupAct::ModBanRotate => self.mod_rotate(true, cx),
            PopupAct::ActivateRoles => self.role_op(false, String::new(), cx),
            PopupAct::Block { npub } => self.set_blocked(npub, true, cx),
            PopupAct::JoinInvite { key, name } => self.join_by_link(key, name, window, cx),
            PopupAct::BlockRequest { npub } => {
                if self.open.as_deref() == Some(npub.as_str()) {
                    self.open = None;
                }
                self.set_blocked(npub, true, cx);
            }
            PopupAct::RemoveAccount { npub } => self.remove_account(npub, window, cx),
            PopupAct::Unblock { npub } => {
                let from_settings = self.settings.as_ref().is_some_and(|s| s.blocked.is_some());
                if let Some(s) = self.settings.as_mut()
                    && let Some(list) = s.blocked.as_mut()
                {
                    list.retain(|u| u.npub != npub);
                }
                self.set_blocked_toast(npub, false, if from_settings { "User unblocked" } else { "User Unblocked" }, cx);
            }
            PopupAct::RemoveWallpaper(chat) => {
                let rx = backend::run(vector_core::db::scoped(async move {
                    vector_core::wallpaper::remove_wallpaper(&chat).await
                }));
                cx.spawn(async move |this, cx| {
                    if let Ok(Err(e)) = rx.await {
                        this.update(cx, |this, cx| this.notice("Wallpaper not removed", e, cx)).ok();
                    }
                })
                .detach();
            }
            PopupAct::DeleteModel(model) => {
                let rx = backend::run(backend::transcribe::delete(model.to_string()));
                cx.spawn(async move |this, cx| {
                    let ok = rx.await.unwrap_or(false);
                    this.update(cx, |this, cx| {
                        if ok {
                            this.show_toast("Model Deleted", cx);
                        }
                        this.load_voice(cx);
                    })
                    .ok();
                })
                .detach();
            }
            PopupAct::ClearStorage => {
                if let Some(s) = self.settings.as_mut() {
                    s.clearing = true;
                }
                let rx = backend::run(backend::storage::clear_all());
                self.after_storage_clear(rx, false, cx);
            }
            PopupAct::DeleteSlice { category, exts } => {
                if let Some(s) = self.settings.as_mut() {
                    s.donut.busy = true;
                }
                let rx = backend::run(backend::storage::clear_category(category, exts));
                self.after_storage_clear(rx, true, cx);
            }
            PopupAct::AddServer => {
                let Some(input) = popup.input else { return };
                let url = input.read(cx).text().trim().to_string();
                if url.is_empty() {
                    return;
                }
                let rx = backend::run(backend::network::add_server(url));
                cx.spawn(async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update(cx, |this, cx| {
                        if let Err(e) = result {
                            let mut popup = kit::Popup::notice("Could not add server");
                            popup.text = Some(kit::escape(&e).into());
                            this.popup = Some((popup, PopupAct::Notice));
                        }
                        this.load_network(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            PopupAct::DisableRelay { url } => self.net_change(
                backend::run(async move {
                    backend::network::toggle_default_relay(url, false)
                        .await
                        .map(|_| ())
                }),
                true,
                cx,
            ),
            PopupAct::RemoveRelay { url } => self.net_change(
                backend::run(async move {
                    backend::network::remove_custom_relay(url).await.map(|_| ())
                }),
                true,
                cx,
            ),
            PopupAct::RemoveServer { url } => self.net_change(
                backend::run(async move { backend::network::remove_server(url).await.map(|_| ()) }),
                true,
                cx,
            ),
            PopupAct::StopKeeping => self.set_keep_media(false, cx),
            PopupAct::KeepPlain => self.set_keep_unencrypted(true, cx),
            PopupAct::Realtime(then) => {
                backend::calls::allow_realtime();
                self.call_go(then, cx);
            }
            PopupAct::RemoveMiniApp { name, pivx } => self.remove_miniapp(name, pivx, cx),
            PopupAct::NexusUninstall(id) => self.nexus_uninstall_confirmed(id, cx),
            PopupAct::RealtimeThen(then) => {
                backend::calls::allow_realtime();
                then(self, cx);
            }
            PopupAct::RemoveMember { community, npub, ban } => self.remove_member(community, npub, ban, cx),
            PopupAct::Logout => {
                let me = self.store.borrow().my_npub.clone();
                self.settings = None;
                self.remove_account_or(me, "Logout failed", window, cx);
            }
            PopupAct::RenameFolder { id } => {
                if let Some(input) = popup.input {
                    let name = input.read(cx).text().trim().to_string();
                    self.start_edit(move |l| l.rename_folder(&id, &name), cx);
                }
            }
            PopupAct::RailRename(folder) => {
                let name = popup.input.map(|i| i.read(cx).text().trim().to_string()).unwrap_or_default();
                self.rail_rename(folder, name, cx);
            }
            PopupAct::CommunityNickname { community, npub } => {
                let nick = popup
                    .input
                    .map(|i| i.read(cx).text().trim().to_string())
                    .unwrap_or_default();
                self.set_community_nickname(community, npub, nick, cx);
            }
            PopupAct::Nickname { npub } => {
                if let Some(input) = popup.input {
                    let nick = input.read(cx).text().trim().to_string();
                    if !self.blocked_by_sync(cx) {
                        self.set_nickname(npub, nick, cx);
                    }
                }
            }
            PopupAct::Hide(id) => self.hide_message(id, cx),
            PopupAct::Delete { id, failed } => {
                if !self.live {
                    let open = self.open.clone();
                    if let Some(chat) = open.and_then(|o| {
                        self.store
                            .borrow_mut()
                            .chat_mut(&o)
                            .map(|c| c.messages.retain(|m| m.id != id))
                    }) {
                        let _ = chat;
                    }
                    self.refresh_rows(false);
                    return;
                }
                let rx = if failed {
                    backend::run(backend::actions::delete_failed(id))
                } else {
                    backend::run(backend::actions::delete(id))
                };
                self.report(rx, "Delete Failed", cx);
            }
        }
    }

    /// A draft that grew tells the open chat we are typing, once per lapse. Not while editing,
    /// not for a `/` command, not to a request still waiting, and not to ourselves.
    fn typed(&mut self, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        let (len, slash) = (input.text().len(), input.text().starts_with('/'));
        let grew = len > std::mem::replace(&mut self.draft_len, len);
        let Some(chat) = self.open.as_ref().map(|s| s.to_string()) else { return };
        if !self.live || !grew || slash || matches!(self.mode, Mode::Edit { .. }) {
            return;
        }
        if self.typing_sent.as_ref().is_some_and(|(id, at)| *id == chat && at.elapsed().as_millis() < data::TYPING_TTL_MS as u128) {
            return;
        }
        {
            let store = self.store.borrow();
            if chat == store.my_npub || store.chats.iter().find(|c| c.id == chat).is_none_or(|c| store.is_request(c)) {
                return;
            }
        }
        self.typing_sent = Some((chat.clone(), std::time::Instant::now()));
        let _ = backend::run(async move {
            if !backend::prefs::sql_bools(&[("send_typing_indicators", true)])[0] {
                return;
            }
            if chat.starts_with("npub1") {
                let _ = vector_core::VectorCore.send_typing(&chat).await;
            } else {
                let _ = vector_core::VectorCore.send_community_typing(&chat).await;
            }
        });
    }

    fn cancel_mode(&mut self, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::Edit { .. }) {
            self.input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.mode = Mode::Normal;
        self.picker = None;
        cx.notify();
    }

    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(id) = self.open.clone() else { return };
        // Notes to self are yours alone.
        if !std::mem::take(&mut self.secret_pass)
            && id.as_ref() != self.store.borrow().my_npub
            && let Some(kind) = vector_core::secrets::detect(&text)
        {
            return self.open_secret_guard(kind, text, id.to_string(), cx);
        }
        let Some((text, bot)) = self.route_command(&text, cx) else {
            self.input.update(cx, |i, cx| i.set_text(&text, cx));
            return;
        };
        let text = crate::mentions::resolve(&text, &std::mem::take(&mut self.tracked));
        self.sync_mentions(cx);
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        if let Mode::Edit { id: msg_id } = &mode {
            if self.live {
                let rx = backend::run(backend::actions::edit(id.to_string(), msg_id.clone(), text));
                self.report(rx, "Edit Failed", cx);
            } else if let Some(m) = self
                .store
                .borrow_mut()
                .chat_mut(&id)
                .and_then(|c| c.messages.iter_mut().find(|m| m.id == *msg_id))
            {
                m.content = text;
                m.edited = true;
            }
            self.refresh_rows(false);
            cx.notify();
            return;
        }
        let reply = match &mode {
            Mode::Reply { id, .. } => id.clone(),
            _ => String::new(),
        };
        if self.live {
            let receiver = id.to_string();
            let used = crate::emoji::in_text(&text);
            let custom = self.emoji_picker.read(cx).customs_in(&text);
            if !used.is_empty() || !custom.is_empty() {
                let _ =
                    backend::run(async move { backend::actions::bump_emoji_batch(used, custom) });
            }
            let rx = backend::run(messaging::send(receiver, text, reply, bot));
            cx.spawn(async move |this, cx| {
                if let Ok(Err(e)) = rx.await {
                    log::warn!("send failed: {e}");
                    this.update(cx, |_, cx| cx.notify()).ok();
                }
            })
            .detach();
            return;
        }
        // Fixtures: a local echo.
        {
            let mut store = self.store.borrow_mut();
            let now = store.now_ms();
            let me = store.my_npub.clone();
            let Some(chat) = store.chat_mut(&id) else {
                return;
            };
            let (text, colors) = vector_core::text_color::extract(&text);
            let addressed_bots = bot.into_iter().filter(|_| !id.starts_with("npub1")).collect();
            let msg = Message {
                addressed_bots,
                id: format!("local-{now}-{}", chat.messages.len()),
                content: text,
                text_spans: colors.into_iter().map(vector_core::text_spans::TextSpan::Color).collect(),
                replied_to: reply,
                at: now,
                mine: true,
                npub: Some(me),
                ..Default::default()
            };
            chat.last_read = msg.id.clone();
            chat.messages.push(msg);
            store.resort();
        }
        self.drop_gap();
        self.refresh_rows(false);
        self.timeline.scroll_to_end();
        cx.notify();
    }
}

impl Render for VectorApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        cx.defer(move |cx| crate::relayinfo::pump(me, cx));
        let rail_mode = self.rail_mode(window);
        self.vw = f32::from(window.viewport_size().width);
        let slim = self.vw < slim_ops::WIDE_MIN;
        crate::theme::set_slim(slim);
        if let Some(v) = &self.community
            && v.search.read(cx).look().size != crate::community::member_search_look(slim).size
        {
            v.search.update(cx, |t, cx| t.set_look(crate::community::member_search_look(slim), cx));
        }
        self.is_slim = slim;
        self.archive.slim = slim;
        if let Some(s) = self.settings.as_mut() {
            s.narrow = f32::from(window.viewport_size().width) <= 600.;
            s.phone = f32::from(window.viewport_size().width) <= 520.;
            if s.slim_geo.borrow_mut().step(&s.slim_scroll) {
                window.request_animation_frame();
            }
        }
        if let Some(s) = self.settings.as_mut()
            && s.slim != slim
        {
            s.slim = slim;
            s.search.update(cx, |t, cx| t.set_look(settings_search_look(slim), cx));
        }
        self.bar_update(window, cx);
        self.video_tick(window, cx);
        if self.media.borrow_mut().audio.tick() || self.rec.animating() {
            window.request_animation_frame();
        }
        {
            let mut media = self.media.borrow_mut();
            media.videos.scale = window.scale_factor();
            if media.videos.tick() {
                window.request_animation_frame();
            }
            for old in media.videos.retired.drain(..) {
                let _ = window.drop_image(old);
            }
            media.models.scale = window.scale_factor();
            media.models.active = window.is_window_active();
            media.models.front = self.model_viewer.is_some();
            if media.models.tick() {
                window.request_animation_frame();
            }
            for old in media.models.retired.drain(..) {
                let _ = window.drop_image(old);
            }
        }
        if let Some(s) = self.scanner.as_mut() {
            self.retired_frames.append(&mut s.retired);
        }
        for old in self.retired_frames.drain(..) {
            let _ = window.drop_image(old);
        }
        if self.store.borrow().anyone_typing() {
            window.request_animation_frame();
        }
        self.revoke_outlived_archives();
        if self.mini_animating() || self.pivx_animating() || self.archive.animating() || self.nexus.as_ref().is_some_and(|n| n.animating()) {
            window.request_animation_frame();
        }
        let popout = self.popout_view(window, cx);
        let launch = self.launch_view(cx);
        let pick = self.nexus_picker_view(cx);
        let perm = self.perm_prompt_view(cx);
        let publish = self.publish_view(window, cx);
        let pivx = self.pivx_dialog_view(window, cx);
        match self.rec.due() {
            Some(crate::recorder::Due::Locked) => self.rec.locked(),
            Some(crate::recorder::Due::Idle) => self.rec.idle(),
            None => {}
        }
        self.send_shown = !self.input.read(cx).text().trim().is_empty();
        self.sync_wallpaper(cx);
        if self.wallpaper.moving() {
            window.request_animation_frame();
        }
        if (self.shortcode.is_some()
            || self.mention.is_some()
            || self.time_pick.is_some()
            || self.time_chip.is_some())
            && !self.input.focus_handle(cx).is_focused(window)
        {
            self.close_popups(cx);
        }
        if matches!(self.screen, Screen::Main)
            && let Some(a) = self.pending_link.take()
        {
            cx.defer_in(window, move |app, window, cx| {
                app.open_deep_link(a, window, cx)
            });
        }
        self.scan_invites(cx);
        let root = div()
            .capture_key_down(cx.listener(|app, e: &gpui::KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape"
                    && app.menu.is_none()
                    && app.popup.is_none()
                    && app.channel_editor.is_some()
                {
                    app.on_channel_editor(crate::chaneditor::Act::Close, window, cx);
                    cx.stop_propagation();
                    return;
                }
                if e.keystroke.key == "escape" && (app.escape_overlay() || app.nexus_escape(window, cx)) {
                    cx.stop_propagation();
                    cx.notify();
                }
                if app.voice_key(e, cx) {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(c(APP_BG))
            .font_family(INTER)
            .text_color(c(TEXT))
            .text_size(px(16.))
            .line_height(px(24.));
        match &self.screen {
            Screen::Booting => {
                return root
                    .child(div().absolute().inset_0().child(chrome(window, true, false)))
                    .children(self.downgrade.as_ref().map(|(now, needs)| crate::downgrade::render(now, needs)));
            }
            Screen::Login(view) => {
                return root
                    .child(div().absolute().inset_0().child(view.clone()))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .child(chrome(window, true, false)),
                    )
                    .child(self.security.clone())
                    .children(self.popup.as_ref().map(|(p, _)| {
                        let app = cx.entity().downgrade();
                        gpui::deferred(kit::popup(p, move |yes, window, cx| {
                            app.update(cx, |app, cx| app.answer_popup(yes, window, cx)).ok();
                        }))
                        .with_priority(15)
                    }));
            }
            Screen::Main => {}
        }
        if let Some(view) = &self.reauth {
            return root
                .child(div().absolute().inset_0().child(view.clone()))
                .child(div().absolute().top_0().left_0().right_0().child(chrome(window, true, false)));
        }
        let store = self.store.clone();
        let store = store.borrow();
        let no_messages = !store
            .chats
            .iter()
            .any(|c| store.listed(c) && !store.is_request(c));
        if no_messages != self.viktor.is_some() {
            self.viktor = no_messages.then(|| {
                static GREETED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                let greet = !GREETED.swap(true, std::sync::atomic::Ordering::Relaxed);
                cx.new(|cx| crate::viktor::Viktor::new(146., greet, cx))
            });
        }
        let net_dialog = self
            .settings
            .as_ref()
            .and_then(|s| s.dialog.as_ref().map(|d| (d, &s.book)))
            .map(|(d, book)| {
                let app = cx.entity().downgrade();
                let on: crate::netdialog::OnAct = std::rc::Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_net(act, window, cx)).ok();
                });
                crate::netdialog::view(d, book, &on, window, cx)
            });
        let popup = self.popup.as_ref().map(|(p, _)| {
            let app = cx.entity().downgrade();
            kit::popup(p, move |yes, window, cx| {
                app.update(cx, |app, cx| app.answer_popup(yes, window, cx))
                    .ok();
            })
        });
        let on_community: crate::community::OnAct = {
            let app = cx.entity().downgrade();
            Rc::new(move |act, window, cx| {
                app.update(cx, |app, cx| app.on_community(act, window, cx))
                    .ok();
            })
        };
        let root = root.on_drop(cx.listener(|app, paths: &gpui::ExternalPaths, window, cx| {
            app.open_paths_preview(paths.paths().to_vec(), window, cx);
        }));
        let net_info = self.net_info.map(|kind| {
            let (a, b) = (cx.entity().downgrade(), cx.entity().downgrade());
            crate::netinfo::modal(
                kind,
                true,
                crate::netinfo::Extras { narrow: self.vw <= 480., ..Default::default() },
                move |_, cx| {
                    a.update(cx, |app, cx| {
                        app.net_info = None;
                        cx.notify();
                    })
                    .ok();
                },
                move |key, w, cx| {
                    b.update(cx, |app, cx| app.net_act(crate::routing::NetAct::Link(key), w, cx)).ok();
                },
            )
        });
        root.children(net_dialog)
            .children(net_info.map(|p| gpui::deferred(p).with_priority(14)))
            .children(popup.map(|p| gpui::deferred(p).with_priority(15)))
            .children(self.rekey.as_ref().map(|r| {
                if r.animating() {
                    window.request_animation_frame();
                }
                gpui::deferred(crate::rekey::view(r)).with_priority(16)
            }))
            .child(self.security.clone())
            .child(chrome(window, false, rail_mode.collapsed && !slim).children(self.update_banner(cx)))
            .child(if slim {
                self.slim_main(&store, &on_community, window, cx)
            } else {
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(rail(
                        &store,
                        &self.rail_open,
                        self.open.as_ref(),
                        self.community.as_ref().map(|v| v.id.as_str()),
                        &store.communities.iter().filter_map(|c| Some((c.community_id.clone(), self.voice.badge(&store, &c.community_id)?))).collect(),
                        rail_mode,
                        &on_community,
                        {
                            let app = cx.entity().downgrade();
                            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                                app.update(cx, |app, cx| {
                                    app.toggle_start(window, cx);
                                })
                                .ok();
                            }
                        },
                        cx.listener(|app, _: &gpui::ClickEvent, _, cx| {
                            app.rail_pref = !app.rail_pref;
                            backend::device::set("ws_rail_collapsed", app.rail_pref.to_string());
                            cx.notify();
                        }),
                    ))
                    .when_some(self.settings.as_ref(), |row, settings| {
                        let app = cx.entity().downgrade();
                        let on: crate::settings::OnAct = Rc::new(move |act, window, cx| {
                            app.update(cx, |app, cx| app.on_settings(act, window, cx))
                                .ok();
                        });
                        let width = f32::from(window.viewport_size().width) - rail_mode.width();
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .child(crate::settings::view(settings, width, &on, cx)),
                        )
                    })
                    .when(self.settings.is_none() && self.nexus.is_some(), |row| {
                        let width = f32::from(window.viewport_size().width) - rail_mode.width();
                        row.child(div().flex_1().min_w_0().h_full().children(self.nexus_view(width, cx)))
                    })
                    .when(self.settings.is_none() && self.nexus.is_none(), |row| {
                        match self
                            .community
                            .as_ref()
                            .and_then(|v| Some((v, store.community(&v.id)?)))
                        {
                            Some((view, com)) => row.child(crate::community::sidebar(
                                &store,
                                view,
                                com,
                                self.open.as_deref(),
                                self.raid
                                    .get(&com.community_id)
                                    .is_some_and(|r| r.0 || r.1 > 0),
                                &on_community,
                                (&self.voice, &self.on_voice_handler(cx), &self.anchors),
                                LIST_W,
                                false,
                            )),
                            None => row.child(chat_list(
                                self,
                                &self.store,
                                &store,
                                self.open.clone().or_else(|| self.profile.as_ref().and_then(|p| p.back.clone())),
                                (self.requests_tab, self.tab_slid, no_messages),
                                cx,
                            )),
                        }
                    })
                    .when(self.settings.is_none() && self.nexus.is_none(), |row| {
                        row.child(match (&self.profile, self.open.clone()) {
                            _ if self.create_community.is_some() => {
                                let app = cx.entity().downgrade();
                                let on: crate::createcommunity::OnAct =
                                    Rc::new(move |act, window, cx| {
                                        app.update(cx, |app, cx| {
                                            app.on_create_community(act, window, cx)
                                        })
                                        .ok();
                                    });
                                let width =
                                    f32::from(window.viewport_size().width) - rail_mode.width() - LIST_W;
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(crate::createcommunity::view(
                                        self.create_community.as_ref().unwrap(),
                                        &store,
                                        width,
                                        &on,
                                        cx,
                                    ))
                                    .into_any_element()
                            }
                            _ if self.new_chat.is_some() => {
                                let app = cx.entity().downgrade();
                                let on: crate::newchat::OnAct = Rc::new(move |act, window, cx| {
                                    app.update(cx, |app, cx| app.on_new_chat(act, window, cx))
                                        .ok();
                                });
                                let width =
                                    f32::from(window.viewport_size().width) - rail_mode.width() - LIST_W;
                                let nc = self.new_chat.as_ref().unwrap();
                                let invite = crate::newchat::invite_key(nc.filter.read(cx).text())
                                    .map(|k| {
                                        self.invite_previews
                                            .get(&k)
                                            .cloned()
                                            .unwrap_or(crate::newchat::Preview::Loading)
                                    })
                                    .or_else(|| {
                                        crate::newchat::go_link(nc.filter.read(cx).text())
                                            .map(crate::newchat::Preview::Go)
                                    });
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(crate::newchat::view(
                                        nc,
                                        &store,
                                        width,
                                        invite.as_ref(),
                                        &on,
                                        cx,
                                    ))
                                    .into_any_element()
                            }
                            (Some(p), _) => {
                                let app = cx.entity().downgrade();
                                let on: crate::profile::OnAct = Rc::new(move |act, window, cx| {
                                    app.update(cx, |app, cx| app.on_profile(act, window, cx))
                                        .ok();
                                });
                                crate::profile::pane(&store, p, &on, window, cx)
                            }
                            (None, Some(id)) => match self
                                .community
                                .as_ref()
                                .filter(|v| v.panel)
                                .and_then(|v| Some((v, store.community(&v.id)?)))
                            {
                                Some((view, com)) => div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .flex()
                                    .child(self.chat_pane(&store, id.clone(), &on_community, window, cx))
                                    .child(crate::community::members_panel(
                                        &store,
                                        &id,
                                        view,
                                        com,
                                        &on_community,
                                        &self.anchors,
                                        cx,
                                    ))
                                    .into_any_element(),
                                None => self
                                    .chat_pane(&store, id, &on_community, window, cx)
                                    .into_any_element(),
                            },
                            (None, None) => empty_pane(self.viktor.clone()).into_any_element(),
                        })
                    })
                .into_any_element()
            })
            .children(self.switcher.as_ref().filter(|_| slim).map(|s| {
                let app = cx.entity().downgrade();
                let on: crate::switcher::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_switcher(act, window, cx)).ok();
                });
                let vp = window.viewport_size();
                let hang = self.profile.is_none();
                let (top, left) = if hang { (CHROME_H + 52., 8.) } else { (CHROME_H + 60., 0.) };
                crate::switcher::view(s, top, left, (f32::from(vp.width), f32::from(vp.height)), hang, &on)
            }))
            .children(
                self.switcher
                    .as_ref()
                    .filter(|_| self.profile.is_some() && !slim)
                    .map(|s| {
                        let app = cx.entity().downgrade();
                        let on: crate::switcher::OnAct = Rc::new(move |act, window, cx| {
                            app.update(cx, |app, cx| app.on_switcher(act, window, cx))
                                .ok();
                        });
                        crate::switcher::view(
                            s,
                            CHROME_H + 60.,
                            rail_mode.width() + LIST_W,
                            (f32::from(window.viewport_size().width), f32::from(window.viewport_size().height)),
                            false,
                            &on,
                        )
                    }),
            )
            .children(self.status_view(&store, window, cx))
            .children(self.qr_view(cx))
            .children(self.qr_scanner_view(window, cx))
            .children(self.badge_card.as_ref().map(|card| {
                if card.opened.elapsed().as_secs_f32() < 0.45 {
                    window.request_animation_frame();
                }
                let app = cx.entity().downgrade();
                let size = window.viewport_size();
                gpui::deferred(crate::badgecard::view(
                    card,
                    window,
                    cx,
                    move |ev, window, cx| {
                        app.update(cx, |app, cx| {
                            use crate::badgecard::Ev;
                            match ev {
                                Ev::Close => app.badge_card = None,
                                Ev::Pointer(p) => {
                                    if let Some(card) = app.badge_card.as_mut() {
                                        let x =
                                            (f32::from(p.x) - f32::from(size.width) / 2.) / 179.;
                                        let y =
                                            (f32::from(p.y) - f32::from(size.height) / 2.) / 230.;
                                        card.tilt = gpui::point(x.clamp(-1., 1.), y.clamp(-1., 1.));
                                    }
                                }
                                Ev::Leave => {
                                    if let Some(card) = app.badge_card.as_mut() {
                                        card.tilt = gpui::Point::default();
                                    }
                                }
                            }
                            let _ = window;
                            cx.notify();
                        })
                        .ok();
                    },
                ))
                .with_priority(12)
            }))
            .children(self.pack_details_view(f32::from(window.viewport_size().width) <= 480., cx))
            .children(popout)
            .children(launch)
            .children(pick)
            .children(publish)
            .children(pivx)
            .children(perm)
            .children(self.toast_view(window))
            .children(
                self.viewer
                    .clone()
                    .map(|v| gpui::deferred(div().absolute().inset_0().child(v)).with_priority(10)),
            )
            .children(
                self.model_viewer
                    .as_ref()
                    .map(|(v, _, _)| gpui::deferred(div().absolute().inset_0().child(v.clone())).with_priority(10)),
            )
            .children(
                self.file_preview
                    .clone()
                    .map(|v| gpui::deferred(div().absolute().inset_0().child(v)).with_priority(9)),
            )
            .children(self.cropper.as_ref().map(|(v, _)| {
                gpui::deferred(div().absolute().inset_0().child(v.clone())).with_priority(11)
            }))
            .children(self.pack_crop.as_ref().map(|(v, _)| {
                gpui::deferred(div().absolute().inset_0().child(v.clone())).with_priority(11)
            }))
            .children(self.comm_crop.as_ref().map(|(v, _, _)| {
                gpui::deferred(div().absolute().inset_0().child(v.clone())).with_priority(14)
            }))
            .children({
                let app = cx.entity().downgrade();
                let on: crate::call::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_call(act, window, cx)).ok();
                });
                let a = self.call_audio.unwrap_or_default();
                let proc_ = crate::call::Processing {
                    auto_gain: a.auto_gain,
                    echo_cancel: a.echo_cancel,
                    noise_suppress: a.noise_suppress,
                };
                crate::call::render(
                    &self.call,
                    &self.store.borrow(),
                    &proc_,
                    &self.call_seen,
                    window.viewport_size(),
                    CHROME_H,
                    &on,
                )
            })
            .children(self.screen_pick.as_ref().map(|p| {
                let app = cx.entity().downgrade();
                let on: crate::screenpick::OnAct = Rc::new(move |act, _, cx| {
                    app.update(cx, |app, cx| app.on_screen_pick(act, cx)).ok();
                });
                crate::screenpick::render(p, window.viewport_size(), &on)
            }))
            .children(self.edit_history.as_ref().map(|h| {
                let app = cx.entity().downgrade();
                let close: crate::edithistory::OnClose = Rc::new(move |_, cx| {
                    app.update(cx, |app, cx| {
                        app.edit_history = None;
                        cx.notify();
                    })
                    .ok();
                });
                crate::edithistory::render(h, &store, &self.media, window.viewport_size(), &close)
            }))
            .children(self.reaction_popups(&store, window, cx))
            .children(self.secret_guard.as_ref().map(|g| {
                let app = cx.entity().downgrade();
                let on: crate::secretguard::OnAnswer = Rc::new(move |a, window, cx| {
                    app.update(cx, |app, cx| app.secret_answer(a, window, cx)).ok();
                });
                crate::secretguard::render(g, &on)
            }))
            .children(self.mini_profile.as_ref().map(|m| {
                let app = cx.entity().downgrade();
                let on: crate::miniprofile::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_mini(act, window, cx)).ok();
                });
                crate::miniprofile::render(m, &store, window.viewport_size(), &self.anchors, &on)
            }))
            .children({
                // The call's own stage on screen carries the same controls.
                let on_stage = self.voice.session.as_ref().is_some_and(|s| self.open.as_deref() == Some(s.channel_id.as_str()));
                (self.call.id.is_none() && !on_stage)
                    .then(|| {
                        crate::voice::bar(&self.voice, &store, self.call.pos, CHROME_H + 10., &self.on_voice_handler(cx))
                            .map(|bar| gpui::deferred(div().absolute().inset_0().child(bar)).with_priority(7))
                    })
                    .flatten()
            })
            .children(
                self.invite_details
                    .as_ref()
                    .and_then(|id| store.invites.iter().find(|i| &i.community_id == id))
                    .map(|inv| {
                        let (a, b) = (inv.community_id.clone(), inv.community_id.clone());
                        crate::community::invite_details(
                            &store,
                            inv,
                            Box::new(cx.listener(|app, _, _, cx| {
                                app.invite_details = None;
                                cx.notify();
                            })),
                            Box::new(cx.listener(move |app, _, window, cx| {
                                app.answer_invite(a.clone(), true, window, cx)
                            })),
                            Box::new(cx.listener(move |app, _, window, cx| {
                                app.answer_invite(b.clone(), false, window, cx)
                            })),
                        )
                    }),
            )
            .children(self.embed_modal.as_ref().map(|m| {
                let app = cx.entity().downgrade();
                let send: crate::embeds::Send = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_embed(act, window, cx)).ok();
                });
                let env = crate::embeds::Env {
                    store: &store,
                    media: &self.media,
                    clock: self.started.elapsed().as_secs_f32(),
                    send,
                    modal: true,
                };
                let el = crate::embeds::modal(m, &env, f32::from(window.viewport_size().height));
                let quoted = std::mem::take(&mut self.media.borrow_mut().embeds.queue);
                if !quoted.is_empty() {
                    let app = cx.entity().downgrade();
                    cx.defer(move |cx| {
                        app.update(cx, |app, cx| app.fetch_embeds(quoted, cx)).ok();
                    });
                }
                el
            }))
            .children(self.invite.as_ref().map(|m| {
                let app = cx.entity().downgrade();
                let on: crate::invite::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_invite(act, window, cx))
                        .ok();
                });
                let filter = m.filter.read(cx).text().to_string();
                crate::invite::view(m, &store, &filter, (f32::from(window.viewport_size().width), f32::from(window.viewport_size().height)), &on)
            }))
            .children(self.comm_settings.as_ref().map(|m| {
                let compact = f32::from(window.viewport_size().width) <= 760.;
                let app = cx.entity().downgrade();
                let on: crate::commsettings::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_comm_settings(act, window, cx))
                        .ok();
                });
                crate::commsettings::view(m, &store, compact, &on, cx)
            }))
            .children(self.channel_editor.as_ref().map(|m| {
                let app = cx.entity().downgrade();
                let on: crate::chaneditor::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_channel_editor(act, window, cx))
                        .ok();
                });
                crate::chaneditor::view(m, &store, f32::from(window.viewport_size().width), &on, cx)
            }))
            .children(self.moderation.as_ref().map(|m| {
                let app = cx.entity().downgrade();
                let on: crate::moderation::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_moderation(act, window, cx))
                        .ok();
                });
                crate::moderation::view(m, &store, f32::from(window.viewport_size().width), &on, cx)
            }))
            .children(self.create_channel.as_ref().map(|m| {
                let app = cx.entity().downgrade();
                let on: crate::createchannel::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_create_channel(act, window, cx))
                        .ok();
                });
                crate::createchannel::view(m, &on, window, cx)
            }))
            .children(self.start.as_ref().map(|sm| {
                let app = cx.entity().downgrade();
                let on: crate::startmenu::OnAct = Rc::new(move |act, window, cx| {
                    app.update(cx, |app, cx| app.on_start(act, window, cx)).ok();
                });
                gpui::deferred(div().absolute().inset_0().child(crate::startmenu::render(
                    sm,
                    &self.store.borrow(),
                    window,
                    &on,
                    cx,
                )))
                .with_priority(8)
            }))
            .children(self.menu.as_ref().map(|(at, items, _)| {
                let pick = cx.entity().downgrade();
                let close = pick.clone();
                let slide = pick.clone();
                kit::context_menu(
                    *at,
                    items,
                    move |ix, window, cx| {
                        pick.update(cx, |app, cx| app.on_menu(ix, window, cx)).ok();
                    },
                    move |ix, v, _, cx| {
                        slide.update(cx, |app, cx| app.on_menu_slide(ix, v, cx)).ok();
                    },
                    self.menu_sliding.clone(),
                    move |_, cx| {
                        close
                            .update(cx, |app, cx| {
                                app.menu = None;
                                cx.notify();
                            })
                            .ok();
                    },
                )
            }))
    }
}

impl VectorApp {
    /// `.chrome-update`: a find is announced in the title strip until it is acted on.
    fn update_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let version = match (&self.updates.phase, &self.updates.found) {
            (crate::settings::UpdPhase::Available, Some((v, _))) => v.clone(),
            _ => return None,
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("chrome-update")
                        .h_full()
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .text_size(px(13.))
                        .text_color(c(0x33db98))
                        .cursor_pointer()
                        .hover(|d| d.underline())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_settings("updates", window, cx)
                        }))
                        .child(format!(
                            "New Vector Update Available ({version}). Download Now"
                        )),
                ),
        )
    }
}

// ── #window-chrome ──────────────────────────────────────────────────────────────────────

/// The title strip; over the login screens it is only its controls, on the same art.
fn chrome(window: &Window, login: bool, mark_only: bool) -> gpui::Div {
    let maximized = window.is_maximized();
    let ctl = |id: &'static str, icon: &'static str, area: WindowControlArea, hover: u32| {
        div()
            .id(id)
            .w(px(37.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(white_a(0.7))
            .hover(move |s| s.bg(c(hover)).text_color(c(0xffffff)))
            .window_control_area(area)
            .child(svg().path(icon).size(px(10.)).text_color(white_a(0.7)))
    };
    div()
        .relative()
        .h(px(CHROME_H))
        .flex_shrink_0()
        .flex()
        .items_center()
        .when(!login, |d| {
            d.bg(c(CHROME_BG)).border_b_1().border_color(c(DIVIDER))
        })
        .child(
            div()
                .id("chrome-drag")
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag)
                .on_mouse_down(MouseButton::Left, |e, window, _| {
                    if e.click_count == 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
                .when(!login, |d| {
                    d.child(
                        div()
                            .relative()
                            .child(
                                img("ui/logo-mark.svg")
                                    .absolute()
                                    .left(px(12.))
                                    .top(px(5.5))
                                    .w(px(12.))
                                    .h(px(20.)),
                            )
                            .when(!mark_only, |d| d.child(
                                img("ui/logo-typeface.svg")
                                    .absolute()
                                    .left(px(32.))
                                    .top(px(9.5))
                                    .w(px(92.))
                                    .h(px(12.)),
                            )),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .h_full()
                .child(
                    div()
                        .w(px(37.))
                        .mr(px(8.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            svg()
                                .path("ui/chrome-help.svg")
                                .size(px(16.))
                                .text_color(c(CHROME_HELP)),
                        ),
                )
                .child(ctl(
                    "chrome-min",
                    "ui/chrome-min.svg",
                    WindowControlArea::Min,
                    HOVER,
                ))
                .child(ctl(
                    "chrome-max",
                    if maximized {
                        "ui/chrome-restore.svg"
                    } else {
                        "ui/chrome-max.svg"
                    },
                    WindowControlArea::Max,
                    HOVER,
                ))
                .child(ctl(
                    "chrome-close",
                    "ui/chrome-close.svg",
                    WindowControlArea::Close,
                    0xe81123,
                )),
        )
}

// ── #navbar (widescreen rail) ────────────────────────────────────────────────────────────

pub(crate) fn avatar(store: &Store, npub: &str, size: f32) -> AnyElement {
    match store.avatar_path(npub) {
        Some(path) => img(path)
            .size(px(size))
            .rounded_full()
            .flex_shrink_0()
            .into_any_element(),
        None => img("icons/user-placeholder.svg")
            .size(px(size))
            .rounded_full()
            .flex_shrink_0()
            .into_any_element(),
    }
}

fn rail_label(text: &'static str) -> impl IntoElement {
    // .ws-rail-group-label
    div()
        .h(px(24.))
        .px(px(16.))
        .font_family(RUBIK)
        .text_size(px(15.))
        .text_color(white_a(0.38))
        .child(text)
}

/// A corner dot cut out of what it sits on by a ring in the rail's colour.
fn slim_badge(color: gpui::Hsla) -> gpui::Div {
    div()
        .absolute()
        .size(px(14.))
        .rounded_full()
        .bg(c(0x090909))
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(10.)).rounded_full().bg(color))
}

/// `(collapsed, locked)`: whether the rail is down to tiles, and whether the window forced it.
#[derive(Clone, Copy)]
struct RailMode {
    collapsed: bool,
    locked: bool,
}

impl RailMode {
    fn width(self) -> f32 {
        if self.collapsed { RAIL_SLIM_W } else { RAIL_W }
    }
}

#[allow(clippy::too_many_arguments)]
fn rail(
    store: &Store,
    folders: &HashSet<String>,
    open: Option<&SharedString>,
    active: Option<&str>,
    voice: &HashMap<String, bool>,
    mode: RailMode,
    on_c: &crate::community::OnAct,
    on_me: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    on_collapse: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let slim = mode.collapsed;
    let unread: Vec<&SerializableChat> = store
        .chats
        .iter()
        .filter(|c| {
            store.listed(c)
                && store.unread_count(c) > 0
                && open.is_none_or(|o| *o != c.id)
                && !store.is_request(c)
        })
        .take(3)
        .collect();
    let any_unread = store
        .chats
        .iter()
        .any(|c| store.listed(c) && store.unread_count(c) > 0);

    // .ws-rail-messages.is-home
    let home = active.is_none();
    let messages = if slim {
        let on = on_c.clone();
        div()
            .h(px(61.))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(c(DIVIDER))
            .child(
                div()
                    .id("ws-rail-messages")
                    .relative()
                    .size(px(45.))
                    .mx_auto()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!home, |d| d.cursor_pointer())
                    .on_click(move |_, window, cx| on(crate::community::Act::Home, window, cx))
                    .on_mouse_down(gpui::MouseButton::Right, {
                        let on = on_c.clone();
                        move |e, window, cx| {
                            cx.stop_propagation();
                            on(crate::community::Act::MessagesMenu(e.position), window, cx)
                        }
                    })
                    .child(svg().path("ui/rail-mail.svg").w(px(33.)).h(px(27.)).text_color(c(0xffffff)))
                    .when(any_unread, |d| d.child(slim_badge(c(PRIMARY)).top(px(2.)).left(px(32.)))),
            )
    } else { div().pt(px(16.)).px(px(8.)).child(
        div()
            .id("ws-rail-messages")
            .h(px(40.))
            .rounded(px(6.))
            .when(home, |d| d.bg(c(RAIL_TILE)))
            .when(!home, |d| {
                d.cursor_pointer().hover(|d| d.bg(white_a(0.045)))
            })
            .on_click({
                let on = on_c.clone();
                move |_, window, cx| on(crate::community::Act::Home, window, cx)
            })
            .on_mouse_down(gpui::MouseButton::Right, {
                let on = on_c.clone();
                move |e, window, cx| {
                    cx.stop_propagation();
                    on(crate::community::Act::MessagesMenu(e.position), window, cx)
                }
            })
            .flex()
            .items_center()
            .justify_center()
            .gap(px(11.))
            .child(
                svg()
                    .path("ui/rail-mail.svg")
                    .w(px(25.))
                    .h(px(20.5))
                    .text_color(c(0xffffff)),
            )
            .child(
                div()
                    .relative()
                    .font_family(RUBIK)
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(14.))
                    .text_color(c(0xffffff))
                    .child("Messages")
                    .when(any_unread, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(67.4))
                                .top(px(-1.))
                                .size(px(8.))
                                .rounded_full()
                                .bg(c(PRIMARY)),
                        )
                    }),
            ),
    ) };

    // #ws-rail-dms
    let dms_slim = (slim && !unread.is_empty()).then(|| {
        div().mt(px(6.)).flex().flex_col().gap(px(8.)).children(unread.iter().map(|chat| {
            let id = chat.id.clone();
            let on = on_c.clone();
            div()
                .id(SharedString::from(format!("rail-{}", chat.id)))
                .relative()
                .size(px(45.))
                .mx_auto()
                .rounded_full()
                .cursor_pointer()
                .on_click(move |_, w, cx| on(crate::community::Act::Dm(id.clone()), w, cx))
                .child(avatar(store, &chat.id, 45.))
                .child(slim_badge(white_a(0.9)).top(px(-2.)).right(px(-2.)))
        }))
    });
    let dms = (!slim && !unread.is_empty()).then(|| {
        div()
            .mt(px(10.))
            .child(rail_label("New Messages"))
            .children(unread.iter().map(|chat| {
                let (id, on) = (chat.id.clone(), on_c.clone());
                div()
                    .id(SharedString::from(format!("rail-{}", chat.id)))
                    .cursor_pointer()
                    .hover(|d| d.bg(white_a(0.045)))
                    .on_click(move |_, w, cx| on(crate::community::Act::Dm(id.clone()), w, cx))
                    .mx(px(8.))
                    .h(px(31.))
                    .rounded(px(10.))
                    .flex()
                    .items_center()
                    .px(px(8.))
                    .gap(px(10.))
                    .child(avatar(store, &chat.id, 26.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(RUBIK)
                            .text_size(px(14.))
                            .text_color(c(0xffffff))
                            .child(store.name(&chat.id)),
                    )
                    .child(
                        div()
                            .size(px(7.))
                            .mr(px(5.))
                            .rounded_full()
                            .bg(white_a(0.32)),
                    )
            }))
    });

    // #ws-rail-spaces
    let spaces = if slim {
        let on = on_c.clone();
        div()
            .mt(px(6.))
            .when(dms_slim.is_some(), |d| {
                d.child(div().pt(px(6.)).child(div().mx(px(16.)).h(px(1.)).bg(c(DIVIDER))))
            })
            .child(crate::rail::render_slim(store, &store.rail, folders, active, RAIL_SLIM_W, voice, on_c))
            .child(
                div()
                    .id("rail-create-community")
                    .size(px(45.))
                    .mx_auto()
                    .rounded(px(8.5))
                    .border_1()
                    .border_dashed()
                    .border_color(white_a(0.22))
                    .cursor_pointer()
                    .hover(|d| d.bg(white_a(0.04)))
                    .on_click(move |_, w, cx| on(crate::community::Act::Create, w, cx))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path("ui/plus-thin.svg").size(px(20.)).text_color(c(PRIMARY))),
            )
    } else { div()
        .mt(px(10.))
        .pt(px(6.))
        .child(div().mx(px(16.)).h(px(1.)).bg(c(DIVIDER)))
        .child(div().pt(px(16.)).child(rail_label("Communities")))
        .child(crate::rail::render(store, &store.rail, folders, active, voice, on_c))
        .child(
            div()
                .id("rail-create-community")
                .cursor_pointer()
                .hover(|d| d.bg(white_a(0.04)))
                .on_click({
                    let on = on_c.clone();
                    move |_, w, cx| on(crate::community::Act::Create, w, cx)
                })
                .mx(px(8.))
                .h(px(31.))
                .rounded(px(10.))
                .flex()
                .items_center()
                .px(px(8.))
                .gap(px(10.))
                .child(
                    div()
                        .size(px(26.))
                        .rounded(px(4.9))
                        .border_1()
                        .border_dashed()
                        .border_color(white_a(0.22))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            svg()
                                .path("ui/plus-thin.svg")
                                .size(px(14.))
                                .text_color(c(PRIMARY)),
                        ),
                )
                .child(
                    div()
                        .font_family(RUBIK)
                        .text_size(px(14.))
                        .text_color(white_a(0.55))
                        .child("Create Community"),
                ),
        ) };

    let on_me = Rc::new(on_me);
    let (on_me1, on_me2) = (on_me.clone(), on_me);
    let me = store.me();
    let my_name = me.map(|p| store.name(&p.id)).unwrap_or_default();
    let status = me.map(|p| p.status.title.clone()).filter(|t| !t.is_empty());

    div()
        .w(px(mode.width()))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(RAIL_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            // #ws-rail-shortcuts
            div()
                .flex_1()
                .min_h_0()
                .border_b_1()
                .border_color(c(DIVIDER))
                .child(messages)
                .children(dms)
                .children(dms_slim)
                .child(spaces),
        )
        .when(mode.locked, |d| d.child(div().mt(px(10.)).mb(px(2.))))
        .when(!mode.locked, |d| {
            // #ws-rail-collapse
            d.child(
                div()
                    .id("ws-rail-collapse")
                    .tooltip(crate::kit::tooltip(if slim { "Expand the sidebar" } else { "Collapse the sidebar" }))
                    .mt(px(10.))
                    .mb(px(2.))
                    .when(slim, |d| d.w(px(40.)).mx_auto().rounded(px(12.)).justify_center())
                    .when(!slim, |d| d.mx(px(8.)).rounded(px(6.)))
                    .h(px(40.))
                    .opacity(0.4)
                    .hover(move |d| {
                        let d = d.opacity(1.);
                        if slim { d } else { d.bg(white_a(0.045)) }
                    })
                    .cursor_pointer()
                    .on_click(on_collapse)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .when(!slim, |d| d.ml(px(14.)))
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path("icons/chevron-double-left.svg")
                                    .size(px(28.))
                                    .text_color(c(0xffffff))
                                    .when(slim, |s| {
                                        s.with_transformation(gpui::Transformation::rotate(gpui::radians(
                                            std::f32::consts::PI,
                                        )))
                                    }),
                            ),
                    )
                    .when(!slim, |d| {
                        d.child(
                            div()
                                .ml(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(px(14.))
                                .text_color(c(TEXT))
                                .child("Collapse"),
                        )
                    }),
            )
        })
        .when(slim, |d| {
            // #ws-rail-account, collapsed: the avatar alone, where the expanded chip puts it.
            d.child(
                div().pt(px(8.)).pb(px(8.)).child(
                    div()
                        .id("ws-rail-account")
                        .cursor_pointer()
                        .on_click(move |e, w, cx| on_me1(e, w, cx))
                        .w(px(40.))
                        .h(px(54.))
                        .mx_auto()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(avatar(store, &store.my_npub, 38.)),
                ),
            )
        })
        .when(!slim, |d| d.child(
            // #ws-rail-account .account
            div().p(px(8.)).pt(px(8.)).child(
                div()
                    .id("ws-rail-account")
                    .cursor_pointer()
                    .on_click(move |e, w, cx| on_me2(e, w, cx))
                    .h(px(54.))
                    .rounded(px(12.))
                    .bg(c(RAIL_TILE))
                    .flex()
                    .items_center()
                    .px(px(9.))
                    .gap(px(10.))
                    .child(avatar(store, &store.my_npub, 38.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::BOLD)
                                    .text_size(px(14.))
                                    .line_height(px(19.))
                                    .text_color(c(0xffffff))
                                    .child(my_name),
                            )
                            .child(
                                div()
                                    .mt(px(1.5))
                                    .truncate()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .text_color(white_a(0.4))
                                    .child(status.unwrap_or_else(|| "Set a Status".into())),
                            ),
                    ),
            ),
        ))
}

// ── #chats ──────────────────────────────────────────────────────────────────────────────

fn chat_list(
    app: &VectorApp,
    shared: &Shared,
    store: &Store,
    open: Option<SharedString>,
    (requests, slid, no_messages): (bool, bool, bool),
    cx: &mut Context<VectorApp>,
) -> impl IntoElement {
    let (mut dot_messages, mut dot_requests) = (false, false);
    let mut ids: Vec<usize> = Vec::new();
    for (i, chat) in store.chats.iter().enumerate() {
        if !store.listed(chat) {
            continue;
        }
        let request = store.is_request(chat);
        if store.unread_count(chat) > 0 {
            if request {
                dot_requests = true;
            } else {
                dot_messages = true;
            }
        }
        if request == requests {
            ids.push(i);
        }
    }
    fn pick(
        to_requests: bool,
        cx: &mut Context<VectorApp>,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |app, _: &gpui::ClickEvent, _, cx| {
            app.requests_tab = to_requests;
            app.tab_slid = true;
            cx.notify();
        })
    }
    let (to_messages, to_requests) = (pick(false, cx), pick(true, cx));
    let tab = |label: &'static str, on: bool, dot: bool, click| {
        div()
            .id(label)
            .flex_1()
            .h(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .font_family(RUBIK)
            .text_size(px(14.))
            .font_weight(if on {
                FontWeight::BOLD
            } else {
                FontWeight::MEDIUM
            })
            .text_color(if on { c(0xffffff) } else { white_a(0.38) })
            .when(!on, |d| {
                d.cursor_pointer()
                    .hover(|s| s.text_color(white_a(0.75)))
                    .on_click(click)
            })
            .child(div().relative().child(label).when(dot && !on, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(-1.))
                        .right(px(-9.))
                        .size(px(8.))
                        .rounded_full()
                        .bg(c(ACCENT)),
                )
            }))
    };
    // .cc-tab-bar: slides between the halves, but never on the first paint.
    let half = (LIST_W - 23.) / 2.;
    let to = if requests { half } else { 0. };
    let bar = div()
        .absolute()
        .bottom(px(-1.))
        .w(px(half))
        .h(px(2.))
        .bg(c(PRIMARY));
    let bar = if slid {
        bar.with_animation(
            SharedString::from(format!("cc-tab-bar-{requests}")),
            gpui::Animation::new(std::time::Duration::from_millis(240))
                .with_easing(crate::kit::bezier(0.2, 0.8, 0.2, 1.)),
            move |d, t| d.left(px(half - to + (2. * to - half) * t)),
        )
        .into_any_element()
    } else {
        bar.left(px(to)).into_any_element()
    };
    let invites: Vec<AnyElement> = if requests {
        store
            .invites
            .iter()
            .filter(|i| {
                !store
                    .profiles
                    .get(i.inviter.as_str())
                    .is_some_and(|p| p.is_blocked)
            })
            .map(|inv| {
                let id = inv.community_id.clone();
                let (a, b, d) = (id.clone(), id.clone(), id);
                crate::community::invite_row(
                    inv,
                    Box::new(cx.listener(move |app, _, window, cx| {
                        app.open_invite_details(d.clone(), window, cx)
                    })),
                    Box::new(cx.listener(move |app, _, window, cx| {
                        app.answer_invite(a.clone(), true, window, cx)
                    })),
                    Box::new(cx.listener(move |app, _, window, cx| {
                        app.answer_invite(b.clone(), false, window, cx)
                    })),
                )
                .into_any_element()
            })
            .collect()
    } else {
        Vec::new()
    };
    let (rows, forced) = chatlist_ops::rows(store, &ids, &app.archive, !invites.is_empty());
    app.sync_chatlist(&rows, store);
    let archive_row = rows
        .first()
        .filter(|r| **r == chatlist_ops::Row::Archive)
        .map(|_| app.archive_row(store, &ids, forced, cx));
    let has_archive = rows.first() == Some(&chatlist_ops::Row::Archive) || ids.iter().any(|&i| store.is_archived(&store.chats[i]));
    let empty = if !ids.is_empty() || !invites.is_empty() {
        None
    } else if requests {
        // .cc-empty
        Some(
            div()
                .mx(px(24.))
                .my(px(28.))
                .text_size(px(14.))
                .line_height(px(20.))
                .text_center()
                .text_color(white_a(0.45))
                .child("New people who message you show up here.")
                .into_any_element(),
        )
    } else {
        no_messages.then(|| get_started(cx).into_any_element())
    };
    let pin_keys: Rc<Vec<String>> = Rc::new(
        rows.iter()
            .filter_map(|r| match r {
                chatlist_ops::Row::Chat { idx, archived: false, .. } => store.chats.get(*idx),
                _ => None,
            })
            .filter(|c| store.pin_rank(&c.id).is_some())
            .map(|c| c.id.clone())
            .collect(),
    );
    let rows = Rc::new(rows);
    let archive_row = RefCell::new(archive_row);
    let store = shared.clone();
    let view = cx.entity().downgrade();
    let state = app.chatlist.clone();
    let notice = app.net_view.as_ref().and_then(|v| crate::routing::notice(v, app.net_stranded));
    let noticed = notice.is_some();
    div()
        .w(px(LIST_W))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(LIST_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .children(notice.map(|n| {
            crate::routing::notice_line(n, cx.listener(|app, _: &gpui::ClickEvent, window, cx| app.open_settings_path("privacy/routing", window, cx)))
        }))
        .child(
            // .chatlist-head
            div()
                .mt(px(if noticed { 0. } else { 22. }))
                .ml(px(15.))
                .mr(px(12.))
                .h(px(36.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(0xffffff))
                        .child("Chats"),
                )
                .child(
                    div()
                        .id("chatlist-new")
                        .tooltip(kit::tooltip("New Message or Community"))
                        .size(px(32.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(white_a(0.12))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|app, _, _, cx| {
                            let b = app.new_btn.get().unwrap_or_default();
                            app.show_menu(
                                gpui::point(b.left(), b.bottom() + px(6.)),
                                vec![
                                    (kit::MenuItem::new("New Message", "chat-circle").lead(), MenuAct::NewMessage),
                                    (kit::MenuItem::new("New Community", "users-multi").lead(), MenuAct::NewCommunity),
                                ],
                                cx,
                            );
                        }))
                        .child({
                            let probe = app.new_btn.clone();
                            gpui::canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().size_full()
                        })
                        .child(
                            svg()
                                .path("ui/plus-thin.svg")
                                .size(px(16.))
                                .text_color(c(ICON_MUTED)),
                        ),
                ),
        )
        .child(
            // .cc-tabs
            div()
                .mt(px(10.))
                .mx(px(11.))
                .flex()
                .border_b_1()
                .border_color(white_a(0.2))
                .relative()
                .child(tab("Messages", !requests, dot_messages, to_messages))
                .child(tab("Requests", requests, dot_requests, to_requests))
                .child(bar),
        )
        .children(empty)
        .when(!invites.is_empty(), |d| {
            d.child(div().pt(px(6.)).flex().flex_col().children(invites))
        })
        .child(
            div()
                .id("chatlist-scroll")
                .flex_1()
                .min_h_0()
                .pt(px(6.))
                .on_scroll(cx.listener(move |app, e: &gpui::ScrollEvent, _, cx| {
                    app.archive_wheel(e, forced, has_archive, cx);
                    app.archive_scrolled(forced);
                }))
                .child(
                    list(state, move |i, _, _| {
                        let Some(row) = rows.get(i).copied() else {
                            return div().into_any_element();
                        };
                        let chatlist_ops::Row::Chat { idx, archived, last } = row else {
                            return archive_row.borrow_mut().take().unwrap_or_else(|| div().into_any_element());
                        };
                        let store = store.borrow();
                        let Some(chat) = store.chats.get(idx) else {
                            return div().into_any_element();
                        };
                        let active = open.as_ref().is_some_and(|o| *o == chat.id);
                        let id: SharedString = chat.id.clone().into();
                        let (open_view, menu_view, menu_id) = (view.clone(), view.clone(), chat.id.clone());
                        let pinned = store.pin_rank(&chat.id).is_some();
                        let mut row = chat_row(&store, chat, active, pinned)
                            .on_click(move |_, window, cx| {
                                let id = id.clone();
                                open_view
                                    .update(cx, |app, cx| {
                                        app.open_chat(id, window, cx);
                                        cx.notify();
                                    })
                                    .ok();
                            })
                            .on_mouse_down(MouseButton::Right, move |e, _, cx| {
                                let (id, at) = (menu_id.clone(), e.position);
                                menu_view.update(cx, |app, cx| app.chat_row_menu(id, at, cx)).ok();
                            });
                        if pinned && !archived {
                            let name = store.name(&chat.id).into();
                            row = chatlist_ops::pin_draggable(row, chat.id.clone(), name, pin_keys.clone(), view.clone());
                        }
                        if archived && last {
                            div()
                                .pb(px(6.))
                                .child(div().pb(px(6.)).border_b_1().border_color(white_a(0.06)).child(row))
                                .into_any_element()
                        } else {
                            row.into_any_element()
                        }
                    })
                    .size_full(),
                ),
        )
}

/// `.chatlist-get-started`: the empty list's first step, which rides the New Chat button.
fn get_started(cx: &mut Context<VectorApp>) -> impl IntoElement {
    div().pt(px(6.)).px(px(7.)).child(
        div()
            .id("chatlist-get-started")
            .relative()
            .overflow_hidden()
            .h(px(50.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded_full()
            .border_1()
            .border_color(ca(89, 252, 179, 0.5))
            .bg(c(0x000000))
            .cursor_pointer()
            .on_click(cx.listener(|app, _, window, cx| app.open_new_chat(window, cx)))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(gpui::relative(0.35))
                    .right_0()
                    .rounded_r_full()
                    .bg(gpui::linear_gradient(
                        90.,
                        gpui::linear_color_stop(ca(89, 252, 179, 0.), 0.),
                        gpui::linear_color_stop(ca(89, 252, 179, 0.3), 1.),
                    )),
            )
            .child(
                div()
                    .ml(px(5.))
                    .size(px(38.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(c(0x191919))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path("icons/add-user.svg")
                            .size(px(17.))
                            .text_color(c(ACCENT)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr(px(12.))
                    .truncate()
                    .text_size(px(15.))
                    .line_height(px(18.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(0xf7f4f4))
                    .child("Get Started"),
            ),
    )
}

/// `.chatlist-contact` (ChatlistRow.svelte).
/// The chat list's last-message line (`chatPreviewText`).
/// `.typing-strip`: who is typing in the open chat, in the room the last row keeps clear
/// above the composer.
fn typing_strip(store: &Store, chat: &str, fade: f32) -> Option<AnyElement> {
    const FACES: usize = 3;
    let typers = store.typers(chat);
    if typers.is_empty() {
        return None;
    }
    let t = crate::attachments::EPOCH.elapsed().as_secs_f32();
    let dots = (0..3).map(|i| {
        // `typing-wave`: up 1.5px and fully lit at 30% of each 1.2s beat.
        let p = ((t - i as f32 * 0.15) / 1.2).rem_euclid(1.);
        let k = if p < 0.3 { p / 0.3 } else if p < 0.6 { 1. - (p - 0.3) / 0.3 } else { 0. };
        let k = k * k * (3. - 2. * k);
        div().size(px(4.)).rounded_full().bg(white_a(0.7)).opacity(0.45 + 0.55 * k).mt(px(-1.5 * k))
    });
    let names: Vec<String> = if typers.len() > 3 { Vec::new() } else { typers.iter().map(|n| store.name(n)).collect() };
    let mut text = div().min_w_0().flex().overflow_hidden().whitespace_nowrap();
    if names.is_empty() {
        text = text.child("Several people are typing");
    } else {
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                text = text.child(if i == names.len() - 1 { "\u{a0}and\u{a0}" } else { ",\u{a0}" });
            }
            text = text.child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(0xffffff)).child(name.clone()));
        }
        text = text.child(if names.len() == 1 { "\u{a0}is typing" } else { "\u{a0}are typing" });
    }
    Some(
        div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(58.))
            .pt(px(32.))
            .px(px(16.))
            .flex()
            .items_start()
            .gap(px(7.))
            .text_size(px(12.5))
            .line_height(px(18.))
            .text_color(white_a(0.55))
            .when(fade > 0., |d| {
                d.child(
                    div().absolute().top_0().bottom_0().left_0().right(px(10.)).opacity(fade).bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(ca(3, 3, 3, 0.), 0.),
                        gpui::linear_color_stop(c(0x030303), 1.),
                    )),
                )
            })
            .child(div().flex().flex_shrink_0().children(typers.iter().take(FACES).enumerate().map(|(i, n)| {
                div()
                    .size(px(18.))
                    .rounded_full()
                    .shadow(vec![gpui::BoxShadow { color: c(0x030303), offset: gpui::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1.), inset: false }])
                    .when(i > 0, |d| d.ml(px(-6.)))
                    .child(avatar(store, n, 18.))
            })))
            .child(div().h(px(18.)).flex().items_center().gap(px(3.)).flex_shrink_0().children(dots))
            .child(text)
            .into_any_element(),
    )
}

fn preview_line(store: &Store, chat: &SerializableChat, color: gpui::Hsla) -> AnyElement {
    preview_line_at(store, chat, color, 13., 18.)
}

fn preview_line_at(store: &Store, chat: &SerializableChat, color: gpui::Hsla, size: f32, line: f32) -> AnyElement {
    let typers = store.typers(&chat.id);
    let group = chat.chat_type != vector_core::ChatType::DirectMessage;
    if let Some(text) = data::typing_text(&typers, group, |n| store.name(n)) {
        // `.typing-indicator-text`: white into grey, breathing over 6s.
        let phase = (crate::attachments::EPOCH.elapsed().as_secs_f32() / 6.).fract();
        return div()
            .overflow_hidden()
            .text_size(px(size))
            .line_height(px(line))
            .child(crate::kit::gradient_text_in(&text, 0., phase, c(0xffffff), c(0x808080)))
            .into_any_element();
    }
    let look = crate::inline::Look::new(RUBIK, size, line, color);
    let Some(m) = chat.messages.last() else {
        return crate::inline::inline(Vec::new(), look).into_any_element();
    };
    let prefix = if m.mine { "You: " } else { "" };
    let atoms = if m.pending {
        vec![crate::inline::Atom::Text {
            text: "Sending...".into(),
            look: look.clone(),
            kind: crate::inline::Kind::Plain,
        }]
    } else if store.pivx.contains_key(&m.id) {
        vec![crate::inline::Atom::Text {
            text: format!("{prefix}Sent a PIVX Payment").into(),
            look: look.clone(),
            kind: crate::inline::Kind::Plain,
        }]
    } else if m.content.is_empty() && !m.attachments.is_empty() {
        let text = format!(
            "{prefix}Sent a {}",
            data::file_type(&m.attachments[0].extension).1
        );
        vec![crate::inline::Atom::Text {
            text: text.into(),
            look: look.clone(),
            kind: crate::inline::Kind::Plain,
        }]
    } else {
        let mut v = Vec::new();
        if !prefix.is_empty() {
            v.push(crate::inline::Atom::Text {
                text: prefix.into(),
                look: look.clone(),
                kind: crate::inline::Kind::Plain,
            });
        }
        let content = crate::texttime::localize(
            &m.content,
            &m.text_spans,
            chrono::Local::now().timestamp() as f64,
        );
        let content = store.resolve_mentions(&content, store.community_of(&chat.id).map(|c| c.community_id.as_str()));
        v.extend(crate::preview::atoms(
            &crate::embeds::for_preview(&content),
            usize::MAX,
            &look,
        ));
        v
    };
    crate::inline::inline(atoms, look)
        .single_line()
        .into_any_element()
}

fn chat_row(store: &Store, chat: &SerializableChat, active: bool, pinned: bool) -> gpui::Stateful<gpui::Div> {
    let unread = store.unread_count(chat) > 0 && !active;
    let (name_color, name_weight) = match (unread, active) {
        (true, _) => (c(0xffffff), FontWeight::BOLD),
        (false, true) => (c(0xffffff), FontWeight::NORMAL),
        _ => (white_a(0.75), FontWeight::NORMAL),
    };
    let preview_color = if active {
        white_a(0.85)
    } else if unread {
        c(PREVIEW_UNREAD)
    } else {
        ca(160, 160, 160, 0.5)
    };
    let last = chat.messages.last().map(|m| m.at).unwrap_or(0);
    div()
        .id(SharedString::from(format!("row-{}", chat.id)))
        .h(px(ROW_PITCH))
        .w(px(230.))
        .pb(px(4.))
        .child(
            div()
                .relative()
                .h(px(50.))
                .flex()
                .items_center()
                .pl(px(12.))
                .when(active, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(-1.))
                            .top(px(5.))
                            .w(px(3.))
                            .h(px(40.))
                            .rounded_r(px(3.))
                            .bg(c(PRIMARY)),
                    )
                })
                .child(avatar(store, &chat.id, 40.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(40.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(20.))
                                .pl(px(10.))
                                .pr(px(15.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(RUBIK)
                                        .text_size(px(15.))
                                        .line_height(px(20.))
                                        .font_weight(name_weight)
                                        .text_color(name_color)
                                        .child(store.name(&chat.id)),
                                )
                                .when(store.profiles.get(&chat.id).is_some_and(|p| p.bot), |d| {
                                    d.child(svg().path("icons/bot.svg").size(px(14.)).flex_shrink_0().text_color(c(ACCENT)))
                                })
                                .when(pinned, |d| {
                                    d.child(svg().path("icons/pin.svg").size(px(14.)).flex_shrink_0().text_color(c(ACCENT)))
                                }),
                        )
                        .child(
                            div()
                                .mt(px(-4.))
                                .h(px(24.))
                                .pr(px(10.))
                                .flex()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .ml(px(10.))
                                        .mr(px(8.))
                                        .overflow_hidden()
                                        .child(preview_line(store, chat, preview_color)),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(11.))
                                        .text_color(c(LIST_TIME))
                                        .child(data::time_ago(store.now_ms(), last)),
                                ),
                        ),
                ),
        )
}

// ── .chat ───────────────────────────────────────────────────────────────────────────────

fn empty_pane(viktor: Option<Entity<crate::viktor::Viktor>>) -> AnyElement {
    if let Some(viktor) = viktor {
        return div()
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(intro(viktor))
            .into_any_element();
    }
    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(img("icons/vector-logo.svg").w(px(128.)).h(px(132.)).opacity(0.1))
        .child(
            div()
                .mt(px(14.))
                .text_size(px(14.))
                .line_height(px(24.))
                .text_color(white_a(0.35))
                .child("Pick a conversation to get started"),
        )
        .into_any_element()
}

/// `.chatlist-intro` in the empty conversation pane: Viktor's welcome, pointing at the Hub.
fn intro(viktor: Entity<crate::viktor::Viktor>) -> impl IntoElement {
    const TEXT_P: &str = "Feel free to join the public community to learn more about Vector, discuss privacy, and make some new friends.";
    let link = 13..13 + "join the public community".len();
    let accent = gpui::HighlightStyle {
        color: Some(c(ACCENT)),
        ..Default::default()
    };
    let words = gpui::InteractiveText::new(
        "intro-text",
        gpui::StyledText::new(TEXT_P).with_highlights([(link.clone(), accent)]),
    )
    .on_click(vec![link], |_, _, cx| {
        cx.open_url("https://vectorapp.io/hub")
    });
    div()
        .max_w(px(448.))
        .px(px(24.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(18.))
        .text_center()
        .child(viktor)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .mb(px(10.))
                        .text_size(px(24.))
                        .line_height(px(29.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(0xf7f4f4))
                        .child("Welcome to Vector!"),
                )
                .child(
                    div()
                        .text_size(px(15.))
                        .line_height(px(15. * 1.55))
                        .text_color(white_a(0.55))
                        .child(words),
                ),
        )
        .with_animation(
            "intro-in",
            gpui::Animation::new(std::time::Duration::from_millis(400))
                .with_easing(kit::bezier(0.22, 1., 0.36, 1.)),
            |d, t| d.opacity(t).mt(px(8. * (1. - t))),
        )
}

impl VectorApp {
    fn dm_header(
        &self,
        store: &Store,
        id: &SharedString,
        papered: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = id.clone();
        let notes = store.my_npub.as_str() == id.as_ref();
        let subtext = if notes {
            "Encrypted Notes to Self".to_string()
        } else if store.stream_hidden(&id) {
            String::new()
        } else {
            store.profiles.get(id.as_ref()).map(|p| p.status.title.clone()).unwrap_or_default()
        };
        let name_row = div()
            .id("chat-header-info")
            .when(!notes, |d| {
                let id = id.to_string();
                d.cursor_pointer().on_click(cx.listener(move |app, _, _, cx| app.open_profile(id.clone(), cx)))
            })
            .flex()
            .items_center()
            .gap(px(6.))
            .child(if notes { div().into_any_element() } else { avatar(store, &id, 22.).into_any_element() })
            .child(
                div()
                    .font_family(RUBIK)
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(18.72))
                    .line_height(px(24.))
                    .text_color(c(TEXT))
                    .child(if notes { "Notes".to_string() } else { store.name(&id) }),
            );
        let menu = !notes || store.chat(&id).is_some();
        div()
            .h(px(61.))
            .flex_shrink_0()
            .relative()
            .map(|d| crate::theme::chat_header_ground(d, papered))
            .flex()
            .justify_center()
            .map(|d| {
                if subtext.is_empty() {
                    d.items_center().child(name_row.mt(px(5.)))
                } else {
                    d.child(
                        div()
                            .pt(px(9.))
                            .max_w(gpui::relative(0.6))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(name_row)
                            .child(
                                div()
                                    .max_w_full()
                                    .overflow_hidden()
                                    .opacity(0.8)
                                    .child(crate::inline::emoji_line(&subtext, crate::inline::Look::new(INTER, 14., 24., c(TEXT)), 17., 1.)),
                            ),
                    )
                }
            })
            .child(
                div()
                    .absolute()
                    .right(px(10.))
                    .top(px(10.))
                    .flex()
                    .when(!notes, |d| {
                        let peer = id.to_string();
                        d.child(
                            div()
                                .id("chat-call")
                                .size(px(40.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.with_realtime_consent(CallGo::Start(peer.clone()), cx)
                                }))
                                .child(
                                    svg()
                                        .path("ui/call.svg")
                                        .size(px(22.))
                                        .text_color(c(ICON_MUTED)),
                                ),
                        )
                    })
                    .when(menu, |d| d.child({
                        let probe = self.chat_menu_btn.clone();
                        let chat = id.to_string();
                        div()
                            .id("chat-menu")
                            .tooltip(crate::kit::tooltip("Chat Options"))
                            .relative()
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |app, _, _, cx| app.open_chat_menu(chat.clone(), cx)))
                            .child(
                                gpui::canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {})
                                    .absolute()
                                    .size_full(),
                            )
                            .child(
                                svg()
                                    .path("ui/menu-dots.svg")
                                    .size(px(28.))
                                    .text_color(c(ICON_MUTED)),
                            )
                    })),
            )
    }

    /// Under the header's ⋯, as `showContextMenu` places it.
    fn chat_menu_at(&self) -> gpui::Point<gpui::Pixels> {
        let b = self.chat_menu_btn.get().unwrap_or_default();
        gpui::point(b.right(), b.bottom() + px(4.))
    }

    /// What the open chat's wallpaper layer should show: a staged pick, else the chat's own
    /// unless backgrounds are off or it is a request's.
    fn wallpaper_key(&self) -> Option<crate::wallpaper::Key> {
        let id = self.open.as_ref()?;
        let b = self.chat_box.get()?;
        let size = (f32::from(b.size.width).round() as u32, f32::from(b.size.height).round() as u32);
        if let Some(p) = self.wallpaper.preview.as_ref().filter(|p| p.chat == id.as_ref()) {
            return Some(crate::wallpaper::Key { path: p.path.clone(), ts: p.ts, blur: p.blur, dim: p.dim, size });
        }
        let store = self.store.borrow();
        let chat = store.chat(id)?;
        let streaming = store.streamer.on && store.streamer.hide_wallpapers;
        if !self.chat_bg || streaming || chat.wallpaper_path.is_empty() || store.is_request(chat) {
            return None;
        }
        Some(crate::wallpaper::Key {
            path: store.media_root.join(&chat.wallpaper_path),
            ts: chat.wallpaper_ts,
            blur: chat.wallpaper_blur.min(30),
            dim: chat.wallpaper_dim.min(100),
            size,
        })
    }

    fn sync_wallpaper(&mut self, cx: &mut Context<Self>) {
        if self.chat_box.get().is_none() && self.open.is_some() {
            return;
        }
        let want = self.wallpaper_key();
        if let Some(job) = self.wallpaper.want(want) {
            self.bake_wallpaper(job, cx);
        }
    }

    fn bake_wallpaper(&mut self, job: crate::wallpaper::Job, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (key, decoded, image) = cx.background_executor().spawn(async move { crate::wallpaper::bake(job) }).await;
            this.update(cx, |this, cx| {
                if let Some(next) = this.wallpaper.baked(key, decoded, image) {
                    this.bake_wallpaper(next, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `startWallpaperChange`: pick a picture, stage it, and open the sliders.
    fn pick_wallpaper(&mut self, chat: String, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let live = self.live;
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let picked = path.to_string_lossy().to_string();
            let staged = if live {
                let chat = chat.clone();
                backend::run(vector_core::db::scoped(async move {
                    vector_core::wallpaper::prepare_wallpaper_preview(&chat, &picked)
                        .await
                        .map(|p| (std::path::PathBuf::from(p.path), p.was_animated, p.recommended_dim))
                }))
                .await
                .unwrap_or_else(|_| Err("cancelled".into()))
            } else {
                Ok((path, false, crate::wallpaper::DEFAULT_DIM))
            };
            this.update(cx, |this, cx| {
                match staged {
                    Ok((path, animated, recommended)) => this.stage_wallpaper(chat, path, animated, recommended, cx),
                    Err(e) => this.notice("Couldn\u{2019}t use that image", e, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// `applyWallpaperPreview`: a re-pick keeps the chat's settings; a first takes the
    /// suggested brightness.
    fn stage_wallpaper(&mut self, chat: String, path: std::path::PathBuf, animated: bool, recommended: u8, cx: &mut Context<Self>) {
        use crate::wallpaper::{DEFAULT_BLUR, Preview};
        let (blur, dim) = match self.store.borrow().chat(&chat).filter(|c| !c.wallpaper_path.is_empty()) {
            Some(c) => (c.wallpaper_blur, c.wallpaper_dim),
            None => (DEFAULT_BLUR, recommended),
        };
        let ts = chrono::Utc::now().timestamp_millis() as u64;
        self.wallpaper.preview = Some(Preview { chat, path, blur, dim, ts });
        self.wallpaper.busy = false;
        self.wallpaper.label = "Edit Mode is enabled.".into();
        self.wallpaper.set_editing(true);
        if animated {
            let mut popup = kit::Popup::notice("Static wallpapers only");
            popup.text = Some("Vector wallpapers don\u{2019}t animate. We grabbed the first frame of your image to use instead.".into());
            self.popup = Some((popup, PopupAct::Notice));
        }
        cx.notify();
    }

    fn wallpaper_slider(&mut self, blur: bool, value: u8, cx: &mut Context<Self>) {
        if let Some(p) = self.wallpaper.preview.as_mut() {
            if blur {
                p.blur = value.min(30);
            } else {
                p.dim = value.clamp(10, 100);
            }
            cx.notify();
        }
    }

    /// `confirmWallpaperChange`: send the staged picture with its settings.
    fn save_wallpaper(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.wallpaper.preview.as_ref() else { return };
        if self.wallpaper.busy {
            return;
        }
        let (chat, blur, dim) = (p.chat.clone(), p.blur, p.dim);
        self.wallpaper.busy = true;
        self.wallpaper.label = "Uploading\u{2026}".into();
        let rx = if self.live {
            backend::run(vector_core::db::scoped(async move {
                vector_core::wallpaper::publish_wallpaper(&chat, blur, dim).await
            }))
        } else {
            let (tx, rx) = futures::channel::oneshot::channel();
            let _ = tx.send(Ok(()));
            rx
        };
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                this.wallpaper.busy = false;
                this.wallpaper.label = "Edit Mode is enabled.".into();
                match res {
                    Ok(()) => {
                        this.wallpaper.preview = None;
                        this.wallpaper.set_editing(false);
                        if !this.chat_bg {
                            let mut popup = kit::Popup::notice("Background Wallpaper");
                            popup.text = Some("Your wallpaper is set. If you want to be able to see it, please enable <b>Settings \u{2192} Display \u{2192} Background Wallpaper</b>.".into());
                            this.popup = Some((popup, PopupAct::Notice));
                        }
                    }
                    Err(e) => this.notice_plain("Wallpaper not sent", &e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `cancelWallpaperChange`: drop the staged picture.
    fn cancel_wallpaper(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.wallpaper.preview.take() else { return };
        self.wallpaper.set_editing(false);
        if self.live {
            let chat = p.chat;
            let _ = backend::run(vector_core::db::scoped(async move {
                let _ = vector_core::wallpaper::cancel_wallpaper_preview(&chat).await;
            }));
        }
        cx.notify();
    }

    /// Send held for 500ms opens the timer picker rather than sending.
    fn send_press(&mut self, cx: &mut Context<Self>) {
        let n = self.send_held.map_or(0, |n| n + 1);
        self.send_held = Some(n);
        self.send_long = false;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            this.update(cx, |app, cx| {
                if app.send_held == Some(n) {
                    app.send_long = true;
                    app.open_send_timer(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The timer picker, from a right-click or a long press on Send.
    fn open_send_timer(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone() else { return };
        let b = self.send_btn.get().unwrap_or_default();
        self.open_self_destruct(chat.to_string(), gpui::point(b.right(), b.bottom() + px(4.)));
        cx.notify();
    }

    /// `buildChatMenuItems`: what the open chat's ⋯ offers.
    fn open_chat_menu(&mut self, chat: String, cx: &mut Context<Self>) {
        let mut items = vec![kit::MenuItem::new("Self-Destruct Timer", "clock")];
        let mut acts = vec![MenuAct::SelfDestruct(chat.clone())];
        let community = self.store.borrow().community_of(&chat).map(|c| c.community_id.clone());
        if let Some(community) = community
            && self.community_caps(&community).contains("manage_channels")
        {
            items.push(kit::MenuItem::new("Edit Channel", "edit"));
            acts.push(MenuAct::EditChannel {
                community,
                channel: chat.clone(),
                category: false,
            });
        }
        let papered = self.store.borrow().chat(&chat).map(|c| !c.wallpaper_path.is_empty());
        if let Some(papered) = papered.filter(|_| self.store.borrow().community_of(&chat).is_none()) {
            items.push(kit::MenuItem::new("Change Wallpaper", "image"));
            acts.push(MenuAct::ChangeWallpaper(chat.clone()));
            if papered {
                items.push(kit::MenuItem::new("Remove Wallpaper", "trash"));
                acts.push(MenuAct::RemoveWallpaper(chat));
            }
        }
        self.menu_trail.clear();
        self.menu = Some((self.chat_menu_at(), items, acts));
        cx.notify();
    }

    /// The timer's durations, the current one ticked; a pick applies to the next send.
    fn open_self_destruct(&mut self, chat: String, at: gpui::Point<gpui::Pixels>) {
        const OPTIONS: [(&str, Option<u64>); 8] = [
            ("Permanent", None),
            ("1 Week", Some(604_800)),
            ("1 Day", Some(86_400)),
            ("6 Hours", Some(21_600)),
            ("1 Hour", Some(3_600)),
            ("5 Minutes", Some(300)),
            ("60 Seconds", Some(60)),
            ("10 Seconds", Some(10)),
        ];
        let current = if self.live { vector_core::self_destruct::chat_duration_secs(&chat) } else { self.self_destruct };
        let items = OPTIONS
            .iter()
            .map(|(label, secs)| kit::MenuItem {
                label: (*label).into(),
                hint: (*secs == current).then(|| "\u{2713}".into()),
                ..Default::default()
            })
            .collect();
        let acts = OPTIONS.iter().map(|(_, secs)| MenuAct::SetSelfDestruct(chat.clone(), *secs)).collect();
        self.menu_trail.clear();
        self.menu = Some((at, items, acts));
    }

    fn on_voice_handler(&self, cx: &mut Context<Self>) -> crate::voice::OnAct {
        let app = cx.entity().downgrade();
        Rc::new(move |act, window, cx| {
            app.update(cx, |app, cx| app.on_voice(act, window, cx)).ok();
        })
    }

    fn fixture_lock(&mut self, lock: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut store = self.store.borrow_mut();
        let now = store.now_ms() / 1000;
        let me = store.my_npub.clone();
        if lock == "blocked" {
            let Some(frank) = store.profiles.iter().find(|(_, p)| p.name == "frank").map(|(n, _)| n.clone()) else { return };
            if let Some(p) = store.profiles.get_mut(&frank) {
                p.is_blocked = true;
            }
            drop(store);
            self.open_chat(frank.into(), window, cx);
            return;
        }
        for c in &mut store.communities {
            match lock {
                "dissolved" => c.dissolved = true,
                "perms" => c.channels.iter_mut().for_each(|ch| ch.my_permissions = Some("0".into())),
                "writer" => c.channels.iter_mut().for_each(|ch| (ch.restricted, ch.can_post) = (true, false)),
                _ => {}
            }
        }
        if lock == "timeout" {
            let ids: Vec<String> = store.communities.iter().map(|c| c.community_id.clone()).collect();
            for id in ids {
                store.timeouts.entry(id).or_default().insert(me.clone(), data::Timeout { from: now - 600, until: now + 3 * 3600 });
            }
        }
    }

    fn chat_pane(
        &self,
        store: &Store,
        id: SharedString,
        on_community: &crate::community::OnAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let channel = store.community_of(&id).and_then(|com| com.channel(&id));
        let lock = store.send_lock(&id);
        let staged_voice = channel.is_some_and(|ch| ch.is_voice());
        let stage = channel.filter(|ch| ch.is_voice()).and_then(|ch| {
            let com = store.community_of(&id)?;
            Some(crate::voice::stage(
                &self.voice,
                store,
                &self.voice_seen,
                f32::from(window.viewport_size().height),
                &com.community_id,
                &ch.channel_id,
                &store.my_npub,
                self.is_slim,
                &self.on_voice_handler(cx),
            ))
        });
        let panel = self.community.as_ref().is_some_and(|v| v.panel);
        let paper = self.wallpaper.image(self.wallpaper_key().as_ref());
        let slim_com = channel.and_then(|_| store.community_of(&id)).filter(|_| self.is_slim);
        let header = if let (Some(ch), Some(com)) = (channel, slim_com) {
            crate::community::slim_header(store, com, ch, self.chat_menu_btn.clone(), on_community).into_any_element()
        } else if let Some(ch) = channel {
            let pins = self.pins.button(&id).map(|_| self.pins_on(cx));
            crate::community::header(ch, panel, self.chat_menu_btn.clone(), pins, on_community).into_any_element()
        } else {
            self.dm_header(store, &id, paper.is_some(), cx).into_any_element()
        };
        let on_wp: crate::wallpaper::OnAct = {
            let app = cx.entity().downgrade();
            Rc::new(move |act, _, cx| {
                app.update(cx, |app, cx| match act {
                    crate::wallpaper::Act::Cancel => app.cancel_wallpaper(cx),
                    crate::wallpaper::Act::Save => app.save_wallpaper(cx),
                    crate::wallpaper::Act::Slide(blur, v) => app.wallpaper_slider(blur, v, cx),
                })
                .ok();
            })
        };
        let staged = self.wallpaper.preview.as_ref().filter(|p| p.chat == id.as_ref());
        let edit_bar = self.wallpaper.edit_alpha().filter(|_| staged.is_some() || self.wallpaper.preview.is_none()).map(|a| {
            crate::wallpaper::edit_bar(&self.wallpaper.label, self.wallpaper.busy, a, &on_wp)
        });
        let preview_bar = staged.map(|p| crate::wallpaper::preview_bar(p.blur, p.dim, self.wallpaper.dragging.clone(), &on_wp));
        let chat_box = self.chat_box.clone();
        let rows_store = self.store.clone();
        let rows_id = id.clone();
        let rows = self.rows.clone();
        let revealed = self.revealed.clone();
        let clock = self.started.elapsed().as_secs_f32();
        let app = cx.entity().downgrade();
        let app_media = app.clone();
        let on_act: timeline::OnAct = Rc::new(move |msg_id, act, window, cx| {
            let msg_id = msg_id.to_string();
            app.update(cx, |app, cx| app.on_act(msg_id, act, window, cx))
                .ok();
        });
        let hovered = self.hovered.clone();
        let picker = self.picker.clone();
        let replying = match &self.mode {
            Mode::Reply { id, .. } => Some(id.clone()),
            _ => None,
        };
        let fail_reasons = self.fail_reasons.clone();
        let media = self.media.clone();
        let live = self.live;
        let invites = Rc::new(self.invite_previews.clone());
        let rules: Option<Rc<crate::mentions::Rules>> = store
            .community_of(&id)
            .map(|c| match self.mention_rules.get(&c.community_id) {
                Some(r) => Rc::new(r.clone()),
                // Fixtures answer as the mock does: anyone may ping everyone.
                None => Rc::new(crate::mentions::Rules { anyone: !self.live, ..Default::default() }),
            });
        let jumped = self
            .jumped
            .clone()
            .filter(|(_, t)| t.elapsed().as_secs_f32() < 1.5);
        let cmd_names = self.command_names();
        // `.history-edge.alone`: a chat with nothing yet has its beginning mid-view.
        let alone = match self.rows.as_slice() {
            [Row::Edge(edge)] => store.chat(&id).map(|chat| history_edge(store, chat, &on_act, *edge)),
            _ => None,
        };
        let anchors = self.anchors.clone();
        // 3px border, 12px padding and the 56px gutter on the left; 12px and the body's 12px on the right.
        let content_w = (f32::from(self.timeline.viewport_bounds().size.width) - 95.).max(0.);
        let vw = f32::from(window.viewport_size().width);
        let messages = list(self.timeline.clone(), move |i, window, cx| {
            let rows_store = rows_store.borrow();
            let (Some(chat), Some(row)) = (rows_store.chat(&rows_id), rows.get(i)) else {
                return div().into_any_element();
            };
            let revealed = revealed.borrow();
            let fail_reasons = fail_reasons.borrow();
            let ctx = timeline::Ctx {
                anchors: &anchors,
                rules: rules.as_deref(),
                store: &rows_store,
                chat,
                revealed: &revealed,
                clock,
                on_act: on_act.clone(),
                hovered: hovered.as_deref(),
                replying: replying.as_deref(),
                picker: picker.as_deref(),
                fail_reasons: &fail_reasons,
                media: &media,
                scale: window.scale_factor(),
                held: rows_store.is_request(chat),
                invites: &invites,
                jumped: jumped
                    .as_ref()
                    .map(|(id, t)| (id.as_str(), t.elapsed().as_secs_f32())),
                commands: cmd_names.as_deref(),
                content_w,
                vw,
            };
            if ctx.jumped.is_some_and(|(id, _)| {
                chat.messages
                    .get(match row {
                        Row::Message { index, .. } => *index,
                        _ => usize::MAX,
                    })
                    .is_some_and(|m| m.id == id)
            }) {
                window.request_animation_frame();
            }
            // A divider's 10px margins stand in for the row's own above it.
            let after_message = i > 0 && matches!(rows[i - 1], Row::Message { .. });
            let el = timeline_row(&ctx, row, after_message);
            // `.chat-rows > *:last-child`: clear of the typing strip.
            let el = if i + 1 == rows.len() {
                div().pb(px(33.)).child(el).into_any_element()
            } else {
                el
            };
            crate::media::pump(&media, app_media.clone(), live, cx);
            let packs = std::mem::take(&mut media.borrow_mut().packs.queue);
            if !packs.is_empty() {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| app.fetch_packs(packs, cx)).ok();
                });
            }
            let embeds = std::mem::take(&mut media.borrow_mut().embeds.queue);
            if !embeds.is_empty() {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| app.fetch_embeds(embeds, cx)).ok();
                });
            }
            let metas = std::mem::take(&mut media.borrow_mut().meta_queue);
            if live {
                for (chat, msg) in metas {
                    let _ = backend::run(backend::unfurl::fetch_msg_metadata(chat, msg));
                }
            }
            let clips = std::mem::take(&mut media.borrow_mut().videos.queue);
            if !clips.is_empty() {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| app.video_work(clips, cx)).ok();
                });
            }
            let videos = std::mem::take(&mut media.borrow_mut().embed_video_queue);
            if !videos.is_empty() {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| {
                        for v in videos {
                            app.fetch_embed_video(v, cx);
                        }
                    })
                    .ok();
                });
            }
            let previews = std::mem::take(&mut media.borrow_mut().embed_preview_queue);
            if !previews.is_empty() && live {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| {
                        for v in previews {
                            app.fetch_embed_preview(v, cx);
                        }
                    })
                    .ok();
                });
            }
            let links = std::mem::take(&mut media.borrow_mut().xdc_url_queue);
            if !links.is_empty() && live {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| app.probe_xdc_urls(links, cx)).ok();
                });
            }
            let packages = std::mem::take(&mut media.borrow_mut().xdc_queue);
            if !packages.is_empty() && live {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| app.read_packages(packages, cx)).ok();
                });
            }
            let probes = std::mem::take(&mut media.borrow_mut().audio.queue);
            if !probes.is_empty() {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| app.audio_work(probes, cx)).ok();
                });
            }
            let autos = std::mem::take(&mut media.borrow_mut().auto_queue);
            if !autos.is_empty() {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| {
                        for (msg, att) in autos {
                            app.start_download(msg, att, true, cx);
                        }
                    })
                    .ok();
                });
            }
            el
        })
        .size_full();

        let drawer = (self.pins.open && self.pins.button(&id).is_some()).then(|| {
            let pane_h = self.chat_box.get().map_or(800., |b| f32::from(b.size.height));
            // `max(200px, 40vh)`, the window being the pane plus its chrome.
            let max_h = ((pane_h + 32.) * 0.4).max(200.);
            crate::pins::drawer(&self.pins, store, &self.revealed.borrow(), clock, max_h, &self.pins_on(cx))
        });
        let bar = (alone.is_none() && drawer.is_none()).then(|| crate::scrollbar::render(&self.bar, window, self.bar_on(cx)));
        let back = (self.return_shown && alone.is_none() && drawer.is_none()).then(|| {
            let app = cx.entity().downgrade();
            crate::scrollbar::return_button(
                self.unread_below,
                Rc::new(move |_, cx| {
                    app.update(cx, |app, cx| {
                        app.unread_below = 0;
                        app.to_tail();
                        cx.notify();
                    })
                    .ok();
                }),
            )
        });
        let request = store
            .chat(&id)
            .is_some_and(|chat| store.is_request(chat))
            .then(|| request_bar(id.to_string(), store.pictures_held(&id), self.vw < 380., cx));
        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(c(CHAT_BG))
            .child(
                gpui::canvas(move |b, _, _| chat_box.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(paper.clone().map(|image| {
                gpui::img(gpui::ImageSource::Render(image)).absolute().top_0().left_0().size_full()
            }))
            .child(div().relative().flex_shrink_0().child(header).children(edit_bar))
            .children(stage)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .when(paper.is_none(), |d| d.bg(c(MESSAGES_BG)))
                    .pt(px({
                        // Slim's header floats 60px over a list padded 75px; a voice stage takes that place in flow.
                        let top = if self.is_slim && !staged_voice { 15. } else { 12. };
                        if request.is_some() { top + 56. } else { top }
                    }))
                    .children(bar)
                    .children(back)
                    .map(|d| match alone {
                        // Auto margins centre it until it no longer fits, then it scrolls from its top.
                        Some(edge) => d.child(div().id("chat-start").size_full().flex().flex_col().overflow_y_scroll().child(div().my_auto().child(edge))),
                        None => d.child(messages),
                    })
                    .children(request)
                    .children(drawer)
                    .children(typing_strip(store, &id, self.typing_fade).inspect(|_| window.request_animation_frame()))
                    // `.fadeout-bottom-msgs`: rows dim into the composer.
                    .child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(40.))
                            .bg(gpui::linear_gradient(
                                180.,
                                gpui::linear_color_stop(ca(3, 3, 3, 0.), 0.8),
                                gpui::linear_color_stop(ca(3, 3, 3, 0.5), 1.),
                            )),
                    ),
            )
            .children(preview_bar)
            .child(composer(
                self.input.clone(),
                &self.mode,
                (self.picker_open && !self.picker_status)
                    .then(|| (self.emoji_picker.clone(), self.picker.is_none(), f32::from(window.viewport_size().width) < 700.)),
                self.attach.map(|o| (o, self.pivx_wallet_view(cx).or_else(|| self.miniapps_view(cx)))),
                self.command_popup(cx).or_else(|| self.time_popup(cx)).or_else(|| self.mention
                    .as_ref()
                    .map(|p| {
                        let app = cx.entity().downgrade();
                        crate::mentions::view(
                            p,
                            Rc::new(move |i, w, cx| {
                                app.update(cx, |app, cx| app.pick_mention(i, w, cx)).ok();
                            }),
                        )
                    })
                    .or_else(|| {
                        self.shortcode.as_ref().map(|p| {
                            let app = cx.entity().downgrade();
                            crate::shortcodes::view(
                                p,
                                &mut self.media.borrow_mut(),
                                Rc::new(move |i, w, cx| {
                                    app.update(cx, |app, cx| app.pick_shortcode(i, w, cx)).ok();
                                }),
                            )
                        })
                    })).or_else(|| self.time_chip_view(cx)),
                self.composer_box.clone(),
                (
                    self.send_shown || matches!(self.mode, Mode::Edit { .. }) || self.cmd_form.is_some(),
                    self.send_flips,
                ),
                (self.self_destruct, self.send_btn.clone()),
                &self.rec,
                {
                    let app = cx.entity().downgrade();
                    Rc::new(move |ui, w: &mut Window, cx: &mut gpui::App| {
                        app.update(cx, |app, cx| app.rec_ui(ui, w, cx)).ok();
                    })
                },
                self.command_form_view(cx),
                lock.map(SharedString::from),
                cx,
            ))
    }
}

/// `.request-bar`: under the header of a DM from someone new, Block or Accept.
fn request_bar(npub: String, pictures: bool, tiny: bool, cx: &mut Context<VectorApp>) -> impl IntoElement {
    let button = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .flex_shrink_0()
            .h(px(34.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(9.))
            .border_1()
            .cursor_pointer()
            .text_size(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(label)
    };
    let (block, accept) = (npub.clone(), npub);
    div()
        .id("request-bar")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(56.))
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(12.))
        .pr(px(10.))
        .bg(c(0x0d0d0d))
        .border_b_1()
        .border_color(white_a(0.12))
        .when(!tiny, |d| {
            d.child(
                svg()
                    .path("ui/request-shield.svg")
                    .size(px(22.))
                    .flex_shrink_0()
                    .text_color(c(ACCENT)),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .line_height(px(17.))
                        .text_color(c(0xffffff))
                        .child("Message request"),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .line_height(px(16.))
                        .text_color(white_a(0.7))
                        .child(if pictures {
                            "Accept to load media & profile"
                        } else {
                            "Accept to load media"
                        }),
                ),
        )
        .child(
            button("request-block", "Block")
                .border_color(c(timeline::DANGER).opacity(0.6))
                .text_color(c(timeline::DANGER))
                .on_click(
                    cx.listener(move |app, _, _, cx| app.confirm_block_request(block.clone(), cx)),
                ),
        )
        .child(
            button("request-accept", "Accept")
                .bg(c(PRIMARY))
                .border_color(gpui::transparent_black())
                .text_color(c(0x000000))
                .on_click(cx.listener(move |app, _, _, cx| app.accept_request(accept.clone(), cx))),
        )
}

/// `.history-edge`: the chat's face over its beginning, or older history on its way.
fn history_edge(store: &Store, chat: &SerializableChat, on: &timeline::OnAct, edge: data::Edge) -> AnyElement {
    let line = |d: gpui::Div| {
        d.flex()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .pt(px(18.))
            .pb(px(10.))
            .text_size(px(13.))
            .text_color(white_a(0.5))
    };
    match edge {
        data::Edge::Loading => line(div())
            .child(div().size(px(14.)).child(crate::attachments::spinner(14., 2., 0.8, 0.25, ca(255, 255, 255, 0.15), c(PRIMARY))))
            .child("Loading older messages…")
            .into_any_element(),
        data::Edge::Retry => {
            let on = on.clone();
            line(div())
                .child("Couldn't load older messages")
                .child(
                    div()
                        .id("history-retry")
                        .px(px(12.))
                        .py(px(4.))
                        .rounded_full()
                        .border_1()
                        .border_color(white_a(0.15))
                        .hover(|d| d.border_color(white_a(0.3)))
                        .text_size(px(12.))
                        .text_color(c(0xffffff))
                        .cursor_pointer()
                        .on_click(move |_, w, cx| on("", timeline::Act::RetryHistory, w, cx))
                        .child("Retry"),
                )
                .into_any_element()
        }
        data::Edge::Start => {
            let alone = chat.messages.is_empty();
            let (art, title, note) = match store.community_of(&chat.id) {
                Some(com) => {
                    let name = com.channel(&chat.id).map_or("general", |ch| ch.name.as_str());
                    (
                        crate::community::logo(store, &com.community_id, 64., 32.).into_any_element(),
                        format!("Welcome to #{name}"),
                        if alone {
                            "This is the very beginning of the channel, say hello! 👋"
                        } else {
                            "This is the beginning of the channel."
                        },
                    )
                }
                None if chat.id == store.my_npub => (
                    crate::ui::avatar(store, &chat.id, 64.),
                    "Notes".to_string(),
                    "This is the beginning of your notes.",
                ),
                None => (
                    crate::ui::avatar(store, &chat.id, 64.),
                    store.name(&chat.id),
                    "This is the beginning of your conversation.",
                ),
            };
            div()
                .flex()
                .flex_col()
                .items_center()
                .font_family(RUBIK)
                .line_height(px(24.))
                .pt(px(32.))
                .px(px(24.))
                .pb(px(20.))
                .gap(px(6.))
                .text_color(white_a(0.6))
                .child(div().mb(px(8.)).child(art))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(white_a(0.9))
                        .child(title),
                )
                .child(div().max_w(px(280.)).text_center().text_size(px(13.)).child(note))
                .into_any_element()
        }
    }
}

/// A short chat sits at the top, as the Tauri list does; a long one opens at its end and
/// follows new messages while scrolled there.
fn timeline_state(count: usize, app: gpui::WeakEntity<VectorApp>) -> ListState {
    let state = ListState::new(count, ListAlignment::Top, px(600.));
    state.set_follow_mode(gpui::FollowMode::Tail);
    // Near the top, the next page of history.
    state.set_scroll_handler(move |ev, _, cx| {
        if ev.visible_range.start < 4 && ev.count > 0 {
            let app = app.clone();
            cx.defer(move |cx| {
                app.update(cx, |app, cx| app.load_older(cx)).ok();
            });
        }
    });
    state
}

fn timeline_row(ctx: &timeline::Ctx, row: &Row, after_message: bool) -> AnyElement {
    match row {
        Row::Edge(edge) => div().w_full().flex().justify_center().child(history_edge(ctx.store, ctx.chat, &ctx.on_act, *edge)).into_any_element(),
        Row::Date { label, time } => divider(
            div()
                .flex()
                .text_size(px(12.))
                .text_color(c(TEXT))
                .child(div().font_weight(FontWeight::BOLD).child(label.clone()))
                .child(format!(", {time}")),
            white_a(0.1),
            0.7,
        ),
        Row::Unread => divider(
            // letter-spacing: 0.08em, which GPUI text has no property for.
            div()
                .flex()
                .gap(px(0.88))
                .font_weight(FontWeight::BOLD)
                .text_size(px(11.))
                .text_color(c(ACCENT))
                .children("NEW".chars().map(|ch| div().child(ch.to_string()))),
            ca(89, 252, 179, 0.6),
            1.,
        ),
        Row::Notice(text) => div()
            .pb(px(20.))
            .child(divider(div().text_size(px(12.)).text_color(c(TEXT)).child(text.clone()), white_a(0.1), 0.7))
            .into_any_element(),
        Row::Message { index, first, run } => {
            let gap = if !after_message {
                0.
            } else if *first {
                14.
            } else {
                1.
            };
            timeline::message_row(ctx, *index, *first, *run, gap)
        }
    }
}

/// `.msg-inline-timestamp`: a centred label with a hairline either side.
fn divider(label: impl IntoElement, line: gpui::Hsla, opacity: f32) -> AnyElement {
    let rule = move || div().flex_1().h(px(1.)).bg(line);
    div()
        .px(px(10.))
        .py(px(10.))
        .child(
            div()
                .h(px(24.))
                .flex()
                .items_center()
                .gap(px(12.))
                .font_family(RUBIK)
                .opacity(opacity)
                .child(rule())
                .child(label)
                .child(rule()),
        )
        .into_any_element()
}

/// `#chat-box.input-box`: the gradient frame around the composer, with the reply bar
/// tucked behind its top while replying, and edit's cancel in place of attach.
fn composer(
    input: Entity<TextInput>,
    mode: &Mode,
    picker: Option<(Entity<crate::picker::EmojiPicker>, bool, bool)>,
    attach: Option<(std::time::Instant, Option<AnyElement>)>,
    popup: Option<AnyElement>,
    measure: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    send: (bool, usize),
    (timer, send_probe): (Option<u64>, Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>),
    rec: &crate::recorder::Recorder,
    on_rec: crate::recorder::On,
    command: Option<(AnyElement, AnyElement)>,
    lock: Option<SharedString>,
    cx: &mut Context<VectorApp>,
) -> impl IntoElement {
    use crate::recorder::State as R;
    let (strip, form) = command.map_or((None, None), |(s, f)| (Some(s), Some(f)));
    let commanding = form.is_some();
    let locked = lock.is_some();
    let busy = !locked && matches!(rec.state, R::Recording | R::Locked | R::Preview | R::Cancelled);
    let previewing = rec.state == R::Preview;
    let hides_mic = matches!(rec.state, R::Recording | R::Locked | R::Preview);
    let icon = |path: &'static str| {
        div()
            .w(px(38.))
            .h(px(45.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(svg().path(path).size(px(23.)).text_color(c(ICON_MUTED)))
    };
    let cancel = |id: &'static str, size: f32, cx: &mut Context<VectorApp>| {
        div()
            .id(id)
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|app, _, _, cx| app.cancel_mode(cx)))
            .child(
                svg()
                    .path("icons/cancel.svg")
                    .size(px(size))
                    .text_color(c(timeline::DANGER)),
            )
    };
    let replying = matches!(mode, Mode::Reply { .. });
    let editing = matches!(mode, Mode::Edit { .. });
    let mut boxed = div()
        .child(
            gpui::canvas(move |b, _, _| measure.set(Some(b)), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .relative()
        .flex_shrink_0()
        .px(px(8.))
        .pt(px(5. + if replying { 34. } else { 0. } + if commanding { 34. } else { 0. }))
        .pb(px(5.))
        .bg(linear_gradient(
            90.,
            linear_color_stop(c(ACCENT), 0.),
            linear_color_stop(c(ACCENT_DEEP), 1.),
        ));
    if let Mode::Reply { name, snippet, .. } = mode {
        let look = crate::inline::Look::new(INTER, 13., 18., white_a(0.45));
        let atoms = crate::preview::reply_atoms(snippet, &look);
        boxed = boxed.child(
            div()
                .absolute()
                .top(px(5.))
                .left(px(8.))
                .right(px(8.))
                .h(px(46.))
                .pl(px(12.))
                .pr(px(44.))
                .pb(px(12.))
                .rounded_t(px(10.))
                .bg(c(0x0b1510))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .w(px(3.))
                        .h(px(16.))
                        .rounded(px(2.))
                        .bg(c(ACCENT))
                        .flex_shrink_0(),
                )
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .max_w(gpui::relative(0.5))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(13.5))
                        .text_color(white_a(0.85))
                        .child("Replying to\u{a0}")
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(c(ACCENT))
                                .child(name.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(crate::inline::inline(atoms, look).single_line()),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(10.5))
                        .top(px(5.))
                        .size(px(24.))
                        .p(px(3.))
                        .child(cancel("reply-cancel", 18., cx)),
                ),
        );
    }
    if let Some(strip) = strip {
        boxed = boxed.child(
            div()
                .absolute()
                .top(px(if replying { 39. } else { 5. }))
                .left(px(8.))
                .right(px(8.))
                .h(px(46.))
                .pl(px(12.))
                .pr(px(44.))
                .pb(px(12.))
                .rounded_t(px(10.))
                .bg(c(0x0b1510))
                .flex()
                .items_center()
                .child(strip),
        );
    }
    if let Some(popup) = popup {
        boxed = boxed.child(
            gpui::deferred(
                div()
                    .absolute()
                    .bottom_full()
                    .left_0()
                    .right_0()
                    .mb(px(6.))
                    .child(popup),
            )
            .with_priority(2),
        );
    }
    boxed
        .child(
            // The buttons keep to the last line as the text grows upward.
            div()
                .relative()
                .min_h(px(45.))
                .rounded(px(10.))
                .bg(c(APP_BG))
                .flex()
                .when(busy, |d| d.items_center())
                .when(!busy, |d| d.items_end())
                .when(busy, |d| {
                    d.child(if previewing {
                        crate::recorder::preview(rec, on_rec.clone())
                    } else {
                        crate::recorder::strip(rec)
                    })
                })
                .when(!busy && !commanding && !locked, |d| d.child(if editing {
                    cancel("edit-cancel", 23., cx)
                        .w(px(38.))
                        .h(px(45.))
                        .into_any_element()
                } else {
                    attach_button(attach.is_some(), cx).into_any_element()
                }))
                .when_some(form, |d, form| d.child(div().flex_1().min_w_0().child(form)))
                .when_some(lock, |d, lock| d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .py(px(12.))
                        .pl(px(15.))
                        .pr(px(10.))
                        .font_family(RUBIK)
                        .text_size(px(15.))
                        .line_height(px(21.))
                        .text_color(white_a(0.38))
                        .child(lock),
                ))
                .when(!commanding && !locked, |d| d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .py(px(12.))
                        .pr(px(10.))
                        .when(busy, |d| d.absolute().invisible().size_0())
                        .child(input),
                ))
                .when(!busy && !commanding && !locked, |d| d.child(
                    icon(if picker.as_ref().is_some_and(|p| p.1) {
                        "icons/wink-face.svg"
                    } else {
                        "icons/smile-face.svg"
                    })
                    .id("composer-emoji")
                    .cursor_pointer()
                    .on_click(
                        cx.listener(|app, _, window, cx| app.toggle_picker(None, window, cx)),
                    ),
                ))
                .when(!locked && (!hides_mic || previewing), |d| {
                    d.child(send_or_mic(
                        (send.0 || previewing, send.1),
                        rec.state == R::Pending,
                        previewing,
                        rec.dictating,
                        send_probe,
                        on_rec.clone(),
                        cx,
                    ))
                })
                // `.self-destruct-badge`: the chat's timer, on Send's top-right corner.
                .when(timer.is_some() && send.0 && !busy && !locked, |d| {
                    d.child(
                        svg()
                            .path("ui/self-destruct.svg")
                            .absolute()
                            .bottom(px(27.))
                            .right(px(6.))
                            .size(px(11.))
                            .text_color(c(ACCENT)),
                    )
                })
                .children(crate::recorder::dot(rec, on_rec.clone()))
                .when(matches!(rec.state, R::Pending | R::Recording), |d| {
                    d.child(crate::recorder::tracker(on_rec.clone()))
                }),
        )
        .children(crate::recorder::overlays(rec))
        .when_some(attach.filter(|_| !editing && !locked), |d, (opened, mini)| {
            d.child(attachment_panel(opened, mini, cx))
        })
        // `.emoji-picker`: 10px above the composer, 16px from the right edge.
        // Under 700px it centres on the window instead.
        .when_some(picker, |d, (p, _, centre)| {
            d.child(
                gpui::deferred(
                    div()
                        .absolute()
                        .bottom(gpui::relative(1.))
                        .map(|d| if centre { d.left_0().right_0().flex().justify_center() } else { d.right(px(16.)) })
                        .pb(px(10.))
                        .child(p),
                )
                .with_priority(3),
            )
        })
}

/// `#chat-input-voice` / `#chat-input-send`: Send for a draft, the mic otherwise, swapped with a
/// spin (`swapOut` 0.15s, then `swapIn` 0.2s) when typing flips it; `flips` names each swap.
fn send_or_mic(
    (send, flips): (bool, usize),
    pending: bool,
    voice_send: bool,
    dictating: bool,
    probe: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    on_rec: crate::recorder::On,
    cx: &mut Context<VectorApp>,
) -> impl IntoElement {
    use gpui::AnimationExt as _;
    let face = |send: bool| {
        if send {
            svg()
                .path("icons/send.svg")
                .size(px(20.))
                .text_color(c(PRIMARY))
        } else {
            svg()
                .path("icons/mic-on.svg")
                .size(px(23.))
                .text_color(c(ICON_MUTED))
                .group_hover("chat-input-send", |s| s.text_color(c(0xffffff)))
        }
    };
    let icon: AnyElement = if flips == 0 {
        face(send).into_any_element()
    } else {
        face(send)
            .with_animation(
                gpui::ElementId::NamedInteger("send-swap".into(), flips as u64),
                gpui::Animation::new(std::time::Duration::from_millis(350)),
                move |s, t| {
                    let at = t * 0.35;
                    let ease = |p: f32| 1. - (1. - p).powi(2);
                    let (p, out) = if at < 0.15 {
                        (ease(at / 0.15), true)
                    } else {
                        (ease((at - 0.15) / 0.2), false)
                    };
                    let (scale, turn, alpha) = if out {
                        (1. - 0.5 * p, -p, 1. - p)
                    } else {
                        (0.5 + 0.5 * p, 1. - p, p)
                    };
                    let s = if out { face(!send) } else { s };
                    s.opacity(alpha).with_transformation(
                        gpui::Transformation::rotate(gpui::radians(
                            turn * std::f32::consts::FRAC_PI_2,
                        ))
                        .with_scaling(gpui::size(scale, scale)),
                    )
                },
            )
            .into_any_element()
    };
    div()
        .id("chat-input-send")
        .group("chat-input-send")
        .w(px(38.))
        .h(px(45.))
        .mr(px(3.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(voice_send, |d| {
            d.on_mouse_down(gpui::MouseButton::Left, |_, w, _| w.prevent_default())
                .on_click(cx.listener(|app, _, _, cx| {
                    if !app.rec.dictating {
                        app.send_voice(cx)
                    }
                }))
        })
        .when(voice_send && dictating, |d| d.opacity(0.35).cursor_default())
        .when(send && !voice_send, |d| {
            d.on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|app, _, w, cx| {
                    w.prevent_default();
                    app.send_press(cx)
                }),
            )
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|app, _, _, _| app.send_held = None))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|app, _, _, cx| app.open_send_timer(cx)),
            )
            .on_click(cx.listener(|app, _, window, cx| {
                if std::mem::take(&mut app.send_long) {
                    return;
                }
                if let Some(form) = app.cmd_form.clone() {
                    return form.update(cx, |f, cx| f.submit(window, cx));
                }
                app.input.update(cx, |i, cx| i.submit(window, cx))
            }))
        })
        .child(gpui::canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().size_full())
        .when(!send, move |d| {
            d.on_mouse_down(gpui::MouseButton::Left, move |e, w, cx| {
                w.prevent_default();
                on_rec(crate::recorder::Ui::Press(e.position), w, cx)
            })
        })
        .child(div().when(pending, |d| d.opacity(0.)).child(icon))
}

/// `#chat-input-file`: the plus, turned to a cross while the panel is open.
fn attach_button(open: bool, cx: &mut Context<VectorApp>) -> impl IntoElement {
    use gpui::AnimationExt as _;
    let turn = move |t: f32| {
        // cubic-bezier(0.34, 1.56, 0.64, 1): an overshoot, eased back.
        let k = 1. - (1. - t).powi(3) + 0.35 * (std::f32::consts::PI * t).sin() * (1. - t);
        if open { k } else { 1. - k }
    };
    div()
        .id("chat-input-file")
        .w(px(38.))
        .h(px(45.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_click(cx.listener(|app, _, _, cx| app.toggle_attach(cx)))
        .child(
            svg()
                .path("icons/plus.svg")
                .size(px(23.))
                .text_color(c(ICON_MUTED))
                .with_animation(
                    if open { "attach-open" } else { "attach-close" },
                    gpui::Animation::new(std::time::Duration::from_millis(400)),
                    move |s, t| {
                        s.with_transformation(gpui::Transformation::rotate(gpui::radians(
                            turn(t) * std::f32::consts::FRAC_PI_4,
                        )))
                    },
                ),
        )
}

/// `.attachment-panel`: File, Folder, Commands and Mini Apps, risen from behind the composer.
fn attachment_panel(
    opened: std::time::Instant,
    mini: Option<AnyElement>,
    cx: &mut Context<VectorApp>,
) -> impl IntoElement {
    if let Some(view) = mini {
        return div()
            .absolute()
            .left(px(8.))
            .right(px(8.))
            .bottom(gpui::relative(1.))
            .pb(px(10.))
            .child(
                div()
                    .id("attachment-panel")
                    .p(px(16.))
                    .bg(c(0x0c0c0c))
                    .border_1()
                    .border_color(c(DIVIDER))
                    .rounded(px(12.))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(view),
            )
            .into_any_element();
    }
    const H: f32 = 107.;
    let t = (opened.elapsed().as_secs_f32() / 0.3).min(1.);
    // cubic-bezier(0.4, 0, 0.2, 1)
    let e = 1. - (1. - t).powi(3);
    let item = |i: usize, id: &'static str, icon: &'static str, size: f32, label: &'static str| {
        let it = ((opened.elapsed().as_secs_f32() - i as f32 * 0.08) / 0.3).clamp(0., 1.);
        div()
            .id(id)
            .relative()
            .left(px(-10. * (1. - it)))
            .opacity(it)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .group(id)
            .child(
                div()
                    .size(px(58.))
                    .rounded(px(12.))
                    .bg(white_a(0.1))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .group_hover(id, |s| s.bg(white_a(0.18)).border_color(c(PRIMARY)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path(icon).size(px(size)).text_color(white_a(0.7))),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .line_height(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(white_a(0.7))
                    .whitespace_nowrap()
                    .child(label),
            )
    };
    if t < 1. || opened.elapsed().as_secs_f32() < 0.6 {
        let me = cx.entity().downgrade();
        cx.defer(move |cx| {
            me.update(cx, |_, cx| cx.notify()).ok();
        });
    }
    div()
        .absolute()
        .left(px(8.))
        .right(px(8.))
        .bottom(gpui::relative(1.))
        .h(px(H + 10.))
        .overflow_hidden()
        .child(
            div()
                .id("attachment-panel")
                .absolute()
                .left_0()
                .right_0()
                .top(px((H + 10.) * (1. - e)))
                .h(px(H))
                .opacity(e)
                .p(px(16.))
                .bg(c(0x0c0c0c))
                .border_1()
                .border_color(c(DIVIDER))
                .rounded(px(12.))
                .flex()
                .gap(px(12.))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    item(0, "attach-file", "icons/file.svg", 26., "File")
                        .on_click(cx.listener(|app, _, window, cx| app.pick_file(window, cx))),
                )
                .child(
                    item(1, "attach-folder", "icons/folder.svg", 27., "Folder")
                        .on_click(cx.listener(|app, _, window, cx| app.pick_folder(window, cx))),
                )
                .child(
                    item(2, "attach-commands", "ui/slash.svg", 22.5, "Commands")
                        .on_click(cx.listener(|app, _, window, cx| app.open_commands(window, cx))),
                )
                .child(
                    item(3, "attach-miniapps", "icons/grid-outline.svg", 22., "Mini Apps")
                        .on_click(cx.listener(|app, _, window, cx| app.show_miniapps(window, cx))),
                ),
        )
        .into_any_element()
}

impl VectorApp {
    fn toggle_attach(&mut self, cx: &mut Context<Self>) {
        self.mini = None;
        self.attach = match self.attach {
            Some(_) => None,
            None => {
                self.picker_open = false;
                Some(std::time::Instant::now())
            }
        };
        cx.notify();
    }

    /// Pick a file for the open chat and preview it before it sends.
    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attach = None;
        cx.notify();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.open_file_preview(path, window, cx))
                        .ok();
                })
                .ok();
        })
        .detach();
    }

    /// A pasted picture is written out first, so it previews and sends as any picked file does.
    fn on_paste(
        &mut self,
        pasted: &crate::composer::Pasted,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = match pasted {
            crate::composer::Pasted::Paths(p) => {
                return self.open_paths_preview(p.clone(), window, cx);
            }
            crate::composer::Pasted::Image { bytes, ext } => {
                let Ok(dir) = backend::archive::work_slot() else {
                    return;
                };
                let path = dir.join(format!("pasted_image.{ext}"));
                if std::fs::write(&path, bytes).is_err() {
                    return;
                }
                path
            }
        };
        self.open_file_preview(path, window, cx);
    }

    /// One file previews as itself; a folder, or several items, as one zip.
    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attach = None;
        cx.notify();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.open_paths_preview(paths, window, cx))
                        .ok();
                })
                .ok();
        })
        .detach();
    }

    fn open_paths_preview(
        &mut self,
        mut paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match paths.len() {
            0 => {}
            1 if paths[0].is_file() => self.open_file_preview(paths.remove(0), window, cx),
            _ => self.open_preview(window, cx, move |chat, reply, window, cx| {
                crate::filepreview::FilePreview::zipped(paths, chat, reply, window, cx)
            }),
        }
    }

    fn open_file_preview(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_preview(window, cx, move |chat, reply, window, cx| {
            crate::filepreview::FilePreview::new(path, chat, reply, window, cx)
        });
    }

    fn open_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        make: impl FnOnce(
            String,
            String,
            &mut Window,
            &mut Context<crate::filepreview::FilePreview>,
        ) -> crate::filepreview::FilePreview,
    ) {
        let Some(chat) = self.open.clone() else {
            return;
        };
        let reply = match &self.mode {
            Mode::Reply { id, .. } => id.clone(),
            _ => String::new(),
        };
        let publish = self.can_publish();
        let preview = cx.new(|cx| {
            let mut p = make(chat.to_string(), reply, window, cx);
            p.publish = publish;
            p
        });
        cx.subscribe_in(
            &preview,
            window,
            |this, _, e: &crate::filepreview::Event, window, cx| {
                this.file_preview = None;
                window.focus(&this.input.focus_handle(cx), cx);
                cx.notify();
                match e {
                    crate::filepreview::Event::Send(send) => this.send_file(send, cx),
                    crate::filepreview::Event::Close(path) => backend::archive::forget(path),
                    crate::filepreview::Event::Publish(path) => this.open_publish(path.clone(), window, cx),
                    crate::filepreview::Event::Failed(err) => {
                        this.notice("Compression Failed", err.clone(), cx)
                    }
                }
            },
        )
        .detach();
        self.file_preview = Some(preview);
        cx.notify();
    }

    fn send_file(&mut self, send: &crate::filepreview::Send, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::Reply { .. }) {
            self.mode = Mode::Normal;
        }
        if !self.live {
            backend::archive::forget(&send.path);
            return;
        }
        let crate::filepreview::Send {
            path,
            receiver,
            reply,
            name,
            compress,
            keep_metadata,
            compressed,
        } = send;
        let (path, receiver, reply, name, compress, keep_metadata) = (
            path.clone(),
            receiver.clone(),
            reply.clone(),
            name.clone(),
            *compress,
            *keep_metadata,
        );
        // The preview's own compression serves only the default strip-and-compress send.
        let wait = compressed.clone().filter(|_| compress && !keep_metadata);
        cx.spawn(async move |this, cx| {
            let precompressed = match wait {
                Some(w) => w.await,
                None => None,
            };
            let rx = backend::run(messaging::send_file(
                receiver,
                reply,
                path.clone(),
                name,
                compress,
                keep_metadata,
                precompressed,
            ));
            let done = rx.await;
            backend::archive::forget(&path);
            let err = match done {
                Ok(Ok(())) => return,
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            if err.contains("Upload cancelled") {
                return;
            }
            this.update(cx, |this, cx| this.upload_failed(&err, cx)).ok();
        })
        .detach();
    }
}

// ── profile ─────────────────────────────────────────────────────────────────────────────

impl VectorApp {
    /// A Community opens on the channel it was last left on, else its landing channel.
    fn open_community(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let (landing, members) = {
            let store = self.store.borrow();
            let Some(com) = store.community(&id) else {
                return;
            };
            (
                com.landing().map(|c| c.channel_id.clone()),
                com.members.clone(),
            )
        };
        if self.community.as_ref().is_none_or(|v| v.id != id) {
            let search = cx.new(|cx| {
                TextInput::new("Search Members...", cx).with_look(crate::community::member_search_look(self.is_slim))
            });
            cx.subscribe(&search, |_, _, _: &crate::composer::InputChanged, cx| {
                cx.notify()
            })
            .detach();
            self.community = Some(crate::community::View {
                id: id.clone(),
                unlisted: self.store.borrow().fixture_unlisted.get(&id).cloned().unwrap_or_default(),
                loading: self.live && members.is_empty(),
                members,
                admins: Vec::new(),
                panel: true,
                search,
                folded: HashSet::new(),
            });
            self.check_raid(id.clone(), cx);
            if self.live {
                let named = backend::community::nicknames(&id);
                self.store.borrow_mut().nicknames.insert(id.clone(), named);
                let rx = backend::run(backend::community::roster(id.clone()));
                cx.spawn(async move |this, cx| {
                    let read = rx.await;
                    this.update(cx, |this, cx| {
                        if let Some(v) = this.community.as_mut().filter(|v| v.id == id) {
                            if let Ok((members, admins, unlisted)) = read {
                                v.members = members;
                                v.admins = admins;
                                v.unlisted = unlisted;
                            }
                            v.loading = false;
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        self.settings = None;
        self.profile = None;
        self.new_chat = None;
        self.start = None;
        match landing {
            Some(ch) => self.open_channel(ch, window, cx),
            None => {
                self.open = None;
                cx.notify();
            }
        }
    }

    fn open_channel(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.borrow().chat(&id).is_none() {
            let rx = backend::run(messaging::chat_snapshot(id.clone()));
            let want = id.clone();
            cx.spawn_in(window, async move |this, cx| {
                let Ok(Some(chat)) = rx.await else { return };
                this.update_in(cx, |this, window, cx| {
                    {
                        let mut store = this.store.borrow_mut();
                        if store.chat(&chat.id).is_none() {
                            store.chats.push(chat);
                            store.resort();
                        }
                    }
                    if this.open.as_deref() == Some(want.as_str()) {
                        this.open_chat(want.into(), window, cx);
                    }
                })
                .ok();
            })
            .detach();
        }
        self.open_chat(id.clone().into(), window, cx);
        self.voice_present(&id);
        let community = self.store.borrow().community_of(&id).map(|c| c.community_id.clone());
        if let Some(community) = community {
            self.refresh_timeouts(&community, cx);
        }
        if self.live {
            let _ = backend::run(backend::community::sync_channel(id, None));
        }
        cx.notify();
    }

    fn on_community(
        &mut self,
        act: crate::community::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::community::Act;
        if self.is_slim {
            match &act {
                Act::Open(id) => {
                    self.slim.drawer = self.store.borrow().community(id).is_some_and(|c| c.channels.len() > 1);
                }
                Act::Channel(_) | Act::ChannelChat(_) | Act::Dm(_) => self.slim.drawer = false,
                Act::Home => {
                    self.slim.drawer = false;
                    self.open = None;
                }
                _ => {}
            }
        }
        match act {
            Act::Open(id) => self.open_community(id, window, cx),
            Act::Home => self.leave_community(),
            Act::Dm(id) => {
                self.leave_community();
                self.settings = None;
                self.nexus = None;
                self.open_chat(id.into(), window, cx);
            }
            Act::Channel(id) => {
                let voice = {
                    let store = self.store.borrow();
                    store.community_of(&id).filter(|c| c.channel(&id).is_some_and(|ch| ch.is_voice())).map(|c| c.community_id.clone())
                };
                if let Some(community) = voice
                    && self.voice.session.as_ref().is_none_or(|s| s.channel_id != id)
                {
                    self.voice_join(community, id.clone(), cx);
                }
                self.open_channel(id, window, cx)
            }
            Act::ChannelChat(id) => self.open_channel(id, window, cx),
            Act::Fold(key) => {
                if let Some(v) = self.community.as_mut()
                    && !v.folded.remove(&key)
                {
                    v.folded.insert(key);
                }
            }
            Act::MessagesMenu(at) => self.messages_menu(at, cx),
            Act::Panel if self.is_slim => {
                self.slim.details = match self.slim.details.take() {
                    Some(_) => None,
                    None => self.open.as_ref().map(|id| id.to_string()),
                };
            }
            Act::Panel => {
                if let Some(v) = self.community.as_mut() {
                    v.panel = !v.panel;
                }
            }
            Act::HeadMenu => {
                if let Some(id) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.community_menu(
                        id,
                        gpui::point(px(self.rail_mode(window).width() - 1.), px(CHROME_H + 61. + 4.)),
                        cx,
                    );
                }
            }
            Act::BannerMenu(at) => {
                if let Some(id) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.show_menu(
                        at,
                        vec![(
                            kit::MenuItem::new("Hide Banner", "eye-off"),
                            MenuAct::BannerHidden(id, true),
                        )],
                        cx,
                    );
                }
            }
            Act::ChannelMenu(channel, at) => {
                if let Some(id) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.channel_menu(id, channel, at, cx);
                }
            }
            Act::Create => self.open_create_community(window, cx),
            Act::RailFolder(id) => self.rail_toggle(id, cx),
            Act::RailMenu(Some(key), _, at) => self.community_menu(key, at, cx),
            Act::RailMenu(None, Some(folder), at) => self.rail_folder_menu(folder, at, cx),
            Act::RailMenu(None, None, _) => {}
            Act::RailDrop(source, target) => self.rail_drop(source, target, cx),
            Act::Invite => {
                if let Some(id) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.open_invite(id, window, cx);
                }
            }
            Act::AddChannel(category) => {
                if let Some(id) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.open_create_channel(id, Some(category), false, window, cx);
                }
            }
            Act::CategoryMenu(category, at) => {
                if let Some(id) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.category_menu(id, category, at, cx);
                }
            }
            Act::ChatMenu => {
                if let Some(chat) = self.open.clone() {
                    self.open_chat_menu(chat.to_string(), cx);
                }
            }
            Act::EditChannel(channel) => {
                if let Some(id) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.open_channel_editor(id, channel, false, window, cx);
                }
            }
            Act::Member(npub, key) => {
                let community = self.community.as_ref().map(|v| v.id.clone());
                let at = self.anchors.borrow().get(&key).copied();
                self.show_mini(npub, community, at, cx);
            }
            Act::Unlisted(why) => {
                let departed = why == "departed";
                self.notice(
                    if departed { "Departed Member" } else { "Unannounced Member" },
                    if departed {
                        "They left or were removed, yet are still posting. This may be suspicious, keep an eye on them."
                    } else {
                        "They joined and are posting without ever publishing a join event. This may be suspicious, keep an eye on them."
                    }
                    .into(),
                    cx,
                );
            }
            Act::MemberMenu(npub, at) => {
                if let Some(community) = self.community.as_ref().map(|v| v.id.clone()) {
                    self.member_menu(community, npub, at, cx);
                }
            }
        }
        cx.notify();
    }

    fn unread_dms(&self) -> Vec<String> {
        let store = self.store.borrow();
        let me = store.my_npub.clone();
        store
            .chats
            .iter()
            .filter(|c| store.listed(c) && c.id != me && store.community_of(&c.id).is_none() && store.unread_count(c) > 0)
            .map(|c| c.id.clone())
            .collect()
    }

    /// `openMessagesMenu`: the Messages door's catch-up.
    pub(super) fn messages_menu(&mut self, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let none = self.unread_dms().is_empty();
        let rows = vec![(kit::MenuItem::new("Mark All as Read", "check").disabled(none), MenuAct::ReadAllDms)];
        self.show_menu(at, rows, cx);
    }

    fn show_menu(
        &mut self,
        at: gpui::Point<gpui::Pixels>,
        rows: Vec<(kit::MenuItem, MenuAct)>,
        cx: &mut Context<Self>,
    ) {
        self.menu_trail.clear();
        let (items, acts) = rows.into_iter().unzip();
        self.menu = Some((at, items, acts));
        cx.notify();
    }

    /// `notifyMenuItems`: the level and mute submenus for a Community, a channel or a DM.
    fn notify_rows(&self, scope: &str, community: Option<&str>) -> Vec<(kit::MenuItem, MenuAct)> {
        const LEVELS: [(&str, &str); 3] = [
            ("all", "All messages"),
            ("mentions", "Mentions"),
            ("nothing", "Nothing"),
        ];
        const MUTES: [(&str, i64); 6] = [
            ("For 15 minutes", 15 * 60_000),
            ("For 1 hour", 60 * 60_000),
            ("For 3 hours", 3 * 60 * 60_000),
            ("For 8 hours", 8 * 60 * 60_000),
            ("For 24 hours", 24 * 60 * 60_000),
            ("Until I turn it back on", -1),
        ];
        let views = if self.live {
            backend::prefs::notify_views(community.map(str::to_string), vec![scope.to_string()])
        } else {
            [community, Some(scope)]
                .into_iter()
                .flatten()
                .map(|id| backend::prefs::NotifyView {
                    scope_id: id.into(),
                    effective_level: "mentions",
                    ..Default::default()
                })
                .collect()
        };
        let Some(me) = views.iter().find(|v| v.scope_id == scope).cloned() else {
            return vec![(
                kit::MenuItem::new("Notification settings unavailable", "").disabled(true),
                MenuAct::Nothing,
            )];
        };
        let parent = community
            .and_then(|c| views.iter().find(|v| v.scope_id == c))
            .filter(|_| community != Some(scope));
        let label = |v: &str| {
            LEVELS
                .iter()
                .find(|l| l.0 == v)
                .map(|l| l.1)
                .unwrap_or("All messages")
        };
        let mut levels: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        if let Some(p) = parent {
            levels.push((
                kit::MenuItem::new(
                    format!("Use Community Default ({})", label(p.effective_level)),
                    "",
                )
                .checked(me.level.is_none()),
                MenuAct::NotifyLevel(scope.into(), None),
            ));
        }
        let ticked = me.level.or(if parent.is_some() {
            None
        } else {
            Some(me.effective_level)
        });
        for (value, name) in LEVELS {
            levels.push((
                kit::MenuItem::new(name, "").checked(ticked == Some(value)),
                MenuAct::NotifyLevel(scope.into(), Some(value)),
            ));
        }
        if community == Some(scope) {
            let off = me.everyone_muted_globally;
            let suppressed = me.suppress_everyone == Some(true);
            levels.push((kit::MenuItem::header("@everyone"), MenuAct::Nothing));
            levels.push((
                kit::MenuItem::new("Allow", "")
                    .checked(!suppressed)
                    .disabled(off)
                    .hint(off.then(|| "off in App Settings".into())),
                MenuAct::SuppressEveryone(scope.into(), false),
            ));
            levels.push((
                kit::MenuItem::new("Suppress", "")
                    .checked(suppressed)
                    .disabled(off),
                MenuAct::SuppressEveryone(scope.into(), true),
            ));
        }
        let mut mutes: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        if me.muted && me.mute_until == 0 {
            mutes.push((
                kit::MenuItem::new("Muted by the community", "").disabled(true),
                MenuAct::Nothing,
            ));
            mutes.push((kit::MenuItem::divider(), MenuAct::Nothing));
        } else if me.muted {
            mutes.push((
                kit::MenuItem::new("Unmute", "volume-max"),
                MenuAct::NotifyMute(scope.into(), 0),
            ));
            mutes.push((kit::MenuItem::divider(), MenuAct::Nothing));
        }
        for (name, ms) in MUTES {
            mutes.push((
                kit::MenuItem::new(name, ""),
                MenuAct::NotifyMute(scope.into(), ms),
            ));
        }
        let hint = (me.muted && me.mute_until > 0).then(|| {
            let at = chrono::Local.timestamp_millis_opt(me.mute_until).single();
            let today = chrono::Local::now().date_naive();
            at.map(|t| {
                if t.date_naive() == today {
                    format!("until {}", t.format("%H:%M"))
                } else {
                    format!("until {}", t.format("%a %H:%M"))
                }
            })
            .unwrap_or_default()
        });
        let sub = |label: &str, _icon: &str, rows: Vec<(kit::MenuItem, MenuAct)>| {
            let (items, acts): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
            MenuAct::Sub(SharedString::from(label.to_string()), items, acts)
        };
        let mute_label = if me.muted { "Muted" } else { "Mute" };
        vec![
            (
                kit::MenuItem::new("Notification Settings", "bell").sub(),
                sub("Notification Settings", "bell", levels),
            ),
            (
                kit::MenuItem::new(
                    mute_label,
                    if me.muted {
                        "volume-mute"
                    } else {
                        "volume-max"
                    },
                )
                .sub()
                .hint(hint.clone()),
                sub(mute_label, "", mutes),
            ),
        ]
    }

    /// `openCommunityMenu`: the Community's own menu, hung off its header.
    fn community_menu(
        &mut self,
        id: String,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let (name, owner, unread) = {
            let store = self.store.borrow();
            let Some(com) = store.community(&id) else {
                return;
            };
            let unread = com.channels.iter().any(|ch| {
                store
                    .chat(&ch.channel_id)
                    .is_some_and(|c| store.unread_count(c) > 0)
            });
            (com.name.clone(), com.is_owner, unread)
        };
        let mut rows: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        if unread {
            rows.push((
                kit::MenuItem::new("Mark as Read", "check"),
                MenuAct::CommunityRead(id.clone()),
            ));
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        }
        rows.push((
            kit::MenuItem::new("Invite People", "add-user"),
            MenuAct::InvitePeople(id.clone()),
        ));
        let caps = if self.live {
            backend::community::capabilities(&id)
        } else {
            [
                "manage_metadata",
                "manage_channels",
                "change_nickname",
                "ban",
                "manage_roles",
                "view_audit_log",
            ]
            .map(String::from)
            .into()
        };
        if caps.contains("manage_channels") {
            rows.push((
                kit::MenuItem::new("Create Channel", "channel-hash"),
                MenuAct::CreateChannel {
                    community: id.clone(),
                    category: None,
                },
            ));
            rows.push((
                kit::MenuItem::new("Create Category", "plus"),
                MenuAct::CreateCategory(id.clone()),
            ));
        }
        if caps.contains("change_nickname") {
            rows.push((
                kit::MenuItem::new("Change Nickname", "edit"),
                MenuAct::CommunityNickname(id.clone()),
            ));
        }
        if ["manage_metadata", "ban", "manage_roles", "view_audit_log"]
            .iter()
            .any(|c| caps.contains(*c))
        {
            rows.push((
                kit::MenuItem::new("Community Settings", "settings"),
                MenuAct::CommunitySettings(id.clone()),
            ));
        }
        rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        rows.extend(self.notify_rows(&id, Some(&id)));
        if self.store.borrow().banner(&id).is_some()
            && self.store.borrow().hidden_banners.contains(&id)
        {
            rows.push((
                kit::MenuItem::new("Show Banner", "eye"),
                MenuAct::BannerHidden(id.clone(), false),
            ));
        }
        if caps.contains("ban") {
            let raid = self.raid.get(&id).filter(|r| r.0);
            let mut item = kit::MenuItem::new("Moderation", "warning");
            if let Some((_, n)) = raid {
                item = item.danger().hint(Some(format!("{n} flagged").into()));
            }
            rows.push((item, MenuAct::Moderation(id.clone())));
        }
        rows.extend(self.start_pin_row(format!("c/{id}")));
        rows.push((
            kit::MenuItem::new("Copy Link", "share"),
            MenuAct::Copy(format!("https://vectorapp.io/c/{id}")),
        ));
        if self.advanced() && !self.store.borrow().stream_hidden(&id) {
            rows.push((
                kit::MenuItem::new("Copy Community ID", "channel-hash"),
                MenuAct::CopyId(id.clone(), "Community"),
            ));
        }
        rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        rows.push((
            kit::MenuItem::new(
                if owner {
                    "Delete Community"
                } else {
                    "Leave Community"
                },
                "x-user",
            )
            .danger(),
            MenuAct::LeaveCommunity { id, name, owner },
        ));
        self.show_menu(at, rows, cx);
    }

    /// `openChannelMenu`: right-click on a channel row.
    fn channel_menu(
        &mut self,
        community: String,
        channel: String,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let unread = {
            let store = self.store.borrow();
            store
                .chat(&channel)
                .is_some_and(|c| store.unread_count(c) > 0)
        };
        let mut rows: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        if unread {
            rows.push((
                kit::MenuItem::new("Mark as Read", "check"),
                MenuAct::ChannelRead(channel.clone()),
            ));
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        }
        rows.extend(self.notify_rows(&channel, Some(&community)));
        let manage = self.community_caps(&community).contains("manage_channels");
        if manage {
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
            rows.push((
                kit::MenuItem::new("Edit Channel", "edit"),
                MenuAct::EditChannel {
                    community: community.clone(),
                    channel: channel.clone(),
                    category: false,
                },
            ));
        }
        rows.extend(self.start_pin_row(format!("c/{community}/{channel}")));
        rows.push((
            kit::MenuItem::new("Copy Link", "share"),
            MenuAct::Copy(format!("https://vectorapp.io/c/{community}/{channel}")),
        ));
        if self.advanced() && !self.store.borrow().stream_hidden(&channel) {
            rows.push((
                kit::MenuItem::new("Copy Channel ID", "channel-hash"),
                MenuAct::CopyId(channel.clone(), "Channel"),
            ));
        }
        let primary = self
            .store
            .borrow()
            .community(&community)
            .is_some_and(|c| c.primary_channel.as_deref() == Some(channel.as_str()));
        if manage && !primary {
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
            rows.push((
                kit::MenuItem::new("Delete Channel", "x").danger(),
                MenuAct::DeleteChannel {
                    community,
                    channel,
                    category: false,
                },
            ));
        }
        self.show_menu(at, rows, cx);
    }

    /// `openCategoryMenu`: what a manager can do with a Category.
    fn category_menu(
        &mut self,
        community: String,
        category: String,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !self.community_caps(&community).contains("manage_channels") {
            return;
        }
        let rows = vec![
            (
                kit::MenuItem::new("Create Channel", "plus"),
                MenuAct::CreateChannel {
                    community: community.clone(),
                    category: Some(category.clone()),
                },
            ),
            (
                kit::MenuItem::new("Edit Category", "edit"),
                MenuAct::EditChannel {
                    community: community.clone(),
                    channel: category.clone(),
                    category: true,
                },
            ),
            (kit::MenuItem::divider(), MenuAct::Nothing),
            (
                kit::MenuItem::new("Delete Category", "x").danger(),
                MenuAct::DeleteChannel {
                    community,
                    channel: category,
                    category: true,
                },
            ),
        ];
        self.show_menu(at, rows, cx);
    }

    fn community_caps(&self, id: &str) -> HashSet<String> {
        if self.live {
            backend::community::capabilities(id)
        } else {
            [
                "manage_metadata",
                "manage_channels",
                "change_nickname",
                "ban",
                "manage_roles",
                "view_audit_log",
                "pin_messages",
            ]
            .map(String::from)
            .into()
        }
    }

    fn advanced(&self) -> bool {
        if self.live {
            backend::prefs::synced_settings().advanced
        } else {
            std::env::var_os("VECTOR_ADVANCED").is_some()
        }
    }

    fn open_create_channel(
        &mut self,
        community: String,
        category: Option<String>,
        is_category: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let category_name = category.as_ref().and_then(|id| {
            let store = self.store.borrow();
            store
                .community(&community)?
                .categories
                .iter()
                .find(|c| &c.category_id == id)
                .map(|c| c.name.clone())
        });
        let name = cx.new(|cx| {
            TextInput::new(crate::createchannel::placeholder(is_category, false), cx)
                .with_look(crate::composer::Look {
                    size: px(15.),
                    line_height: px(18.),
                    ..kit::field_look()
                })
                .keep_on_submit()
        });
        cx.subscribe_in(&name, window, |app, _, _: &SendText, window, cx| {
            app.on_create_channel(crate::createchannel::Act::Submit, window, cx)
        })
        .detach();
        cx.subscribe_in(&name, window, |app, _, _: &CancelInput, window, cx| {
            app.on_create_channel(crate::createchannel::Act::Close, window, cx)
        })
        .detach();
        cx.subscribe(&name, |app, _, _: &crate::composer::InputChanged, cx| {
            if let Some(m) = app.create_channel.as_mut() {
                m.error = None;
            }
            cx.notify();
        })
        .detach();
        window.focus(&name.focus_handle(cx), cx);
        self.create_channel = Some(crate::createchannel::CreateChannel {
            community,
            category,
            category_name,
            is_category,
            voice: false,
            private: false,
            name,
            busy: false,
            error: None,
        });
        cx.notify();
    }

    fn on_create_channel(
        &mut self,
        act: crate::createchannel::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::createchannel::Act;
        let Some(m) = self.create_channel.as_mut() else {
            return;
        };
        match act {
            Act::Close => self.create_channel = None,
            Act::Private => m.private = !m.private,
            Act::Voice(voice) => {
                m.voice = voice;
                let ph = crate::createchannel::placeholder(m.is_category, voice);
                m.name.update(cx, |i, cx| i.set_placeholder(ph, cx));
                window.focus(&m.name.focus_handle(cx), cx);
            }
            Act::Submit => {
                if !m.valid(cx) || m.busy {
                    return;
                }
                let name = m.name.read(cx).text().trim().to_string();
                if !self.live {
                    self.create_channel = None;
                    cx.notify();
                    return;
                }
                m.busy = true;
                let (community, category, is_category, voice, private) = (
                    m.community.clone(),
                    m.category.clone(),
                    m.is_category,
                    m.voice,
                    m.private,
                );
                let rx = backend::run(async move {
                    if is_category {
                        backend::community::create_category(community, name)
                            .await
                            .map(|_| None)
                    } else {
                        backend::community::create_channel(
                            community, name, private, category, voice,
                        )
                        .await
                        .map(Some)
                    }
                });
                cx.spawn_in(window, async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update_in(cx, |this, window, cx| {
                        match result {
                            Ok(channel) => {
                                this.create_channel = None;
                                this.reload_communities(cx);
                                if let Some(ch) = channel.filter(|_| !voice) {
                                    this.open_channel(ch, window, cx);
                                }
                            }
                            Err(e) => {
                                if let Some(m) = this.create_channel.as_mut() {
                                    m.busy = false;
                                    m.error = Some(e);
                                }
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn ask_leave_community(
        &mut self,
        id: String,
        name: String,
        owner: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut popup = kit::Popup::notice(if owner {
            "Delete this community?"
        } else {
            "Leave Community"
        });
        popup.cancel = Some("Cancel".into());
        if owner {
            popup.text = Some(format!("This permanently ends \"<b>{name}</b>\" for everyone, including you. No new messages can be sent and no one can rejoin. People can still delete their own past messages. This cannot be undone.<br><br>Type the community name to confirm:").into());
            let input = cx.new(|cx| {
                TextInput::new(name.clone(), cx)
                    .with_look(kit::field_look())
                    .keep_on_submit()
                    .submit_empty()
            });
            cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| {
                app.answer_popup(true, window, cx)
            })
            .detach();
            cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
                app.answer_popup(false, window, cx)
            })
            .detach();
            window.focus(&input.focus_handle(cx), cx);
            popup.input = Some(input);
        } else {
            popup.text = Some(
                format!("Leave \"<b>{name}</b>\"? You'll need a new invite to rejoin.").into(),
            );
        }
        self.popup = Some((popup, PopupAct::LeaveCommunity { id, name, owner }));
        cx.notify();
    }

    fn open_invite(&mut self, community: String, window: &mut Window, cx: &mut Context<Self>) {
        let (name, mut excluded, owner) = {
            let store = self.store.borrow();
            let Some(com) = store.community(&community) else {
                return;
            };
            (
                com.name.clone(),
                com.members.iter().cloned().collect::<HashSet<String>>(),
                com.owner_npub.clone(),
            )
        };
        excluded.extend(owner);
        if let Some(v) = self.community.as_ref().filter(|v| v.id == community) {
            excluded.extend(v.members.iter().cloned());
            excluded.extend(v.admins.iter().cloned());
        }
        let filter = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                weight: FontWeight::MEDIUM,
                placeholder: white_a(0.35),
                ..kit::field_look()
            })
        });
        cx.subscribe(&filter, |_, _, _: &crate::composer::InputChanged, cx| {
            cx.notify()
        })
        .detach();
        cx.subscribe_in(&filter, window, |app, _, _: &CancelInput, window, cx| {
            app.on_invite(crate::invite::Act::Close, window, cx)
        })
        .detach();
        let contacts: Option<HashSet<String>> = if self.live {
            None
        } else {
            Some(
                self.store
                    .borrow()
                    .chats
                    .iter()
                    .filter(|c| matches!(c.chat_type, vector_core::ChatType::DirectMessage))
                    .map(|c| c.id.clone())
                    .collect(),
            )
        };
        self.invite = Some(crate::invite::Invite {
            community: community.clone(),
            name,
            links: Vec::new(),
            public: false,
            others: Vec::new(),
            expanded: false,
            copied: None,
            revoking: None,
            creating: false,
            busy: false,
            status: None,
            filter,
            picked: HashSet::new(),
            contacts,
            excluded,
        });
        if !self.live {
            if let Some(m) = self.invite.as_mut() {
                let link = |t: &str, label: Option<&str>, joins| crate::invite::Link {
                    url: format!("https://vectorapp.io/invite/{}", t.repeat(20)),
                    token: format!("tok{}", if label.is_some() { 1 } else { 2 }),
                    label: label.map(String::from),
                    joins,
                };
                m.links = vec![link("Ab3", Some("Reddit"), 3), link("Zx9", None, 0)];
                m.public = true;
            }
        } else {
            self.reload_invite_links(cx);
            let id = community.clone();
            let rx = backend::run(async move {
                let banned = backend::community::banned(&id);
                let (members, admins) = backend::community::members(id).await;
                let contacts = vector_core::db::events::get_dm_contact_npubs().unwrap_or_default();
                (banned, members, admins, contacts)
            });
            cx.spawn(async move |this, cx| {
                let Ok((banned, members, admins, contacts)) = rx.await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    if let Some(m) = this.invite.as_mut().filter(|m| m.community == community) {
                        m.excluded
                            .extend(banned.into_iter().chain(members).chain(admins));
                        m.contacts = Some(contacts.into_iter().collect());
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        window.focus(&self.invite.as_ref().unwrap().filter.focus_handle(cx), cx);
        cx.notify();
    }

    fn reload_invite_links(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.invite.as_ref().map(|m| m.community.clone()) else {
            return;
        };
        let rx = backend::run(backend::community::invite_links(id.clone()));
        cx.spawn(async move |this, cx| {
            let Ok(got) = rx.await else { return };
            this.update(cx, |this, cx| {
                let Some(m) = this.invite.as_mut().filter(|m| m.community == id) else {
                    return;
                };
                m.links = got
                    .mine
                    .into_iter()
                    .map(|r| crate::invite::Link {
                        url: r.url,
                        token: r.token,
                        label: r.label,
                        joins: r.join_count,
                    })
                    .collect();
                m.public = got.public;
                m.others = got.others;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn ask_invite_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut popup = kit::Popup::notice("Label this link");
        popup.icon = None;
        popup.text = Some("Optional. A label lets you see which link people join through (e.g. \"Reddit\", \"Twitter\"). Leave blank to skip.".into());
        popup.confirm = "Create link".into();
        popup.cancel = Some("Cancel".into());
        let input = cx.new(|cx| {
            TextInput::new("Label (optional)", cx)
                .with_look(kit::field_look())
                .keep_on_submit()
                .submit_empty()
        });
        cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| {
            app.answer_popup(true, window, cx)
        })
        .detach();
        cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
            app.answer_popup(false, window, cx)
        })
        .detach();
        window.focus(&input.focus_handle(cx), cx);
        popup.input = Some(input);
        self.popup = Some((popup, PopupAct::InviteLabel));
        cx.notify();
    }

    fn create_invite_link(&mut self, label: Option<String>, cx: &mut Context<Self>) {
        let Some(m) = self.invite.as_mut() else {
            return;
        };
        m.busy = true;
        m.creating = true;
        m.status = Some(("Creating link...".into(), false));
        if !self.live {
            m.busy = false;
            m.creating = false;
            m.status = None;
            return cx.notify();
        }
        let rx = backend::run(backend::community::create_public_invite(
            m.community.clone(),
            label,
        ));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                if let Some(m) = this.invite.as_mut() {
                    m.busy = false;
                    m.creating = false;
                    m.status = match res {
                        Ok(Err(e)) => Some((e, true)),
                        _ => None,
                    };
                }
                this.reload_invite_links(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn revoke_invite_link(&mut self, token: String, privatize: bool, cx: &mut Context<Self>) {
        let Some(m) = self.invite.as_mut() else {
            return;
        };
        m.busy = true;
        m.revoking = Some(token.clone());
        if !privatize {
            m.status = Some(("Revoking…".into(), false));
        }
        if !self.live {
            m.links.retain(|l| l.token != token);
            m.public = !m.links.is_empty();
            m.busy = false;
            m.revoking = None;
            m.status = None;
            return cx.notify();
        }
        let rx = backend::run(backend::community::revoke_invite(
            m.community.clone(),
            token,
        ));
        if privatize {
            self.rekey = Some(crate::rekey::Rekey::new("Making community private"));
        }
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            if privatize && matches!(res, Ok(Ok(_))) {
                this.update(cx, |this, cx| {
                    if let Some(r) = this.rekey.as_mut() {
                        r.set(100., Some("Community is now private".into()));
                    }
                    cx.notify();
                })
                .ok();
                cx.background_executor().timer(std::time::Duration::from_millis(700)).await;
            }
            this.update(cx, |this, cx| {
                this.rekey = None;
                if let Some(m) = this.invite.as_mut() {
                    m.busy = false;
                    m.revoking = None;
                    m.status = None;
                    if let Ok(Err(e)) = res {
                        if privatize {
                            this.notice("Couldn't make private", e, cx);
                        } else if let Some(m) = this.invite.as_mut() {
                            m.status = Some((e, true));
                        }
                    }
                }
                this.reload_invite_links(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_comm_settings(
        &mut self,
        community: String,
        section: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (name, description, relays, voice) = {
            let store = self.store.borrow();
            let Some(com) = store.community(&community) else {
                return;
            };
            (
                com.name.clone(),
                com.description.clone(),
                com.relays.clone(),
                com.voice_relays.clone(),
            )
        };
        let caps: HashSet<String> = if self.live {
            backend::community::capabilities(&community)
        } else {
            ["manage_metadata", "ban", "manage_roles", "view_audit_log"]
                .map(String::from)
                .into()
        };
        let look = || crate::composer::Look {
            size: px(15.),
            line_height: px(18.),
            weight: FontWeight::MEDIUM,
            ..kit::field_look()
        };
        let field = |placeholder: &str, text: &str, max: usize, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut t = TextInput::new(placeholder.to_string(), cx)
                    .with_look(look())
                    .max_chars(max)
                    .keep_on_submit();
                t.set_text(text, cx);
                t
            })
        };
        let search = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                size: px(14.),
                line_height: px(17.),
                weight: FontWeight::NORMAL,
                placeholder: c(0xb2b2b2),
                ..kit::field_look()
            })
        });
        let name_in = field("Community name", &name, crate::commsettings::NAME_MAX, cx);
        let description_in = cx.new(|cx| {
            let mut t = TextInput::new("Tell people what brings everyone here", cx)
                .with_look(crate::composer::Look {
                    size: px(15.),
                    line_height: px(22.5),
                    weight: FontWeight::NORMAL,
                    ..kit::field_look()
                })
                .multiline(px(114.))
                .max_chars(crate::commsettings::DESCRIPTION_MAX);
            t.set_text(&description, cx);
            t
        });
        let add_look = || crate::composer::Look {
            size: px(14.),
            line_height: px(17.),
            weight: FontWeight::MEDIUM,
            placeholder: c(0x5c5c5c),
            ..kit::field_look()
        };
        let relay_in = cx.new(|cx| {
            TextInput::new("wss://relay.example.com", cx)
                .with_look(add_look())
                .max_chars(256)
                .keep_on_submit()
        });
        let voice_in = cx.new(|cx| {
            TextInput::new("https://relay.example.com", cx)
                .with_look(add_look())
                .max_chars(256)
                .keep_on_submit()
        });
        let audit_query = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                size: px(16.),
                line_height: px(19.),
                weight: FontWeight::MEDIUM,
                placeholder: white_a(0.6),
                ..kit::field_look()
            })
        });
        let ban_query = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                size: px(16.),
                line_height: px(19.),
                weight: FontWeight::MEDIUM,
                placeholder: white_a(0.6),
                ..kit::field_look()
            })
        });
        let role_name = field("new role", "", crate::roles::NAME_MAX, cx);
        let role_query = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                size: px(16.),
                line_height: px(19.),
                weight: FontWeight::MEDIUM,
                placeholder: white_a(0.6),
                ..kit::field_look()
            })
        });
        for input in [
            &search,
            &name_in,
            &description_in,
            &relay_in,
            &voice_in,
            &ban_query,
            &audit_query,
            &role_name,
            &role_query,
        ] {
            cx.subscribe(input, |app, _, _: &crate::composer::InputChanged, cx| {
                if let Some(m) = app.comm_settings.as_mut() {
                    m.relay_error = None;
                    m.voice_error = None;
                }
                cx.notify()
            })
            .detach();
            cx.subscribe_in(input, window, |app, _, _: &CancelInput, window, cx| {
                app.on_comm_settings(crate::commsettings::Act::Close, window, cx)
            })
            .detach();
        }
        cx.subscribe_in(&relay_in, window, |app, _, _: &SendText, window, cx| {
            app.on_comm_settings(crate::commsettings::Act::AddRelay, window, cx)
        })
        .detach();
        cx.subscribe_in(&voice_in, window, |app, _, _: &SendText, window, cx| {
            app.on_comm_settings(crate::commsettings::Act::AddVoice, window, cx)
        })
        .detach();
        let bans = if self.live {
            backend::community::banned(&community)
        } else {
            self.store
                .borrow()
                .profiles
                .values()
                .filter(|p| &*p.name == "frank" || &*p.name == "heidi")
                .map(|p| p.id.to_string())
                .collect()
        };
        self.comm_settings = Some(crate::commsettings::CommSettings {
            community,
            section,
            anchor: None,
            search,
            scroll: gpui::ScrollHandle::new(),
            caps,
            saved: crate::commsettings::Saved {
                name,
                description,
                relays: relays.clone(),
                voice: voice.clone(),
            },
            name: name_in,
            description: description_in,
            relays,
            voice,
            relay_input: relay_in,
            voice_input: voice_in,
            relay_edit: None,
            relay_error: None,
            voice_error: None,
            bans,
            ban_query,
            ban_sel: HashSet::new(),
            unbanning: false,
            audit: crate::commsettings::Audit {
                action: "all",
                shown: crate::commsettings::AUDIT_PAGE,
                ..Default::default()
            },
            audit_query,
            advanced: self.advanced(),
            report: None,
            roles: Default::default(),
            role_name,
            role_query,
            role_hex: None,
            saving: false,
            progress: None,
            icon_pick: None,
            icon_removed: false,
            banner_pick: None,
            banner_removed: false,
            warn_until: None,
        });
        if section == "audit" {
            self.load_audit(cx);
        }
        if section == "roles" {
            self.load_roles(None, cx);
        }
        cx.notify();
    }

    /// The roles view, and the members the editor can hand a role to; `reopen` reopens that
    /// role (or `everyone`) once the fresh view lands.
    fn load_roles(&mut self, reopen: Option<String>, cx: &mut Context<Self>) {
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        m.roles.loaded = true;
        let id = m.community.clone();
        let (view, members) = if self.live {
            (backend::community::roles_view(&id).ok(), None)
        } else {
            let store = self.store.borrow();
            (
                store.fixture_roles.clone(),
                store.community(&id).map(|c| c.members.clone()),
            )
        };
        if let Some(view) = view {
            m.roles.view = Some(view);
        }
        m.roles.order = None;
        if let Some(members) = members {
            m.roles.members = members;
        }
        if let Some(key) = reopen {
            if m.roles.edit.is_some() {
                self.edit_role(Some(key), cx);
            }
        }
        if self.live {
            let rx = backend::run(async move { backend::community::members(id.clone()).await });
            cx.spawn(async move |this, cx| {
                let Ok((members, admins)) = rx.await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    if let Some(m) = this.comm_settings.as_mut() {
                        let mut all = admins;
                        all.extend(
                            members
                                .into_iter()
                                .filter(|n| !all.contains(n))
                                .collect::<Vec<_>>(),
                        );
                        m.roles.members = all;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// Opens a role in the editor: an id, `everyone`, None for a new role, or closes it.
    fn edit_role(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        use crate::roles::{Draft, Edit, role_of};
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        let edit = match key.as_deref() {
            Some("everyone") => {
                let perms = m
                    .roles
                    .view
                    .as_ref()
                    .map(|v| crate::roles::num(&v["everyone"]))
                    .unwrap_or(0);
                let d = Draft {
                    permissions: perms,
                    ..Default::default()
                };
                Some(Edit {
                    id: Some("everyone".into()),
                    everyone: true,
                    draft: d.clone(),
                    base: d,
                })
            }
            Some(id) => m.roles.find(id).map(|r| {
                let d = role_of(r);
                Edit {
                    id: Some(id.to_string()),
                    everyone: false,
                    draft: d.clone(),
                    base: d,
                }
            }),
            None => {
                let d = Draft {
                    name: "new role".into(),
                    ..Default::default()
                };
                Some(Edit {
                    id: None,
                    everyone: false,
                    draft: d.clone(),
                    base: Draft::default(),
                })
            }
        };
        let name = edit
            .as_ref()
            .map(|e| e.draft.name.clone())
            .unwrap_or_default();
        m.role_name.update(cx, |t, cx| t.set_text(&name, cx));
        m.role_query.update(cx, |t, cx| t.set_text("", cx));
        m.roles.edit = edit;
        m.roles.adding = false;
        m.roles.pick.clear();
        m.roles.custom = false;
        m.roles.access_open = false;
        m.role_hex = None;
        m.anchor = None;
        m.scroll.set_offset(gpui::point(px(0.), px(0.)));
    }

    fn on_role(&mut self, act: crate::roles::Act, window: &mut Window, cx: &mut Context<Self>) {
        use crate::roles::Act;
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        match act {
            Act::Open(id) => self.edit_role(Some(id), cx),
            Act::New => self.edit_role(None, cx),
            Act::Everyone => self.edit_role(Some("everyone".into()), cx),
            Act::Back => {
                if m.roles_dirty(cx) && m.roles.edit.is_some() {
                    m.warn_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(2600));
                } else {
                    self.edit_role(Some(String::new()), cx);
                }
            }
            Act::Color(color) => {
                if let Some(e) = m.roles.edit.as_mut() {
                    e.draft.color = color;
                }
            }
            Act::Custom => {
                m.roles.custom = !m.roles.custom;
                if m.roles.custom && m.role_hex.is_none() {
                    let color = m
                        .roles
                        .edit
                        .as_ref()
                        .map(|e| e.draft.color)
                        .filter(|c| *c != 0)
                        .unwrap_or(0x99aab5);
                    let input = cx.new(|cx| {
                        let mut t = TextInput::new("#99aab5", cx)
                            .with_look(crate::composer::Look {
                                size: px(14.),
                                line_height: px(17.),
                                weight: FontWeight::MEDIUM,
                                placeholder: c(0x5c5c5c),
                                ..kit::field_look()
                            })
                            .max_chars(7)
                            .keep_on_submit();
                        t.set_text(&format!("#{color:06x}"), cx);
                        t
                    });
                    cx.subscribe(
                        &input,
                        |app, input, _: &crate::composer::InputChanged, cx| {
                            let Some(color) = crate::roles::parse_hex(input.read(cx).text()) else {
                                return;
                            };
                            if let Some(e) = app
                                .comm_settings
                                .as_mut()
                                .and_then(|m| m.roles.edit.as_mut())
                            {
                                e.draft.color = color;
                            }
                            cx.notify();
                        },
                    )
                    .detach();
                    window.focus(&input.focus_handle(cx), cx);
                    m.role_hex = Some(input);
                }
            }
            Act::Perm(bit) => {
                let mask = 1u64 << bit;
                if m.roles.can_toggle(mask) {
                    if let Some(e) = m.roles.edit.as_mut() {
                        e.draft.permissions ^= mask;
                    }
                }
            }
            Act::Display => {
                if m.roles.editable() {
                    if let Some(e) = m.roles.edit.as_mut() {
                        e.draft.display = !e.draft.display;
                    }
                }
            }
            Act::Mentionable => {
                if m.roles.editable() {
                    if let Some(e) = m.roles.edit.as_mut() {
                        e.draft.mentionable = !e.draft.mentionable;
                    }
                }
            }
            Act::Move { id, to, before } => {
                let mut ids: Vec<String> = m
                    .roles
                    .rows()
                    .iter()
                    .filter_map(|r| r["role_id"].as_str().map(String::from))
                    .filter(|r| *r != id)
                    .collect();
                if let Some(mut at) = ids.iter().position(|r| *r == to) {
                    if !before {
                        at += 1;
                    }
                    ids.insert(at, id);
                    m.roles.order = (ids != m.roles.published_order()).then_some(ids);
                }
            }
            Act::Adding(on) => {
                m.roles.adding = on;
                m.roles.pick.clear();
                m.role_query.update(cx, |t, cx| t.set_text("", cx));
            }
            Act::Pick(npub) => {
                if !m.roles.pick.remove(&npub) {
                    m.roles.pick.insert(npub);
                }
            }
            Act::AddPicked => {
                let add: Vec<String> = m.roles.pick.iter().cloned().collect();
                self.set_role_holders(add, Vec::new(), cx);
            }
            Act::Remove(npub) => self.set_role_holders(Vec::new(), vec![npub], cx),
            Act::Delete => {
                let Some(role) = m
                    .roles
                    .edit
                    .as_ref()
                    .and_then(|e| e.id.clone())
                    .and_then(|id| m.roles.find(&id).cloned())
                else {
                    return;
                };
                let name = role["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .unwrap_or("this role")
                    .to_string();
                let held = role["holders"].as_array().map(Vec::len).unwrap_or(0);
                let lost = match held {
                    0 => String::new(),
                    1 => " The member who holds it loses its permissions.".into(),
                    n => format!(" The {n} members who hold it lose its permissions."),
                };
                let mut popup = kit::Popup::notice("Delete role");
                popup.text = Some(format!("Delete {name}?{lost} This can't be undone.").into());
                popup.confirm = "Delete".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((
                    popup,
                    PopupAct::DeleteRole(role["role_id"].as_str().unwrap_or("").to_string()),
                ));
            }
            Act::Activate => {
                let mut popup = kit::Popup::notice("Switch to the new roles");
                popup.text = Some("Adds @everyone, channel permissions and role deletion. Every role carries over. Members on an older Vector keep the roles from before the switch and won't see later changes until they update.".into());
                popup.confirm = "Switch".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::ActivateRoles));
            }
            Act::AccessOpen => m.roles.access_open = !m.roles.access_open,
            Act::Access(v) => {
                m.roles.access_open = false;
                if let Some(e) = m.roles.edit.as_mut() {
                    e.draft.channel_id = (!v.is_empty()).then(|| v.to_string());
                }
            }
        }
        cx.notify();
    }

    fn save_roles(&mut self, cx: &mut Context<Self>) {
        use backend::community::{RoleSave, RoleWrite};
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        let name = m.role_name.read(cx).text().trim().to_string();
        let edit_dirty = m.roles.edit_dirty(&name);
        let mut save = RoleSave {
            order: m.roles.order.clone(),
            everyone: None,
            role: None,
        };
        let mut reopen = None;
        if let Some(e) = m.roles.edit.as_ref().filter(|_| edit_dirty) {
            if e.everyone {
                save.everyone = Some(e.draft.permissions.to_string());
                reopen = Some("everyone".to_string());
            } else {
                if name.is_empty() {
                    self.show_toast("A role needs a name", cx);
                    return;
                }
                save.role = Some(RoleWrite {
                    id: e.id.clone(),
                    name: name.clone(),
                    color: e.draft.color,
                    permissions: e.draft.permissions.to_string(),
                    channel_id: e.draft.channel_id.clone(),
                    display: e.draft.display,
                    mentionable: e.draft.mentionable,
                });
            }
        } else {
            reopen = m.roles.edit.as_ref().and_then(|e| e.id.clone());
        }
        if !self.live {
            if let Some(view) = m.roles.view.as_mut() {
                if let Some(order) = &save.order {
                    let mut roles = view["roles"].as_array().cloned().unwrap_or_default();
                    roles.sort_by_key(|r| {
                        order
                            .iter()
                            .position(|id| r["role_id"].as_str() == Some(id))
                            .unwrap_or(usize::MAX)
                    });
                    view["roles"] = roles.into();
                }
                if let Some(p) = &save.everyone {
                    view["everyone"] = p.clone().into();
                }
                if let Some(w) = &save.role {
                    let id = w.id.clone().unwrap_or_else(|| {
                        format!(
                            "{:064x}",
                            view["roles"].as_array().map(Vec::len).unwrap_or(0) + 10
                        )
                    });
                    let fresh = serde_json::json!({"role_id": id, "name": w.name, "color": w.color, "permissions": w.permissions, "channel_id": w.channel_id, "display": w.display, "mentionable": w.mentionable, "manageable": true, "holders": []});
                    let roles = view["roles"].as_array_mut().unwrap();
                    match roles
                        .iter_mut()
                        .find(|r| r["role_id"].as_str() == Some(id.as_str()))
                    {
                        Some(r) => {
                            let holders = r["holders"].take();
                            *r = fresh;
                            r["holders"] = holders;
                        }
                        None => roles.push(fresh),
                    }
                    reopen = Some(id);
                }
            }
            m.roles.order = None;
            if let Some(key) = reopen {
                self.edit_role(Some(key), cx);
            }
            return;
        }
        m.saving = true;
        let id = m.community.clone();
        let rx = backend::run(backend::community::save_roles(id.clone(), save));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("Save failed".into()));
            this.update(cx, |this, cx| {
                if let Some(m) = this.comm_settings.as_mut().filter(|m| m.community == id) {
                    m.saving = false;
                }
                match res {
                    Ok(made) => this.load_roles(made.or(reopen), cx),
                    Err(e) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Deletes `role_id`, or (`delete` false) moves the community to the new roles.
    fn role_op(&mut self, delete: bool, role_id: String, cx: &mut Context<Self>) {
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        if !self.live {
            if let Some(view) = m.roles.view.as_mut() {
                if delete {
                    view["roles"]
                        .as_array_mut()
                        .map(|a| a.retain(|r| r["role_id"].as_str() != Some(role_id.as_str())));
                } else {
                    view["activated"] = true.into();
                }
            }
            if delete {
                self.edit_role(Some(String::new()), cx);
            }
            cx.notify();
            return;
        }
        m.roles.busy = true;
        let id = m.community.clone();
        let everyone = m
            .roles
            .view
            .as_ref()
            .map(|v| crate::roles::num(&v["everyone_default"]).to_string())
            .unwrap_or_else(|| "0".into());
        let rx = if delete {
            backend::run(backend::community::delete_role(id.clone(), role_id))
        } else {
            backend::run(backend::community::activate_roles(id.clone(), everyone))
        };
        cx.spawn(async move |this, cx| {
            let res = rx
                .await
                .unwrap_or_else(|_| Err("The roles couldn't be changed".into()));
            this.update(cx, |this, cx| {
                if let Some(m) = this.comm_settings.as_mut().filter(|m| m.community == id) {
                    m.roles.busy = false;
                }
                match res {
                    Ok(()) => {
                        if delete {
                            this.edit_role(Some(String::new()), cx);
                        }
                        this.load_roles(None, cx);
                    }
                    Err(e) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_role_holders(&mut self, add: Vec<String>, remove: Vec<String>, cx: &mut Context<Self>) {
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        let Some(role_id) = m.roles.edit.as_ref().and_then(|e| e.id.clone()) else {
            return;
        };
        if m.roles.busy || (add.is_empty() && remove.is_empty()) {
            return;
        }
        if !self.live {
            if let Some(view) = m.roles.view.as_mut() {
                for r in view["roles"]
                    .as_array_mut()
                    .into_iter()
                    .flatten()
                    .filter(|r| r["role_id"].as_str() == Some(role_id.as_str()))
                {
                    let holders = r["holders"].as_array_mut().unwrap();
                    holders.retain(|h| !remove.iter().any(|n| h.as_str() == Some(n)));
                    holders.extend(add.iter().map(|n| serde_json::Value::from(n.clone())));
                }
            }
            m.roles.adding = false;
            m.roles.pick.clear();
            return;
        }
        let held: Vec<(String, Vec<String>)> = add
            .iter()
            .chain(&remove)
            .map(|n| {
                let roles = m
                    .roles
                    .view
                    .as_ref()
                    .and_then(|v| v["roles"].as_array())
                    .into_iter()
                    .flatten()
                    .filter(|r| {
                        r["holders"]
                            .as_array()
                            .is_some_and(|h| h.iter().any(|h| h.as_str() == Some(n)))
                    })
                    .filter_map(|r| r["role_id"].as_str().map(String::from))
                    .collect();
                (n.clone(), roles)
            })
            .collect();
        m.roles.busy = true;
        let id = m.community.clone();
        let rx = backend::run(backend::community::set_role_holders(
            id.clone(),
            role_id.clone(),
            held,
            add,
            remove,
        ));
        cx.spawn(async move |this, cx| {
            let res = rx
                .await
                .unwrap_or_else(|_| Err("Couldn't change who holds this role".into()));
            this.update(cx, |this, cx| {
                if let Some(m) = this.comm_settings.as_mut().filter(|m| m.community == id) {
                    m.roles.busy = false;
                    m.roles.adding = false;
                    m.roles.pick.clear();
                }
                if let Err(e) = res {
                    this.show_toast(&e, cx);
                }
                this.load_roles(Some(role_id), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_sync_report(&mut self, cx: &mut Context<Self>) {
        let live = self.live;
        let fixture = if live { None } else { self.store.borrow().fixture_sync_report.clone() };
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        if m.report.as_ref().is_some_and(|r| r.loading) {
            return;
        }
        m.report = Some(crate::commsettings::Report { loading: true, ..Default::default() });
        let id = m.community.clone();
        let rx = if live {
            backend::run(backend::community::sync_report(id.clone()))
        } else {
            let (tx, rx) = futures::channel::oneshot::channel();
            let _ = tx.send(sync_report_fixture(fixture));
            rx
        };
        cx.spawn(async move |this, cx| {
            let (control, epoch) = rx.await.unwrap_or_else(|_| (Err("The report couldn't be read.".into()), Err(String::new())));
            this.update(cx, |this, cx| {
                let Some(r) = this.comm_settings.as_mut().filter(|m| m.community == id).and_then(|m| m.report.as_mut()) else {
                    return;
                };
                r.loading = false;
                if let (Err(e), Err(_)) = (&control, &epoch) {
                    r.error = Some(e.clone());
                } else {
                    let side = |v: Result<serde_json::Value, String>| v.unwrap_or_else(|e| serde_json::json!({ "error": e }));
                    let data = serde_json::json!({
                        "client": std::env::consts::OS,
                        "version": env!("CARGO_PKG_VERSION"),
                        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        "community": id.chars().take(12).collect::<String>(),
                        "control": side(control),
                        "epoch": side(epoch),
                    });
                    let mut out = Vec::new();
                    let mut ser = serde_json::Serializer::with_formatter(&mut out, serde_json::ser::PrettyFormatter::with_indent(b" "));
                    let _ = serde::Serialize::serialize(&data, &mut ser);
                    r.text = String::from_utf8(out).unwrap_or_default().into();
                    r.data = Some(data);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_audit(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        if m.audit.loading {
            return;
        }
        m.audit.loaded = true;
        if !self.live {
            let log = self
                .store
                .borrow()
                .fixture_audit
                .clone()
                .unwrap_or_default();
            m.audit.entries = log["entries"].as_array().cloned().unwrap_or_default();
            m.audit.compacted = log["compacted"].as_bool().unwrap_or(false);
            return;
        }
        m.audit.loading = true;
        let id = m.community.clone();
        let rx = backend::run(backend::community::audit_log(id.clone()));
        cx.spawn(async move |this, cx| {
            let res = rx
                .await
                .unwrap_or_else(|_| Err("The audit log couldn't be read.".into()));
            this.update(cx, |this, cx| {
                let Some(m) = this.comm_settings.as_mut().filter(|m| m.community == id) else {
                    return;
                };
                m.audit.loading = false;
                match res {
                    Ok(log) => {
                        m.audit.entries = log["entries"].as_array().cloned().unwrap_or_default();
                        m.audit.compacted = log["compacted"].as_bool().unwrap_or(false);
                        m.audit.error = None;
                    }
                    Err(e) => m.audit.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_comm_settings(
        &mut self,
        act: crate::commsettings::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::commsettings::{Act, RELAYS_MAX, relay_address, voice_address};
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        match act {
            Act::ExplainAuth => self.notice(
                "This relay uses AUTH",
                "AUTH relays ask every app to sign in with its Nostr key. Vector signs in as you, so the relay can link your npub to this community every time you connect. Without AUTH, the relay sees this community only through Concord's anonymous keys.\n\nFor faster connections and more privacy, use relays without AUTH.".into(),
                cx,
            ),
            Act::PickIcon => self.pick_comm_image(false, window, cx),
            Act::PickBanner => self.pick_comm_image(true, window, cx),
            Act::RemoveIcon => {
                m.icon_pick = None;
                m.icon_removed = self.store.borrow().icon(&m.community).is_some();
            }
            Act::RemoveBanner => {
                m.banner_pick = None;
                m.banner_removed = self.store.borrow().banner(&m.community).is_some();
            }
            Act::Close if m.report.is_some() => m.report = None,
            Act::Close => {
                if m.saving || m.unbanning {
                    return;
                }
                if m.dirty(cx) {
                    m.warn_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(2600));
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(2700))
                            .await;
                        this.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .detach();
                } else {
                    self.comm_settings = None;
                }
            }
            Act::Section(id) => {
                if m.section != id {
                    m.section = id;
                    m.anchor = None;
                    m.ban_sel.clear();
                    m.scroll.set_offset(gpui::point(px(0.), px(0.)));
                }
            }
            Act::Anchor(section, anchor) => {
                if m.section != section {
                    m.section = section;
                    m.ban_sel.clear();
                }
                m.anchor = Some(anchor);
                let ix = m
                    .sections()
                    .iter()
                    .find(|s| s.id == section)
                    .and_then(|s| s.anchors.iter().position(|a| a.id == anchor))
                    .unwrap_or(0);
                if ix == 0 {
                    m.scroll.set_offset(gpui::point(px(0.), px(0.)));
                } else {
                    m.scroll.scroll_to_item(0);
                }
            }
            Act::ClearSearch => m.search.update(cx, |t, cx| t.set_text("", cx)),
            Act::AddRelay => {
                let raw = m.relay_input.read(cx).text().to_string();
                if m.relays.len() >= RELAYS_MAX {
                    m.relay_error = Some(format!("At most {RELAYS_MAX} relays."));
                } else {
                    match relay_address(&raw, &m.relays, None) {
                        Ok(url) => {
                            m.relays.push(url);
                            m.relay_input.update(cx, |t, cx| t.set_text("", cx));
                            m.relay_error = None;
                        }
                        Err(e) => m.relay_error = (!e.is_empty()).then_some(e),
                    }
                }
            }
            Act::EditRelay(i) => {
                let Some(url) = m.relays.get(i).cloned() else {
                    return;
                };
                let input = cx.new(|cx| {
                    let mut t = TextInput::new("", cx)
                        .with_look(crate::composer::Look {
                            size: px(14.),
                            line_height: px(17.),
                            weight: FontWeight::NORMAL,
                            ..kit::field_look()
                        })
                        .keep_on_submit();
                    t.set_text(&url, cx);
                    t
                });
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| {
                    app.on_comm_settings(crate::commsettings::Act::CommitEdit, window, cx)
                })
                .detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
                    app.on_comm_settings(crate::commsettings::Act::CancelEdit, window, cx)
                })
                .detach();
                cx.subscribe(&input, |app, _, _: &crate::composer::InputChanged, cx| {
                    if let Some(m) = app.comm_settings.as_mut() {
                        m.relay_error = None;
                    }
                    cx.notify()
                })
                .detach();
                window.focus(&input.focus_handle(cx), cx);
                m.relay_edit = Some((i, input));
                m.relay_error = None;
            }
            Act::CommitEdit => {
                let Some((i, input)) = m.relay_edit.take() else {
                    return;
                };
                let raw = input.read(cx).text().to_string();
                match relay_address(&raw, &m.relays, Some(i)) {
                    Ok(url) => {
                        if let Some(slot) = m.relays.get_mut(i) {
                            *slot = url;
                        }
                    }
                    Err(e) if e.is_empty() => {}
                    Err(e) => {
                        m.relay_error = Some(e);
                        m.relay_edit = Some((i, input));
                    }
                }
            }
            Act::CancelEdit => m.relay_edit = None,
            Act::RemoveRelay(i) => {
                if m.relays.len() > 1 && i < m.relays.len() {
                    m.relays.remove(i);
                }
            }
            Act::AddVoice => {
                let raw = m.voice_input.read(cx).text().to_string();
                match voice_address(&raw, &m.voice) {
                    Ok(url) => {
                        m.voice.push(url);
                        m.voice_input.update(cx, |t, cx| t.set_text("", cx));
                        m.voice_error = None;
                    }
                    Err(e) => m.voice_error = (!e.is_empty()).then_some(e),
                }
            }
            Act::RemoveVoice(i) => {
                if i < m.voice.len() {
                    m.voice.remove(i);
                }
            }
            Act::Ban(npub) => {
                if !m.ban_sel.remove(&npub) {
                    m.ban_sel.insert(npub);
                }
            }
            Act::BanAll => {
                let rows = crate::commsettings::ban_rows(m, &self.store.borrow(), cx);
                if rows.iter().all(|n| m.ban_sel.contains(n)) {
                    for n in &rows {
                        m.ban_sel.remove(n);
                    }
                } else {
                    m.ban_sel.extend(rows);
                }
            }
            Act::ClearBans => m.ban_sel.clear(),
            Act::Unban => {
                if m.unbanning || m.ban_sel.is_empty() {
                    return;
                }
                let npubs: Vec<String> = m.ban_sel.iter().cloned().collect();
                if !self.live {
                    m.bans.retain(|n| !m.ban_sel.contains(n));
                    m.ban_sel.clear();
                } else {
                    m.unbanning = true;
                    let id = m.community.clone();
                    let rx = backend::run(backend::community::set_banned(
                        id.clone(),
                        npubs.clone(),
                        false,
                    ));
                    cx.spawn(async move |this, cx| {
                        let res = rx.await.unwrap_or_else(|_| Err("Unban failed".into()));
                        this.update(cx, |this, cx| {
                            if let Some(m) =
                                this.comm_settings.as_mut().filter(|m| m.community == id)
                            {
                                m.unbanning = false;
                                if res.is_ok() {
                                    m.bans.retain(|n| !npubs.contains(n));
                                    m.ban_sel.clear();
                                }
                            }
                            if let Err(e) = res {
                                this.show_toast(&e, cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            Act::Reset => {
                let (name, description) = (m.saved.name.clone(), m.saved.description.clone());
                m.name.update(cx, |t, cx| t.set_text(&name, cx));
                m.description
                    .update(cx, |t, cx| t.set_text(&description, cx));
                m.relays = m.saved.relays.clone();
                m.voice = m.saved.voice.clone();
                m.relay_edit = None;
                m.roles.order = None;
                if let Some(e) = m.roles.edit.as_mut() {
                    e.draft = if e.id.is_none() {
                        crate::roles::Draft {
                            name: "new role".into(),
                            ..Default::default()
                        }
                    } else {
                        e.base.clone()
                    };
                    let name = e.draft.name.clone();
                    m.role_name.update(cx, |t, cx| t.set_text(&name, cx));
                }
                m.roles.custom = false;
                m.role_hex = None;
                m.relay_error = None;
                m.voice_error = None;
                m.warn_until = None;
            }
            Act::Save => self.save_comm_settings(cx),
            Act::Learn => self.open_web("https://docs.iroh.computer/concepts/relays", cx),
            Act::AuditRefresh => self.load_audit(cx),
            Act::SyncReport => self.load_sync_report(cx),
            Act::ReportClose => m.report = None,
            Act::ReportCopy => {
                if let Some(text) = m.report.as_ref().map(|r| r.text.to_string()).filter(|t| !t.is_empty()) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    self.toast = Some(("Copied Sync Report".into(), std::time::Instant::now()));
                }
            }
            Act::Role(a) => return self.on_role(a, window, cx),
            Act::AuditToggle(id) => {
                if !m.audit.open.remove(&id) {
                    m.audit.open.insert(id);
                }
            }
            Act::AuditMore => m.audit.shown += crate::commsettings::AUDIT_PAGE,
            Act::AuditFilter => m.audit.filter_open = !m.audit.filter_open,
            Act::AuditPick(v) => {
                m.audit.action = v;
                m.audit.filter_open = false;
                m.audit.shown = crate::commsettings::AUDIT_PAGE;
            }
        }
        if self
            .comm_settings
            .as_ref()
            .is_some_and(|m| m.section == "audit" && !m.audit.loaded)
        {
            self.load_audit(cx);
        }
        if self
            .comm_settings
            .as_ref()
            .is_some_and(|m| m.section == "roles" && !m.roles.loaded)
        {
            self.load_roles(None, cx);
        }
        let _ = window;
        cx.notify();
    }

    fn save_comm_settings(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        if m.saving || !m.dirty(cx) {
            return;
        }
        if m.roles_dirty(cx) {
            self.save_roles(cx);
            let Some(m) = self.comm_settings.as_ref() else {
                return;
            };
            if !(m.overview_dirty(cx) || m.relays != m.saved.relays || m.voice != m.saved.voice) {
                return;
            }
        }
        let Some(m) = self.comm_settings.as_mut() else {
            return;
        };
        let name = m.name.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let description = m.description.read(cx).text().trim().to_string();
        let name_change = (name != m.saved.name).then(|| name.clone());
        let description_change = (description != m.saved.description).then(|| description.clone());
        let voice = (m.voice != m.saved.voice).then(|| m.voice.clone());
        let relays = (m.relays != m.saved.relays).then(|| m.relays.clone());
        let id = m.community.clone();
        m.warn_until = None;
        let icon = m
            .icon_pick
            .as_ref()
            .map(|p| (p.path.clone(), p.crop.clone()));
        let banner = m
            .banner_pick
            .as_ref()
            .map(|p| (p.path.clone(), p.crop.clone()));
        let (icon_removed, banner_removed) = (m.icon_removed, m.banner_removed);
        if !self.live {
            m.saved = crate::commsettings::Saved {
                name,
                description,
                relays: m.relays.clone(),
                voice: m.voice.clone(),
            };
            m.icon_pick = None;
            m.banner_pick = None;
            m.icon_removed = false;
            m.banner_removed = false;
            return;
        }
        m.saving = true;
        m.progress = None;
        let (tx, mut progress) = futures::channel::mpsc::unbounded::<u8>();
        let rx = backend::run({
            let id = id.clone();
            async move {
                if name_change.is_some() || description_change.is_some() {
                    backend::community::edit_metadata(id.clone(), name_change, description_change)
                        .await?;
                }
                if icon_removed {
                    backend::community::remove_image(id.clone(), false).await?;
                }
                if banner_removed {
                    backend::community::remove_image(id.clone(), true).await?;
                }
                for (banner, pick) in [(false, icon), (true, banner)] {
                    if let Some((path, crop)) = pick {
                        let tx = tx.clone();
                        let _ = tx.unbounded_send(0);
                        backend::community::set_image(id.clone(), path, banner, crop, move |p| {
                            let _ = tx.unbounded_send(p);
                        })
                        .await?;
                    }
                }
                drop(tx);
                let voice = match voice {
                    Some(v) => Some(backend::community::set_voice_relays(id.clone(), v).await?),
                    None => None,
                };
                Ok::<_, String>(voice)
            }
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(p) = progress.next().await {
                let ok = this
                    .update(cx, |this, cx| {
                        if let Some(m) = this.comm_settings.as_mut() {
                            m.progress = Some(p);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
            let res = rx.await.unwrap_or_else(|_| Err("Save failed".into()));
            this.update(cx, |this, cx| {
                let art = this.comm_settings.as_ref().is_some_and(|m| {
                    m.icon_pick.is_some()
                        || m.banner_pick.is_some()
                        || m.icon_removed
                        || m.banner_removed
                });
                if art {
                    this.resolve_art(Some(id.clone()), cx);
                }
                let live = this.comm_settings.as_mut().filter(|m| m.community == id);
                match res {
                    Ok(voice) => {
                        if let Some(m) = live {
                            m.saving = false;
                            m.progress = None;
                            m.icon_pick = None;
                            m.banner_pick = None;
                            m.icon_removed = false;
                            m.banner_removed = false;
                            if let Some(v) = voice {
                                m.voice = v;
                            }
                            m.saved = crate::commsettings::Saved {
                                name: m.name.read(cx).text().trim().to_string(),
                                description: m.description.read(cx).text().trim().to_string(),
                                relays: m.saved.relays.clone(),
                                voice: m.voice.clone(),
                            };
                        }
                        this.reload_communities(cx);
                        if let Some(r) = relays {
                            this.check_relays(id.clone(), r, cx);
                        }
                    }
                    Err(e) => {
                        if let Some(m) = live {
                            m.saving = false;
                            m.progress = None;
                        }
                        this.show_toast(&e, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `csMoveRelays`, first half: check the new relays. One that can't hold a Community stops
    /// the move; a throttling or single-network one waits on a yes.
    fn check_relays(&mut self, id: String, relays: Vec<String>, cx: &mut Context<Self>) {
        self.set_relay_saving(true);
        self.rekey = Some(crate::rekey::Rekey::new("Checking the new relays"));
        let rx = backend::run(backend::community::check_relays(id.clone(), relays.clone()));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("Save failed".into()));
            this.update(cx, |this, cx| {
                this.rekey = None;
                this.set_relay_saving(false);
                cx.notify();
                let checks = match res {
                    Ok(c) => c,
                    Err(e) => return this.show_toast(&e, cx),
                };
                if let Some(bad) = checks.iter().find(|c| !c.ok) {
                    let mut popup = kit::Popup::notice("This relay can't host the community");
                    let why = bad.reason.clone().unwrap_or_else(|| format!("{} can't hold this community.", bad.relay));
                    popup.text = Some(kit::escape(&why).into());
                    this.popup = Some((popup, PopupAct::Notice));
                    return;
                }
                let cautions = relay_cautions(&checks, &relays);
                if cautions.is_empty() {
                    return this.move_relays(id, relays, cx);
                }
                let mut popup = kit::Popup::notice("Before this community moves");
                popup.text = Some(format!("{}<br><br>Move anyway?", cautions.join("<br><br>")).into());
                popup.confirm = "Move Anyway".into();
                popup.cancel = Some("Cancel".into());
                this.popup = Some((popup, PopupAct::MoveRelays { id, relays }));
            })
            .ok();
        })
        .detach();
    }

    /// `csMoveRelays`, second half: give the new relays the whole Community, then announce it.
    fn move_relays(&mut self, id: String, relays: Vec<String>, cx: &mut Context<Self>) {
        self.set_relay_saving(true);
        self.rekey = Some(crate::rekey::Rekey::new("Moving to new relays"));
        cx.notify();
        let rx = backend::run(backend::community::move_relays(id.clone(), relays));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("Save failed".into()));
            let ok = res.is_ok();
            this.update(cx, |this, cx| {
                if ok && let Some(r) = this.rekey.as_mut() {
                    r.set(100., Some("Everyone will follow the community to its new relays".into()));
                }
                cx.notify();
            })
            .ok();
            if ok {
                cx.background_executor().timer(std::time::Duration::from_millis(700)).await;
            }
            this.update(cx, |this, cx| {
                this.rekey = None;
                this.set_relay_saving(false);
                cx.notify();
                match res {
                    Ok((moved, warnings)) => {
                        if let Some(m) = this.comm_settings.as_mut().filter(|m| m.community == id) {
                            m.relays = moved.clone();
                            m.saved.relays = moved;
                        }
                        this.reload_communities(cx);
                        if !warnings.is_empty() {
                            let mut popup = kit::Popup::notice("Moved, with a few gaps");
                            popup.text = Some(warnings.iter().map(|w| kit::escape(w)).collect::<Vec<_>>().join("<br><br>").into());
                            this.popup = Some((popup, PopupAct::Notice));
                        }
                    }
                    Err(e) => this.show_toast(&e, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_relay_saving(&mut self, on: bool) {
        if let Some(m) = self.comm_settings.as_mut() {
            m.saving = on;
        }
    }

    /// `promptCommunityNickname`: yours, or a member's beneath you.
    fn ask_community_nickname(
        &mut self,
        community: String,
        npub: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, own, current) = {
            let store = self.store.borrow();
            let mine = npub == store.my_npub;
            let p = store.profiles.get(npub.as_str());
            let own = p
                .map(|p| {
                    if !p.name.is_empty() {
                        p.name.clone()
                    } else {
                        p.display_name.clone()
                    }
                })
                .unwrap_or_default();
            let current = store
                .nicknames
                .get(&community)
                .and_then(|n| n.get(&npub))
                .cloned()
                .unwrap_or_default();
            let title = if mine {
                "Change Nickname".to_string()
            } else {
                format!("Change {}'s Nickname", store.name(&npub))
            };
            (title, (own, mine), current)
        };
        let (own, mine) = own;
        let placeholder = if own.is_empty() {
            "Nickname".to_string()
        } else {
            own
        };
        let input = cx.new(|cx| {
            let mut t = TextInput::new(placeholder, cx)
                .with_look(kit::field_look())
                .keep_on_submit()
                .submit_empty();
            t.set_text(&current, cx);
            t
        });
        cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| {
            app.answer_popup(true, window, cx)
        })
        .detach();
        cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
            app.answer_popup(false, window, cx)
        })
        .detach();
        window.focus(&input.focus_handle(cx), cx);
        let mut popup = kit::Popup::notice(title);
        popup.icon = None;
        popup.text = Some(
            format!(
                "Everyone in this community sees it. Leave it empty to go by {} own name.",
                if mine { "your" } else { "their" }
            )
            .into(),
        );
        popup.confirm = "Save".into();
        popup.cancel = Some("Cancel".into());
        popup.input = Some(input);
        self.popup = Some((popup, PopupAct::CommunityNickname { community, npub }));
        cx.notify();
    }

    fn set_community_nickname(
        &mut self,
        community: String,
        npub: String,
        nick: String,
        cx: &mut Context<Self>,
    ) {
        if !self.live {
            let mut store = self.store.borrow_mut();
            let named = store.nicknames.entry(community).or_default();
            if nick.is_empty() {
                named.remove(&npub);
            } else {
                named.insert(npub, nick);
            }
            cx.notify();
            return;
        }
        let rx = backend::run(backend::community::set_nickname(
            community.clone(),
            npub,
            nick,
        ));
        cx.spawn(async move |this, cx| {
            let res = rx
                .await
                .unwrap_or_else(|_| Err("Couldn't set the nickname".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(()) => {
                        let named = backend::community::nicknames(&community);
                        this.store.borrow_mut().nicknames.insert(community, named);
                    }
                    Err(e) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_invite(&mut self, act: crate::invite::Act, window: &mut Window, cx: &mut Context<Self>) {
        use crate::invite::Act;
        let Some(m) = self.invite.as_mut() else {
            return;
        };
        match act {
            Act::Close => {
                if !m.busy {
                    self.invite = None;
                }
            }
            Act::Expand => m.expanded = true,
            Act::Copy(token) => {
                let Some(url) = m
                    .links
                    .iter()
                    .find(|l| l.token == token)
                    .map(|l| l.url.clone())
                else {
                    return;
                };
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                m.copied = Some(token.clone());
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(1200))
                        .await;
                    this.update(cx, |this, cx| {
                        if let Some(m) = this
                            .invite
                            .as_mut()
                            .filter(|m| m.copied.as_deref() == Some(token.as_str()))
                        {
                            m.copied = None;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Act::Create => {
                if m.busy {
                    return;
                }
                if m.public {
                    self.ask_invite_label(window, cx);
                } else {
                    let mut popup = kit::Popup::notice("Make community public?");
                    popup.text = Some("Creating an invite link makes this community <b>public</b>: anyone with the link can join. You can make it private again later by revoking every link.".into());
                    popup.confirm = "Make public".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::InvitePublic));
                }
            }
            Act::Revoke(token) => {
                if m.busy {
                    return;
                }
                let others: usize = m.others.iter().map(|o| o.1).sum();
                if m.links.len() == 1 && others == 0 {
                    let mut popup = kit::Popup::notice("Make community private?");
                    popup.text = Some("Revoking the last invite link makes this community <b>private</b> again. This can take a few seconds.".into());
                    popup.confirm = "Make private".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::InvitePrivate { token }));
                } else {
                    self.revoke_invite_link(token, false, cx);
                }
            }
            Act::Toggle(npub) => {
                if !m.busy && !m.picked.remove(&npub) {
                    if self.live && !self.store.borrow().profiles.contains_key(npub.as_str()) {
                        let n = npub.clone();
                        let _ = backend::run(async move {
                            vector_core::profile::sync::load_profile(n, &sync::ProfileHandler).await
                        });
                    }
                    m.picked.insert(npub);
                }
            }
            Act::Cta => {
                if m.busy {
                    return;
                }
                if m.picked.is_empty() {
                    self.invite = None;
                    return cx.notify();
                }
                let targets: Vec<String> = m.picked.iter().cloned().collect();
                let count = targets.len();
                let people = |n: usize| if n == 1 { "person" } else { "people" };
                m.busy = true;
                m.status = Some((format!("Inviting {count} {}…", people(count)), false));
                let id = m.community.clone();
                let live = self.live;
                let rx = backend::run(async move {
                    let mut ok = 0usize;
                    for npub in targets {
                        if !live || backend::community::invite(id.clone(), npub).await.is_ok() {
                            ok += 1;
                        }
                    }
                    ok
                });
                cx.spawn(async move |this, cx| {
                    let Ok(ok) = rx.await else { return };
                    this.update(cx, |this, cx| {
                        let Some(m) = this.invite.as_mut() else {
                            return;
                        };
                        m.busy = false;
                        let fail = count - ok;
                        if fail == 0 {
                            m.status = Some((format!("Invited {ok} {}!", people(ok)), false));
                            m.picked.clear();
                            m.filter.update(cx, |t, cx| t.set_text("", cx));
                            cx.spawn(async move |this, cx| {
                                cx.background_executor()
                                    .timer(std::time::Duration::from_secs(3))
                                    .await;
                                this.update(cx, |this, cx| {
                                    if let Some(m) = this.invite.as_mut().filter(|m| {
                                        m.status
                                            .as_ref()
                                            .is_some_and(|s| !s.1 && s.0.starts_with("Invited"))
                                    }) {
                                        m.status = None;
                                        cx.notify();
                                    }
                                })
                                .ok();
                            })
                            .detach();
                        } else {
                            m.status = Some((format!("Invited {ok}, {fail} failed."), ok == 0));
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Looks up the invite links in the open chat, once per change to it.
    fn fetch_packs(&mut self, naddrs: Vec<String>, cx: &mut Context<Self>) {
        for naddr in naddrs {
            if !self.live {
                let res = self
                    .store
                    .borrow()
                    .fixture_packs
                    .get(&naddr)
                    .cloned()
                    .ok_or_else(|| "Pack not found on any relay".to_string());
                self.media.borrow_mut().packs.settle(&naddr, res);
                cx.notify();
                continue;
            }
            let n = naddr.clone();
            let rx =
                backend::run(
                    async move { vector_core::emoji_packs::fetch_pack_by_naddr(&n).await },
                );
            cx.spawn(async move |this, cx| {
                let Ok(res) = rx.await else { return };
                this.update(cx, |this, cx| {
                    this.media.borrow_mut().packs.settle(&naddr, res);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Equipped packs, as their ids.
    /// The picker's equipped packs: the account's, or every fixture pack.
    fn reload_picker_packs(&mut self, cx: &mut Context<Self>) {
        let picker = self.emoji_picker.clone();
        let live = self.live;
        picker.update(cx, |p, _| p.set_media(self.media.clone(), live));
        if !live {
            let store = self.store.borrow();
            let mut packs: Vec<_> = store.fixture_packs.values().cloned().collect();
            packs.sort_by(|a, b| a.title.cmp(&b.title));
            let packs =
                crate::picker::packs_from_core(packs, |u| timeline::emoji_source(&store, u));
            drop(store);
            picker.update(cx, |p, cx| p.set_packs(packs, cx));
            return;
        }
        self.load_equipped(false, cx);
    }

    /// The account's packs with the theme's pinned one at its slot, fetching that once if unseen.
    fn load_equipped(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let picker = self.emoji_picker.clone();
        let rx = backend::run(backend::packs::equipped(refresh));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(eq)) = rx.await else { return };
            let mut packs = crate::picker::packs_from_core(eq.packs, str::to_string);
            if let Some(p) = eq.theme.and_then(|i| packs.get_mut(i)) {
                p.theme = true;
            }
            picker.update(cx, |p, cx| p.set_packs(packs, cx));
            let Some(naddr) = eq.fetch else { return };
            if !backend::run(backend::packs::fetch_theme(naddr)).await.unwrap_or(false) {
                return;
            }
            this.update(cx, |this, cx| this.load_equipped(false, cx)).ok();
        })
        .detach();
    }

    /// The `:` before the caret, re-read on every edit.
    fn update_shortcode(&mut self, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        let (text, caret) = (input.text().to_string(), input.caret());
        let send = !text.trim().is_empty();
        if send != self.send_shown {
            self.send_shown = send;
            self.send_flips += 1;
        }
        if self.update_commands(&text, cx) {
            return;
        }
        let now = self.now_local();
        self.time_chip = None;
        self.input.update(cx, |i, _| i.set_chip(false));
        if let Some(d) = self.time_dismissed
            && !text
                .get(d..d + 5)
                .is_some_and(|t| t.eq_ignore_ascii_case("@time") || t.eq_ignore_ascii_case("/time"))
        {
            self.time_dismissed = None;
        }
        let pick = crate::texttime::picker(&text, caret, now, self.time_pick.as_ref())
            .filter(|p| Some(p.at) != self.time_dismissed);
        if pick.is_some() {
            self.shortcode = None;
            self.mention = None;
            self.mention_pool = None;
            self.time_pick = pick;
            self.input.update(cx, |i, _| i.set_nav(true));
            return cx.notify();
        }
        self.time_pick = None;
        if let Some(p) = self.suggest_mention(&text, caret, cx) {
            self.shortcode = None;
            self.mention = Some(p);
            self.input.update(cx, |i, _| i.set_nav(true));
            return cx.notify();
        }
        self.mention = None;
        self.mention_pool = None;
        let popup =
            crate::shortcodes::suggest(&text, caret, self.emoji_picker.read(cx), self.emoticons);
        let none = popup.is_none();
        self.set_shortcode(popup, cx);
        if none {
            self.suggest_time(&text, caret, now, cx);
        }
    }

    fn now_local(&self) -> chrono::DateTime<chrono::Local> {
        use chrono::TimeZone;
        chrono::Local
            .timestamp_millis_opt(self.store.borrow().now_ms() as i64)
            .single()
            .unwrap_or_else(chrono::Local::now)
    }

    fn suggest_time(
        &mut self,
        text: &str,
        caret: usize,
        now: chrono::DateTime<chrono::Local>,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            self.chip_passed.clear();
        }
        if !self.time_suggestions || self.input.read(cx).has_selection() {
            return;
        }
        let chip = text
            .get(..caret)
            .and_then(|before| crate::texttime::suggest(before, now))
            .map(|found| crate::texttime::Chip {
                phrase: text[found.start..found.end].to_string(),
                found,
            })
            .filter(|c| !self.chip_passed.contains(&c.key()));
        let on = chip.is_some();
        self.time_chip = chip;
        self.input.update(cx, |i, _| i.set_chip(on));
        cx.notify();
    }

    fn time_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.time_pick.as_ref()?;
        let app = cx.entity().downgrade();
        let now = self.store.borrow().now_ms() as f64 / 1000.;
        Some(crate::texttime::picker_view(
            p,
            now,
            Rc::new(move |i, w, cx| {
                app.update(cx, |app, cx| app.pick_time(i, w, cx)).ok();
            }),
        ))
    }

    fn time_chip_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chip = self.time_chip.as_ref()?;
        let left = self
            .input
            .read(cx)
            .x_at(chip.found.start)
            .zip(self.composer_box.get())
            .map_or(0., |(x, b)| f32::from(x - b.origin.x).min(f32::from(b.size.width) - 200.));
        let app = cx.entity().downgrade();
        let now = self.store.borrow().now_ms() as f64 / 1000.;
        Some(crate::texttime::chip_view(
            chip,
            now,
            left,
            Rc::new(move |w, cx| {
                app.update(cx, |app, cx| app.accept_time_chip(w, cx)).ok();
            }),
        ))
    }

    fn close_popups(&mut self, cx: &mut Context<Self>) {
        self.shortcode = None;
        self.mention = None;
        self.mention_pool = None;
        self.time_pick = None;
        self.time_chip = None;
        self.input.update(cx, |i, _| {
            i.set_nav(false);
            i.set_chip(false);
        });
    }

    fn pick_time(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        let (text, caret) = (input.text().to_string(), input.caret());
        let Some(p) = crate::texttime::picker(&text, caret, self.now_local(), None) else {
            return;
        };
        let (Some(t), Some(&style)) = (&p.parsed, p.styles().get(i)) else {
            return;
        };
        let code = format!("<t:{}:{style}> ", t.unix);
        self.time_pick = None;
        self.input.update(cx, |input, cx| {
            input.set_nav(false);
            input.replace(p.at..caret, &code, window, cx);
        });
        cx.notify();
    }

    fn accept_time_chip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chip) = self.time_chip.take() else {
            return;
        };
        let s = &chip.found;
        self.input.update(cx, |input, cx| {
            input.set_chip(false);
            let (text, caret) = (input.text().to_string(), input.caret().max(s.end));
            let gap = if s.end == text.len() { " " } else { "" };
            let code = format!("<t:{}:{}>{gap}{}", s.unix, s.style, &text[s.end..caret]);
            input.replace(s.start..caret, &code, window, cx);
        });
        cx.notify();
    }

    /// `@` people, Roles and @everyone; `#` the Community's channels.
    fn suggest_mention(
        &mut self,
        text: &str,
        caret: usize,
        cx: &mut Context<Self>,
    ) -> Option<crate::mentions::Popup> {
        use crate::mentions;
        let chat_id = self.open.clone()?;
        if let Some((start, query)) = mentions::trigger(text, caret, '@') {
            let pool = match self.mention_pool.clone() {
                Some(p) => p,
                None => {
                    let community = self
                        .store
                        .borrow()
                        .community_of(&chat_id)
                        .map(|c| c.community_id.clone());
                    if let Some(id) = community
                        .as_ref()
                        .filter(|id| self.live && !self.mention_rules.contains_key(*id))
                    {
                        self.load_mention_rules(id.clone(), cx);
                    }
                    let store = self.store.borrow();
                    let chat = store.chat(&chat_id)?;
                    let roster = self
                        .community
                        .as_ref()
                        .map(|v| v.members.as_slice())
                        .unwrap_or_default();
                    let rules = community.as_ref().and_then(|id| self.mention_rules.get(id));
                    let p = Rc::new(mentions::candidates(&store, chat, roster, rules));
                    drop(store);
                    self.mention_pool = Some(p.clone());
                    p
                }
            };
            let items = mentions::filter(&pool, query, 5);
            return (!items.is_empty()).then_some(mentions::Popup {
                start,
                items,
                active: 0,
            });
        }
        let (start, query) = mentions::trigger(text, caret, '#')?;
        let pool = mentions::channels(&self.store.borrow(), &chat_id);
        let items = mentions::filter(&pool, query, 8);
        (!items.is_empty()).then_some(mentions::Popup {
            start,
            items,
            active: 0,
        })
    }

    fn load_mention_rules(&mut self, community: String, cx: &mut Context<Self>) {
        self.mention_rules
            .insert(community.clone(), Default::default());
        let id = community.clone();
        let rx = backend::run(async move { vector_core::VectorCore.community_mentions(&id).ok() });
        cx.spawn(async move |this, cx| {
            let Ok(Some(v)) = rx.await else { return };
            let Ok(rules) = serde_json::from_value::<crate::mentions::Rules>(v) else {
                return;
            };
            this.update(cx, |this, cx| {
                this.mention_rules.insert(community, rules);
                if this.mention.is_some() {
                    this.mention_pool = None;
                    this.update_shortcode(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn pick_mention(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.mention.take() else { return };
        let Some(item) = p.items.get(i).cloned() else {
            return;
        };
        self.mention_pool = None;
        self.input.update(cx, |input, cx| {
            input.set_nav(false);
            let caret = input.caret();
            input.replace(p.start..caret, &crate::mentions::insert(&item), window, cx);
        });
        if !matches!(
            item.kind,
            crate::mentions::Kind::Channel { .. } | crate::mentions::Kind::Time
        )
            && !self
                .tracked
                .iter()
                .any(|(n, k)| *n == item.name && *k == item.npub)
        {
            self.tracked.push((item.name, item.npub));
            self.sync_mentions(cx);
        }
        self.mention = None;
        cx.notify();
    }

    fn sync_mentions(&mut self, cx: &mut Context<Self>) {
        let names = self.tracked.iter().map(|(n, _)| n.clone()).collect();
        self.input.update(cx, |i, cx| i.set_mentions(names, cx));
    }

    fn set_shortcode(&mut self, popup: Option<crate::shortcodes::Popup>, cx: &mut Context<Self>) {
        let open = popup.is_some();
        self.shortcode = popup;
        self.input.update(cx, |i, _| i.set_nav(open));
        cx.notify();
    }

    fn shortcode_nav(
        &mut self,
        nav: crate::composer::Nav,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::composer::Nav;
        if self.command_nav(nav, window, cx) {
            return;
        }
        if let Some(p) = self.time_pick.as_mut() {
            let n = p.styles().len();
            match nav {
                Nav::Up if n > 0 => p.active = (p.active + n - 1) % n,
                Nav::Down if n > 0 => p.active = (p.active + 1) % n,
                Nav::Pick if n > 0 => {
                    let i = p.active;
                    return self.pick_time(i, window, cx);
                }
                Nav::Close => {
                    self.time_dismissed = Some(p.at);
                    self.time_pick = None;
                    self.input.update(cx, |i, _| i.set_nav(false));
                }
                _ => {}
            }
            return cx.notify();
        }
        if self.mention.is_none()
            && self.shortcode.is_none()
            && let Some(chip) = &self.time_chip
        {
            match nav {
                Nav::Pick => return self.accept_time_chip(window, cx),
                Nav::Close => {
                    self.chip_passed.insert(chip.key());
                    self.time_chip = None;
                    self.input.update(cx, |i, _| i.set_chip(false));
                }
                _ => {}
            }
            return cx.notify();
        }
        if let Some(p) = self.mention.as_mut() {
            let n = p.items.len();
            match nav {
                Nav::Up => p.active = (p.active + n - 1) % n,
                Nav::Down => p.active = (p.active + 1) % n,
                Nav::Pick => {
                    let i = p.active;
                    return self.pick_mention(i, window, cx);
                }
                Nav::Close => {
                    self.mention = None;
                    self.mention_pool = None;
                    self.input.update(cx, |i, _| i.set_nav(false));
                }
            }
            return cx.notify();
        }
        let Some(p) = self.shortcode.as_mut() else {
            return self.input.update(cx, |i, _| i.set_nav(false));
        };
        let n = p.items.len();
        match nav {
            Nav::Up => p.active = (p.active + n - 1) % n,
            Nav::Down => p.active = (p.active + 1) % n,
            Nav::Pick => {
                let i = p.active;
                return self.pick_shortcode(i, window, cx);
            }
            Nav::Close => return self.set_shortcode(None, cx),
        }
        cx.notify();
    }

    fn pick_shortcode(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.shortcode.take() else {
            return;
        };
        let Some(cell) = p.items.get(i).cloned() else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.set_nav(false);
            let caret = input.caret();
            input.replace(
                p.start..caret,
                &crate::shortcodes::insert(&cell),
                window,
                cx,
            );
        });
        self.shortcode = None;
        match cell {
            crate::picker::Cell::Stock(e) => {
                self.emoji_picker.update(cx, |p, _| p.bump(e.emoji));
                if self.live {
                    let _ = backend::run(async move { backend::actions::bump_emoji(e.emoji) });
                }
            }
            crate::picker::Cell::Custom(e) => {
                self.emoji_picker
                    .update(cx, |p, _| p.bump(&format!(":{}:", e.code)));
                if self.live {
                    let (c, u) = (e.code.clone(), e.url.clone());
                    let _ =
                        backend::run(async move { backend::actions::bump_custom_emoji(&c, &u) });
                }
            }
        }
        cx.notify();
    }

    /// A link clicked in the app: a Vector place opens in place, anything else in the browser.
    fn follow_url(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        match vector_core::deep_link::parse_deep_link(url) {
            Some(a) => self.open_deep_link(a, window, cx),
            None => self.open_web(url, cx),
        }
    }

    /// `openUrl`: off Clearnet a web page loads outside the account's network, so it asks first.
    fn open_web(&mut self, url: &str, cx: &mut Context<Self>) {
        let web = url.starts_with("http://") || url.starts_with("https://");
        let label = if self.live && web { backend::calls::off_clearnet_label() } else { None };
        let Some(label) = label else {
            return cx.open_url(url);
        };
        let masked = self.store.borrow().stream_mask(url).unwrap_or_else(|| url.to_string());
        let shown = match masked.char_indices().nth(220) {
            Some((i, _)) => format!("{}…", &masked[..i]),
            None => masked,
        };
        let esc = |t: &str| t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let mut popup = kit::Popup::notice("Open in your browser?");
        popup.text = Some(format!("It loads outside {}.<br><br>{}", esc(&label), esc(&shown)).into());
        popup.confirm = "Open".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::OpenWeb(url.to_string())));
        cx.notify();
    }

    /// A link from the OS: held until an account is open.
    pub fn deep_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(a) = vector_core::deep_link::parse_deep_link(url) else {
            return;
        };
        window.activate_window();
        if matches!(self.screen, Screen::Main) {
            self.open_deep_link(a, window, cx);
        } else {
            self.pending_link = Some(a);
        }
    }

    /// `openDeepLink`: a link only ever opens a place; joining takes a tap there.
    fn open_deep_link(
        &mut self,
        a: vector_core::deep_link::DeepLinkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if a.account
            .as_ref()
            .is_some_and(|n| *n != self.store.borrow().my_npub)
        {
            return;
        }
        if a.action_type != "emoji_pack" && a.action_type != "community_invite" {
            self.pack_details = None;
        }
        match a.action_type.as_str() {
            "profile" => self.open_profile(a.target, cx),
            "chat" => {
                if self.open.as_deref() != Some(a.target.as_str()) {
                    self.open_chat(a.target.into(), window, cx);
                }
                if let Some(m) = a.message {
                    self.jump_to_message(m, cx);
                }
            }
            "emoji_pack" => self.open_pack_details(a.target, cx),
            "community_invite" => self.preview_invite_link(a.target, window, cx),
            "go" => match vector_core::golink::GoLink::parse_payload(&a.target) {
                Some(link) => self.open_go_link(link, window, cx),
                None => {
                    self.toast = Some(("That link is broken".into(), std::time::Instant::now()))
                }
            },
            "settings" => self.open_settings_path(&a.target, window, cx),
            "nexus" if a.target.is_empty() => self.show_nexus(None, window, cx),
            "nexus" => self.nexus_show_app(a.target, window, cx),
            "requests" | "home" => {
                self.go_home();
                if a.action_type == "requests" {
                    self.requests_tab = true;
                } else {
                    self.requests_tab = false;
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// The chat list, with nothing over it.
    fn go_home(&mut self) {
        self.leave_community();
        self.profile = None;
        self.settings = None;
        self.new_chat = None;
        self.open = None;
    }

    /// `category[/section[/group]]`: the category, at the deepest part this build has an anchor for.
    fn open_settings_path(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let path = match path {
            "theme" | "appearance/theme" => "appearance/display/appearance",
            p => p,
        };
        let mut parts = path.split('/').filter(|p| !p.is_empty());
        let cats = crate::settings::categories(&crate::settings::Offers {
            voice: self.live || std::env::var_os("VECTOR_TRANSCRIPT").is_some(),
            routing: true,
            updates: true,
        });
        let want = parts.next().unwrap_or("");
        let Some(cat) = cats.iter().find(|c| c.id == want).or(cats.first()) else {
            return;
        };
        let anchor = parts
            .rev()
            .find_map(|p| cat.anchors.iter().find(|a| a.id == p))
            .map(|a| a.id);
        let id = cat.id;
        self.open = None;
        self.open_settings(id, window, cx);
        if let Some(anchor) = anchor {
            self.settings_act(crate::settings::Act::Anchor(id, anchor), window, cx);
        }
    }

    /// `previewAndJoinCommunityLink`: a member goes in, anyone else is asked first.
    fn preview_invite_link(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = crate::newchat::invite_card_keys(&url).into_iter().next() else {
            self.notice(
                "Invalid Invite",
                "This invite link could not be loaded.".into(),
                cx,
            );
            return;
        };
        self.toast = Some((
            "Loading community invite…".into(),
            std::time::Instant::now(),
        ));
        let rx = if self.live {
            backend::run(backend::community::preview_invite(
                crate::newchat::invite_url(&key),
            ))
        } else {
            let (tx, rx) = futures::channel::oneshot::channel();
            let preview = std::env::var("VECTOR_INVITE_PREVIEW").map(|name| {
                crate::backend::community::InvitePreview {
                    community_id: "c".repeat(64),
                    name,
                    description: "Late-night builders, ships and shenanigans.".into(),
                    icon: None,
                }
            });
            tx.send(preview.map_err(|_| "Not found on any relay".to_string()))
                .ok();
            rx
        };
        cx.spawn_in(window, async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("Cancelled".into()));
            this.update_in(cx, |this, window, cx| {
                this.toast = None;
                match res {
                    Err(e) => this.notice(
                        "Invalid Invite",
                        format!("This invite link could not be loaded.\n\n{e}"),
                        cx,
                    ),
                    Ok(p) if this.store.borrow().community(&p.community_id).is_some() => {
                        this.open_community(p.community_id, window, cx)
                    }
                    Ok(p) => {
                        let mut text = format!("You've been invited to join {}.", p.name);
                        if !p.description.is_empty() {
                            text.push_str("\n\n");
                            text.push_str(&p.description);
                        }
                        this.popup = Some((
                            kit::Popup {
                                title: format!("Join {}?", p.name).into(),
                                text: Some(text.into()),
                                icon: Some("icons/group-placeholder.svg"),
                                confirm: "Confirm".into(),
                                cancel: Some("Cancel".into()),
                                input: None,
                                fine: None,
                                link: None,
                            },
                            PopupAct::JoinInvite { key, name: p.name },
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `openGoLink`: a Community, a channel, or a message, or why it can't be shown.
    fn open_go_link(
        &mut self,
        link: vector_core::golink::GoLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use vector_core::golink::GoLink;
        let (community, channel, message) = match link {
            GoLink::Community { community } => (Some(community), None, None),
            GoLink::Channel { community, channel } => (Some(community), Some(channel), None),
            GoLink::ChannelMessage {
                community,
                channel,
                message,
            } => (Some(community), Some(channel), Some(message)),
            GoLink::Message { message } => (None, None, Some(message)),
        };
        if let Some(community) = community {
            if self.store.borrow().community(&community).is_none() {
                return self.refuse_go("You're not in this community");
            }
            let Some(channel) = channel else {
                return self.open_community(community, window, cx);
            };
            if self.store.borrow().chat(&channel).is_none() {
                return self.refuse_go("That channel isn't available to you");
            }
            if self.open.as_deref() != Some(channel.as_str()) {
                self.open_channel(channel.clone(), window, cx);
            }
            let Some(message) = message else { return };
            self.locate_go_message(message, Some(channel), window, cx);
        } else if let Some(message) = message {
            self.locate_go_message(message, None, window, cx);
        }
    }

    fn refuse_go(&mut self, text: &str) {
        self.toast = Some((text.to_string().into(), std::time::Instant::now()));
    }

    /// Where a linked message is on this device; a link naming a room counts only there.
    fn locate_go_message(
        &mut self,
        message: String,
        room: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = if self.live {
            let m = message.clone();
            backend::run(async move {
                vector_core::db::scoped(async move { vector_core::db::events::locate_message(&m) })
                    .await
                    .unwrap_or_default()
            })
        } else {
            let chat = self
                .store
                .borrow()
                .chats
                .iter()
                .find(|c| c.messages.iter().any(|m| m.id == message))
                .map(|c| c.id.clone());
            let (tx, rx) = futures::channel::oneshot::channel();
            tx.send(vector_core::db::events::MessageLocation {
                chat,
                deleted: false,
            })
            .ok();
            rx
        };
        cx.spawn_in(window, async move |this, cx| {
            let Ok(at) = rx.await else { return };
            this.update_in(cx, |this, window, cx| {
                match (at.chat, room) {
                    (Some(chat), Some(room)) if chat == room => {
                        this.jump_or_say(message, "That message can't be shown", cx)
                    }
                    (Some(chat), None) => {
                        if this.open.as_deref() != Some(chat.as_str()) {
                            this.open_chat(chat.into(), window, cx);
                        }
                        this.jump_or_say(message, "That message can't be shown", cx);
                    }
                    (_, room) if at.deleted => {
                        let _ = room;
                        this.refuse_go("That message was deleted");
                    }
                    (_, Some(_)) => this.refuse_go("That message isn't on this device yet"),
                    (_, None) => this.refuse_go("That message isn't on this device"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `openPackDetailsModal`: the pack an naddr names, over every screen.
    fn open_pack_details(&mut self, naddr: String, cx: &mut Context<Self>) {
        let loading = matches!(
            self.media.borrow_mut().packs.get(&naddr),
            crate::packs::State::Loading
        );
        if loading {
            let queue = std::mem::take(&mut self.media.borrow_mut().packs.queue);
            self.fetch_packs(queue, cx);
        }
        self.pack_details = Some(crate::packdetails::Details::new(naddr));
        cx.notify();
    }

    fn pack_details_view(&mut self, narrow: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.pack_details.as_ref()?;
        let app = cx.entity().downgrade();
        let on: crate::packdetails::On = Rc::new(move |act, _, cx| {
            app.update(cx, |app, cx| match act {
                crate::packdetails::Act::Close => {
                    app.pack_details = None;
                    cx.notify();
                }
                crate::packdetails::Act::Toggle => {
                    if let Some(n) = app.pack_details.as_ref().map(|d| d.naddr.clone()) {
                        app.on_pack(crate::packs::Act::Toggle(n), cx);
                    }
                }
            })
            .ok();
        });
        let cap = if self.live {
            vector_core::emoji_packs::effective_display_cap()
        } else {
            usize::MAX
        };
        let store = self.store.borrow();
        let live = self.live;
        let src = |u: &str| {
            if live {
                u.to_string()
            } else {
                timeline::emoji_source(&store, u)
            }
        };
        let el = crate::packdetails::view(d, cap, &mut self.media.borrow_mut(), &src, narrow, on);
        crate::media::pump(&self.media, cx.entity().downgrade(), live, cx);
        Some(gpui::deferred(el).with_priority(13).into_any_element())
    }

    /// A dead pack's Remove Pack, from the picker.
    fn remove_pack(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let i = id.clone();
        let rx = backend::run(async move { vector_core::emoji_packs::unsubscribe_pack(&i).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("Cancelled".into()));
            this.update(cx, |this, cx| match res {
                Ok(()) => {
                    this.media.borrow_mut().packs.subscribed.remove(&id);
                    this.reload_picker_packs(cx);
                }
                Err(e) => this.notice("Couldn't Remove Pack", e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn reload_subscribed_packs(&mut self, cx: &mut Context<Self>) {
        self.reload_picker_packs(cx);
        if !self.live {
            return;
        }
        let rx = backend::run(async {
            vector_core::emoji_packs::load_subscriptions()
                .unwrap_or_default()
                .iter()
                .filter_map(|a| vector_core::emoji_packs::naddr_from_addr(a).ok())
                .collect::<std::collections::HashSet<_>>()
        });
        cx.spawn(async move |this, cx| {
            let Ok(ids) = rx.await else { return };
            this.update(cx, |this, cx| {
                this.media.borrow_mut().packs.subscribed = ids;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_pack(&mut self, act: crate::packs::Act, cx: &mut Context<Self>) {
        use crate::packs::{Act, State};
        match act {
            Act::Copy(naddr) => {
                let n = naddr.clone();
                let rx = self.live.then(|| {
                    backend::run(async move {
                        vector_core::emoji_packs::share_naddr(&n).await.unwrap_or(n)
                    })
                });
                cx.spawn(async move |this, cx| {
                    let share = match rx {
                        Some(rx) => rx.await.unwrap_or_else(|_| naddr.clone()),
                        None => naddr.clone(),
                    };
                    this.update(cx, |this, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!(
                            "https://vectorapp.io/emojis/pack/{share}"
                        )));
                        this.media.borrow_mut().packs.copied =
                            Some((naddr, std::time::Instant::now()));
                        cx.notify();
                    })
                    .ok();
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(1600))
                        .await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
            Act::Toggle(naddr) => {
                let State::Ready(pack) = self.media.borrow_mut().packs.get(&naddr) else {
                    return;
                };
                let id = pack.id.clone();
                let subscribed = self.media.borrow().packs.subscribed.contains(&id);
                let max = if self.live {
                    vector_core::emoji_packs::effective_max_equipped_packs()
                } else {
                    10
                };
                if !subscribed && self.emoji_picker.read(cx).pack_count() >= max {
                    self.notice(
                        "Pack Slots Full!",
                        format!(
                            "Vector supports up to {max} equipped packs. Remove one to add another."
                        ),
                        cx,
                    );
                    return;
                }
                self.media.borrow_mut().packs.busy.insert(id.clone());
                cx.notify();
                let live = self.live;
                let (n, i) = (naddr.clone(), id.clone());
                let rx = if live {
                    backend::run(async move {
                        if subscribed {
                            vector_core::emoji_packs::unsubscribe_pack(&i).await
                        } else {
                            vector_core::emoji_packs::subscribe_pack(&n)
                                .await
                                .map(|_| ())
                        }
                    })
                } else {
                    let (tx, rx) = futures::channel::oneshot::channel();
                    tx.send(Ok(())).ok();
                    rx
                };
                cx.spawn(async move |this, cx| {
                    // The label stays a beat: a local toggle lands in milliseconds.
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(350))
                        .await;
                    let res = rx.await.unwrap_or_else(|_| Err("Cancelled".into()));
                    this.update(cx, |this, cx| {
                        let mut media = this.media.borrow_mut();
                        media.packs.busy.remove(&id);
                        match res {
                            Ok(()) if subscribed => {
                                media.packs.subscribed.remove(&id);
                                drop(media);
                                this.reload_picker_packs(cx);
                            }
                            Ok(()) => {
                                media.packs.subscribed.insert(id);
                                drop(media);
                                this.reload_picker_packs(cx);
                                if this
                                    .pack_details
                                    .as_ref()
                                    .is_some_and(|d| d.naddr.eq_ignore_ascii_case(&naddr))
                                {
                                    this.pack_details = None;
                                    this.toast =
                                        Some(("Pack equipped".into(), std::time::Instant::now()));
                                }
                            }
                            Err(e) => {
                                drop(media);
                                this.notice(
                                    if subscribed {
                                        "Couldn't Remove Pack"
                                    } else {
                                        "Couldn't Add Pack"
                                    },
                                    e,
                                    cx,
                                );
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// A reference the expanded view was waiting on.
    fn embed_settled(&mut self, entity: &str, cx: &mut Context<Self>) {
        if self.embed_wanted.as_ref().is_some_and(|(e, _)| e == entity) {
            let (entity, url) = self.embed_wanted.take().unwrap_or_default();
            self.push_embed(entity, url, cx);
        }
    }

    fn fetch_embeds(&mut self, entities: Vec<String>, cx: &mut Context<Self>) {
        for entity in entities {
            if !self.live {
                let fix = self.store.borrow().fixture_embeds.get(&entity).cloned();
                let res = fix
                    .ok_or_else(|| "Not found on any relay".to_string())
                    .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()));
                self.media.borrow_mut().embeds.settle(&entity, res);
                self.embed_settled(&entity, cx);
                cx.notify();
                continue;
            }
            let reference = entity.clone();
            let rx = backend::run(async move { vector_core::nostr_embed::fetch(&reference).await });
            cx.spawn(async move |this, cx| {
                let res = match rx.await {
                    Ok(r) => r.and_then(crate::embeds::Embed::from_core),
                    Err(_) => return,
                };
                this.update(cx, |this, cx| {
                    if let Ok(e) = &res {
                        let store = this.store.borrow();
                        let unknown: Vec<String> = [Some(&e.author), e.reposted_by.as_ref()]
                            .into_iter()
                            .flatten()
                            .filter(|n| !store.profiles.contains_key(*n))
                            .cloned()
                            .collect();
                        drop(store);
                        for npub in unknown {
                            let _ = backend::run(async move {
                                vector_core::profile::sync::queue_profile_sync(
                                    npub,
                                    vector_core::profile::sync::SyncPriority::High,
                                    false,
                                )
                            });
                        }
                    }
                    this.media.borrow_mut().embeds.settle(&entity, res);
                    this.embed_settled(&entity, cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Escape closes the topmost light overlay; true when it did.
    fn escape_overlay(&mut self) -> bool {
        if let Some(mut s) = self.scanner.take() {
            self.retired_frames.extend(s.frame.take());
            self.retired_frames.append(&mut s.retired);
            return true;
        }
        if self.viewer.is_some() || self.model_viewer.is_some() {
            return false;
        }
        if self.screen_pick.is_some() {
            self.close_screen_pick();
            return true;
        }
        if self.call.maximized.take().is_some() || self.call.pref_open.take().is_some() {
            return true;
        }
        if self.menu.take().is_some()
            || self.net_info.take().is_some()
            || self.mini_profile.take().is_some()
            || self.edit_history.take().is_some()
            || self.reaction_details.take().is_some()
            || self.secret_guard.take().is_some()
            || self.perm_prompt.take().is_some()
            || self.publish.take_if(|f| !f.busy()).is_some()
            || self.launch.take().is_some()
            || self.nexus.as_mut().and_then(|n| n.picker.take()).is_some()
            || self.pack_details.take().is_some()
            || self.embed_modal.take().is_some()
            || self.invite_details.take().is_some()
        {
            return true;
        }
        self.create_channel.take().is_some() || self.invite.take().is_some()
    }

    /// Show `entity` in the expanded view, over whatever it shows now, once it resolves.
    fn push_embed(&mut self, entity: String, url: Option<String>, cx: &mut Context<Self>) {
        let state = self.media.borrow_mut().embeds.get(&entity);
        match state {
            crate::embeds::State::Ready(embed) => {
                let view = crate::embeds::View {
                    embed,
                    url,
                    revealed: false,
                };
                match self.embed_modal.as_mut() {
                    Some(m) => {
                        m.stack.push(view);
                        m.scroll.set_offset(gpui::point(px(0.), px(0.)));
                    }
                    None => {
                        self.embed_modal = Some(crate::embeds::Modal {
                            stack: vec![view],
                            scroll: gpui::ScrollHandle::new(),
                        })
                    }
                }
            }
            crate::embeds::State::Failed(err, _) => {
                self.toast = Some((err.into(), std::time::Instant::now()))
            }
            crate::embeds::State::Loading => {
                self.embed_wanted = Some((entity, url));
                let queued = std::mem::take(&mut self.media.borrow_mut().embeds.queue);
                self.fetch_embeds(queued, cx);
            }
        }
        cx.notify();
    }

    fn on_embed(&mut self, act: crate::embeds::Act, _window: &mut Window, cx: &mut Context<Self>) {
        use crate::embeds::Act;
        match act {
            Act::OpenUrl(url) => self.open_web(&url, cx),
            Act::Copy(text, label) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.toast = Some((format!("{label} copied").into(), std::time::Instant::now()));
            }
            Act::Reveal(key) => {
                if key.is_empty() {
                    if let Some(v) = self.embed_modal.as_mut().and_then(|m| m.stack.last_mut()) {
                        v.revealed = true;
                    }
                } else {
                    self.media.borrow_mut().embeds.revealed.insert(key);
                }
            }
            Act::Profile(npub) => self.open_profile(npub, cx),
            Act::View(open) => {
                return self.on_act(String::new(), timeline::Act::View(open), _window, cx);
            }
            Act::Expand(entity, url) => {
                self.embed_modal = None;
                self.push_embed(entity, url, cx);
            }
            Act::Ref(entity) => self.push_embed(entity, None, cx),
            Act::Push(entity, url) => self.push_embed(entity, url, cx),
            Act::Back => {
                if let Some(m) = self.embed_modal.as_mut() {
                    m.stack.pop();
                    if m.stack.is_empty() {
                        self.embed_modal = None;
                    }
                }
            }
            Act::Close => self.embed_modal = None,
            Act::Video(v) => {
                let mut media = self.media.borrow_mut();
                match media.embed_videos.get(&v.url) {
                    Some(crate::embeds::VideoState::Loading(_)) => {
                        media.embed_autoplay.remove(&v.url);
                        let url = v.url.clone();
                        drop(media);
                        let _ = backend::run(async move { backend::embedvideo::cancel(&url) });
                    }
                    Some(crate::embeds::VideoState::Ready(_)) => {}
                    _ => {
                        media.embed_videos.insert(v.url.clone(), crate::embeds::VideoState::Loading(-1));
                        media.embed_autoplay.insert(v.url.clone());
                        drop(media);
                        self.fetch_embed_video(v, cx);
                    }
                }
            }
            Act::Clip(id, a) => {
                let url = id.strip_prefix("embed:").unwrap_or(&id);
                let mut media = self.media.borrow_mut();
                let input = match media.embed_videos.get(url) {
                    Some(crate::embeds::VideoState::Ready(spec)) => Some(spec.clone()),
                    _ => None,
                };
                media.videos.act(&id, input, a);
            }
        }
        cx.notify();
    }

    fn fetch_embed_preview(&mut self, v: crate::embeds::Media, cx: &mut Context<Self>) {
        let url = v.url.clone();
        let rx = backend::run(backend::embedvideo::preview(v.url, v.sha256));
        cx.spawn(async move |this, cx| {
            let spec = match rx.await {
                Ok(Ok(backend::embedvideo::Fetched::Held(h))) => crate::audio::AudioInputSpec::Held(h),
                Ok(Ok(backend::embedvideo::Fetched::Sealed(p, mime))) => crate::audio::AudioInputSpec::Sealed(p, mime),
                _ => return,
            };
            this.update(cx, |this, cx| {
                this.media.borrow_mut().embed_previews.insert(url, Some(spec));
                this.refresh_rows(false);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fetch_embed_video(&mut self, v: crate::embeds::Media, cx: &mut Context<Self>) {
        let url = v.url.clone();
        let rx = backend::run(backend::embedvideo::fetch(v.url, v.size, v.sha256, v.fallbacks));
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update(cx, |this, cx| {
                use crate::embeds::VideoState;
                let mut media = this.media.borrow_mut();
                match result {
                    Ok(backend::embedvideo::Fetched::Held(h)) => {
                        media.embed_videos.insert(url, VideoState::Ready(crate::audio::AudioInputSpec::Held(h)));
                    }
                    Ok(backend::embedvideo::Fetched::Sealed(p, mime)) => {
                        media.embed_videos.insert(url, VideoState::Ready(crate::audio::AudioInputSpec::Sealed(p, mime)));
                    }
                    Err(e) if backend::embedvideo::is_cancel(&e) => {
                        media.embed_videos.remove(&url);
                        media.embed_autoplay.remove(&url);
                    }
                    Err(e) => {
                        media.embed_autoplay.remove(&url);
                        media.embed_videos.insert(url, VideoState::Failed(e));
                    }
                }
                drop(media);
                this.refresh_rows(false);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn scan_invites(&mut self, cx: &mut Context<Self>) {
        let keys = {
            let store = self.store.borrow();
            let Some(chat) = self.open.as_deref().and_then(|id| store.chat(id)) else {
                return;
            };
            let stamp = (
                chat.id.clone(),
                chat.messages.len(),
                chat.messages
                    .last()
                    .map(|m| m.id.clone())
                    .unwrap_or_default(),
            );
            if self.invite_scan.as_ref() == Some(&stamp) {
                return;
            }
            self.invite_scan = Some(stamp);
            if store.is_request(chat) {
                return;
            }
            chat.messages
                .iter()
                .flat_map(|m| crate::newchat::invite_card_keys(&m.content))
                .filter(|k| !self.invite_previews.contains_key(k))
                .collect::<Vec<_>>()
        };
        for key in keys {
            self.resolve_invite(key, cx);
        }
    }

    fn resolve_invite(&mut self, key: String, cx: &mut Context<Self>) {
        if matches!(
            self.invite_previews.get(&key),
            Some(crate::newchat::Preview::Ready(_) | crate::newchat::Preview::Loading)
        ) {
            return;
        }
        self.invite_previews
            .insert(key.clone(), crate::newchat::Preview::Loading);
        if !self.live {
            let preview = std::env::var("VECTOR_INVITE_PREVIEW").ok().map(|name| {
                crate::backend::community::InvitePreview {
                    community_id: "c".repeat(64),
                    name,
                    description: "Late-night builders, ships and shenanigans.".into(),
                    icon: None,
                }
            });
            self.invite_previews.insert(
                key,
                preview.map_or(
                    crate::newchat::Preview::Failed,
                    crate::newchat::Preview::Ready,
                ),
            );
            return;
        }
        let rx = backend::run(backend::community::preview_invite(
            crate::newchat::invite_url(&key),
        ));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                let state = match res {
                    Ok(Ok(p)) => crate::newchat::Preview::Ready(p),
                    _ => crate::newchat::Preview::Failed,
                };
                this.invite_previews.insert(key, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn join_by_link(
        &mut self,
        key: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toast = Some((format!("Joining {name}…").into(), std::time::Instant::now()));
        let rx = backend::run(backend::community::join_by_link(
            crate::newchat::invite_url(&key),
        ));
        cx.spawn_in(window, async move |this, cx| {
            let res = rx.await;
            this.update_in(cx, |this, window, cx| {
                if let Some(n) = this.new_chat.as_mut() {
                    n.joining = false;
                }
                match res {
                    Ok(Ok(id)) => {
                        this.new_chat = None;
                        let rx = backend::run(async { backend::community::list() });
                        cx.spawn_in(window, async move |this, cx| {
                            let Ok(list) = rx.await else { return };
                            let list: Vec<data::Community> = list
                                .into_iter()
                                .filter_map(|v| serde_json::from_value(v).ok())
                                .collect();
                            this.update_in(cx, |this, window, cx| {
                                this.store.borrow_mut().communities = list;
                                this.open_community(id, window, cx);
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                    }
                    Ok(Err(e)) => this.notice("Failed to Join", e, cx),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_invite_details(
        &mut self,
        community: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invite_details = Some(community);
        cx.notify();
    }

    fn reload_invites(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let rx = backend::run(backend::community::pending_invites());
        cx.spawn(async move |this, cx| {
            let Ok(list) = rx.await else { return };
            this.update(cx, |this, cx| {
                this.store.borrow_mut().invites =
                    list.into_iter().map(data::Invite::from).collect();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn answer_invite(
        &mut self,
        community: String,
        accept: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        self.invite_details = None;
        let name = {
            let mut store = self.store.borrow_mut();
            let name = store
                .invites
                .iter()
                .find(|i| i.community_id == community)
                .map(|i| i.name.clone())
                .unwrap_or_default();
            store.invites.retain(|i| i.community_id != community);
            name
        };
        cx.notify();
        if !self.live {
            return;
        }
        if !accept {
            let _ = backend::run(backend::community::decline_invite(community));
            return;
        }
        self.toast = Some((format!("Joining {name}…").into(), std::time::Instant::now()));
        let rx = backend::run(backend::community::accept_invite(community.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let res = rx.await;
            this.update_in(cx, |this, window, cx| match res {
                Ok(Ok(())) => {
                    let rx = backend::run(async { backend::community::list() });
                    cx.spawn_in(window, async move |this, cx| {
                        let Ok(list) = rx.await else { return };
                        let list: Vec<data::Community> = list
                            .into_iter()
                            .filter_map(|v| serde_json::from_value(v).ok())
                            .collect();
                        this.update_in(cx, |this, window, cx| {
                            this.store.borrow_mut().communities = list;
                            this.open_community(community, window, cx);
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                Ok(Err(e)) => {
                    this.notice("Couldn't Join", e, cx);
                    this.reload_invites(cx);
                }
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    /// Back to Messages: the chat list returns in place of the channels.
    fn leave_community(&mut self) {
        if self.community.take().is_some() {
            self.open = None;
            if self.live {
                messaging::set_active_chat(None);
            }
        }
    }

    fn reload_communities(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let open = self.community.as_ref().map(|v| v.id.clone());
        let rx = backend::run(async move {
            let named = open.map(|id| {
                let n = backend::community::nicknames(&id);
                (id, n)
            });
            (
                backend::community::list(),
                named,
                backend::community::hidden_banners(),
            )
        });
        cx.spawn(async move |this, cx| {
            let Ok((list, named, hidden)) = rx.await else {
                return;
            };
            let list: Vec<data::Community> = list
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            this.update(cx, |this, cx| {
                this.store.borrow_mut().communities = list;
                this.store.borrow_mut().hidden_banners = hidden;
                this.resolve_art(None, cx);
                if let Some((id, n)) = named {
                    this.store.borrow_mut().nicknames.insert(id, n);
                }
                let gone = this
                    .community
                    .as_ref()
                    .is_some_and(|v| this.store.borrow().community(&v.id).is_none());
                if gone {
                    this.leave_community();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_profile(&mut self, npub: String, cx: &mut Context<Self>) {
        let back = self.open.take();
        self.new_chat = None;
        if self.live {
            backend::prefs::refresh(npub.clone());
        }
        self.profile = Some(crate::profile::Screen::new(npub.clone(), back));
        cx.notify();
        if !self.live {
            // Fixtures: VECTOR_BADGES=invites,fawkes,bug,v2026 (counts; 0 for none) on our own profile.
            if let Ok(spec) = std::env::var("VECTOR_BADGES")
                && npub == self.store.borrow().my_npub
            {
                let n: Vec<u32> = spec
                    .split(',')
                    .map(|v| v.trim().parse().unwrap_or(0))
                    .collect();
                let at = |i: usize| n.get(i).copied().unwrap_or(0);
                if let Some(p) = self.profile.as_mut() {
                    p.badges = backend::badges::Badges {
                        invites: at(0),
                        fawkes: at(1) > 0,
                        bug: at(2) as u8,
                        v2026: at(3) > 0,
                    };
                }
            }
            return;
        }
        let mine = npub == self.store.borrow().my_npub;
        let rx = backend::run(backend::badges::of(npub.clone(), mine));
        cx.spawn(async move |this, cx| {
            let Ok(badges) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(p) = this.profile.as_mut().filter(|p| p.npub == npub) {
                    p.badges = badges;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_profile(
        &mut self,
        act: crate::profile::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::profile::Act;
        let Some(p) = self.profile.as_mut() else {
            return;
        };
        let npub = p.npub.clone();
        match act {
            Act::Back => {
                let back = p.back.clone();
                self.profile = None;
                match back {
                    Some(id) => self.open_chat(id, window, cx),
                    None => cx.notify(),
                }
            }
            Act::Message => self.open_chat(npub.into(), window, cx),
            Act::ToggleMute => {
                if !self.live {
                    if let Some(chat) = self.store.borrow_mut().chat_mut(&npub) {
                        chat.muted = !chat.muted;
                    }
                    cx.notify();
                    return;
                }
                let rx = backend::run(backend::prefs::toggle_mute(npub.clone()));
                cx.spawn(async move |this, cx| {
                    let Ok(Some(muted)) = rx.await else { return };
                    this.update(cx, |this, cx| {
                        if let Some(chat) = this.store.borrow_mut().chat_mut(&npub) {
                            chat.muted = muted;
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Act::Share | Act::CopyNpub => {
                let (text, done) = if act == Act::Share {
                    (
                        format!("https://vectorapp.io/profile/{npub}"),
                        "Profile Link Copied",
                    )
                } else {
                    (npub.clone(), "Copied npub")
                };
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                p.copied = Some((act, std::time::Instant::now()));
                self.show_toast(done, cx);
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(2010))
                        .await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
            Act::More => {
                p.more_open = !p.more_open;
                cx.notify();
            }
            Act::Nickname => {
                p.more_open = false;
                let input = cx.new(|cx| {
                    TextInput::new("Nickname", cx)
                        .with_look(kit::field_look())
                        .keep_on_submit()
                        .submit_empty()
                });
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| {
                    app.answer_popup(true, window, cx)
                })
                .detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
                    app.answer_popup(false, window, cx)
                })
                .detach();
                window.focus(&input.focus_handle(cx), cx);
                let mut popup = kit::Popup::notice("Choose a Nickname");
                popup.icon = None;
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                popup.input = Some(input);
                self.popup = Some((popup, PopupAct::Nickname { npub }));
                cx.notify();
            }
            Act::Block => {
                p.more_open = false;
                let blocked = self
                    .store
                    .borrow()
                    .profiles
                    .get(&npub)
                    .is_some_and(|p| p.is_blocked);
                if blocked {
                    self.set_blocked(npub, false, cx);
                } else {
                    let mut popup = kit::Popup::notice("Block User");
                    popup.text = Some("Are you sure you want to block this user? You will no longer receive DMs from them.".into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::Block { npub }));
                    cx.notify();
                }
            }
            Act::SetStatus => self.open_status(window, cx),
            Act::Qr => self.show_profile_qr(&npub, window, cx),
            Act::Edit => {
                let Some(me) = self.store.borrow().me().cloned() else {
                    return;
                };
                let look = crate::composer::Look {
                    line_height: px(20.),
                    color: c(TEXT),
                    ..kit::field_look()
                };
                let name = cx.new(|cx| {
                    let mut i = TextInput::new("", cx)
                        .with_look(look.clone())
                        .centered()
                        .keep_on_submit();
                    i.set_text(&me.name, cx);
                    i
                });
                let bio = cx.new(|cx| {
                    let mut i = TextInput::new("", cx)
                        .with_look(look)
                        .centered()
                        .multiline(px(100000.))
                        .keep_on_submit();
                    i.set_text(&me.about, cx);
                    i
                });
                for input in [&name, &bio] {
                    cx.subscribe(input, |_, _, _: &crate::composer::InputChanged, cx| {
                        cx.notify()
                    })
                    .detach();
                }
                window.focus(&name.focus_handle(cx), cx);
                p.edit = Some(crate::profile::Edit {
                    name,
                    bio,
                    removed_banner: false,
                    snap_name: me.name.trim().to_string(),
                    snap_about: me.about.trim().to_string(),
                    avatar: None,
                    banner: None,
                });
                cx.notify();
            }
            Act::RemoveBanner => {
                if let Some(e) = p.edit.as_mut() {
                    e.removed_banner = true;
                }
                cx.notify();
            }
            Act::Pick(banner) => self.pick_picture(banner, window, cx),
            Act::ExitEdit(cancel) => {
                let Some(mut e) = p.edit.take() else { return };
                cx.notify();
                if cancel {
                    return;
                }
                for (pick, banner) in [(e.avatar.take(), false), (e.banner.take(), true)] {
                    if let Some(pick) = pick {
                        self.save_picture(&npub, pick, banner, cx);
                    }
                }
                let name = e.name.read(cx).text().trim().to_string();
                let about = e.bio.read(cx).text().trim().to_string();
                let (name_changed, about_changed) = (name != e.snap_name, about != e.snap_about);
                if let Some(me) = self.store.borrow_mut().profiles.get_mut(&npub) {
                    me.name = name.clone();
                    me.about = about.clone();
                    if e.removed_banner {
                        me.banner.clear();
                        me.banner_cached.clear();
                    }
                }
                self.show_toast("Profile Saved", cx);
                if !self.live || !(name_changed || about_changed || e.removed_banner) {
                    return;
                }
                // Empty leaves a field as it is; a blank clears it.
                let name = if name_changed { name } else { String::new() };
                let banner = if e.removed_banner {
                    " ".to_string()
                } else {
                    String::new()
                };
                let about = if about_changed {
                    if about.is_empty() {
                        " ".to_string()
                    } else {
                        about
                    }
                } else {
                    String::new()
                };
                let rx = backend::run(vector_core::profile::sync::update_profile(
                    name,
                    String::new(),
                    banner,
                    about,
                    &sync::ProfileHandler,
                ));
                cx.spawn(async move |this, cx| {
                    if let Ok(true) = rx.await {
                        return;
                    }
                    this.update(cx, |this, cx| {
                        let mut popup = kit::Popup::notice("Profile Update Failed!");
                        popup.text =
                            Some("Failed to broadcast profile update to the network.".into());
                        this.popup = Some((popup, PopupAct::Notice));
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Act::Switcher => {
                if self.switcher.is_some() {
                    self.close_switcher(cx);
                } else {
                    self.open_switcher(cx);
                }
            }
            Act::Badge(which) => {
                self.badge_card = Some(crate::badgecard::Card::of(which, p.badges));
                cx.notify();
            }
            Act::SwitcherEdit => {
                if let Some(s) = self.switcher.as_mut() {
                    s.editing = !s.editing;
                    let editing = s.editing;
                    if let Some(p) = self.profile.as_mut() {
                        p.switcher = Some(editing);
                    }
                    cx.notify();
                }
            }
        }
    }

    fn show_profile_qr(&mut self, npub: &str, window: &mut Window, cx: &mut Context<Self>) {
        let side = (((self.vw * 0.72).min(320.) - 32.) * window.scale_factor()).round() as u32;
        if let Some(image) = crate::qr::image(&format!("https://vectorapp.io/profile/{npub}"), side) {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            self.qr = Some((image, focus));
            cx.notify();
        }
    }

    /// `openQrScanner`: needs a camera decoder this client doesn't carry yet.
    fn open_switcher(&mut self, cx: &mut Context<Self>) {
        let active = self.store.borrow().my_npub.clone();
        if !self.live {
            let rows = self.fixture_accounts();
            self.show_switcher(rows, active, false, cx);
            return;
        }
        let rx = backend::run(async { (account::accounts(), account::account_cap()) });
        cx.spawn(async move |this, cx| {
            let Ok((accounts, cap)) = rx.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let at_cap = accounts.len() >= cap;
                let rows = accounts
                    .into_iter()
                    .map(|a| crate::switcher::Row {
                        npub: a.npub,
                        name: a.name,
                        avatar: a.avatar.map(Into::into),
                    })
                    .collect();
                this.show_switcher(rows, active, at_cap, cx);
            })
            .ok();
        })
        .detach();
    }

    fn open_settings(&mut self, cat: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings::{Home, Settings};
        let slim = self.is_slim;
        let search = cx.new(|cx| TextInput::new("Search", cx).with_look(settings_search_look(slim)));
        cx.subscribe_in(
            &search,
            window,
            |_, _, _: &crate::composer::InputChanged, _, cx| cx.notify(),
        )
        .detach();
        let seen = backend::device::get("settings_seen")
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default();
        let sql_keys: Vec<(&'static str, bool)> = crate::settings::toggles()
            .filter_map(|t| match t.home {
                Home::Sql(default) => Some((t.key, default)),
                _ => None,
            })
            .collect();
        let sql = if self.live {
            backend::prefs::sql_bools(&sql_keys)
        } else {
            sql_keys.iter().map(|(_, d)| *d).collect()
        };
        let mut on: HashMap<&'static str, bool> =
            sql_keys.iter().map(|(k, _)| *k).zip(sql).collect();
        on.insert("encryption", self.security.update(cx, |f, _| f.sync()));
        for t in crate::settings::toggles() {
            if let Home::Device(yes, no, default) = t.home {
                on.insert(
                    t.key,
                    match backend::device::get(t.key) {
                        Some(v) if v == yes => true,
                        Some(v) if v == no => false,
                        _ => default,
                    },
                );
            }
        }
        let synced = if self.live {
            Some(backend::prefs::synced_settings())
        } else {
            None
        };
        on.insert("advanced", synced.as_ref().is_some_and(|s| s.advanced));
        on.insert("players", !self.store.borrow().players_off);
        on.insert(
            "streamer_on",
            synced.as_ref().is_some_and(|s| s.streamer.on),
        );
        on.insert(
            "streamer_wallpapers",
            synced.as_ref().is_none_or(|s| s.streamer.hide_wallpapers),
        );
        let me = self.store.borrow().my_npub.clone();
        on.insert(
            "stream_consent",
            self.store
                .borrow()
                .profiles
                .get(&me)
                .is_some_and(|p| p.stream_consent),
        );
        let theme = if self.live {
            backend::prefs::sql("theme")
        } else {
            None
        }
        .unwrap_or_else(|| "vector".into());
        let notif_privacy = if self.live {
            backend::prefs::sql("notif_content_privacy")
        } else {
            None
        }
        .unwrap_or_else(|| "full".into());
        self.profile = None;
        self.new_chat = None;
        self.switcher = None;
        self.settings = Some(Settings {
            cat,
            search,
            scroll: gpui::ScrollHandle::new(),
            offers: crate::settings::Offers {
                voice: self.live || std::env::var_os("VECTOR_TRANSCRIPT").is_some(),
                routing: vector_core::transport::supported().len() > 1,
                updates: true,
            },
            theme,
            open_select: None,
            on,
            flipped: HashSet::new(),
            streamer_notif: synced
                .map(|s| s.streamer.notif)
                .unwrap_or_else(|| "none".into()),
            notif_privacy,
            storage: None,
            relays: None,
            servers: None,
            clock: None,
            dialog: None,
            sec_kind: self.security.read(cx).kind().to_string(),
            signer: if self.live {
                backend::bunker::status()
            } else {
                std::env::var("VECTOR_SIGNER").ok()
            }
            .map(|npub| (npub, vector_core::bunker_state().as_label())),
            sec_advanced: false,
            updates: self.updates.clone(),
            audio: None,
            sliders: Default::default(),
            dragging: Default::default(),
            donut: Default::default(),
            auto_limit: if self.live {
                backend::prefs::sql("max_auto_download_bytes")
                    .and_then(|v| v.parse().ok())
                    .filter(|v: &u64| *v > 0)
            } else {
                None
            }
            .unwrap_or(10_485_760),
            clearing: false,
            moving: None,
            notif_sound: if self.live {
                backend::prefs::sql("notif_sound")
            } else {
                None
            }
            .unwrap_or_else(|| "default".into()),
            sound_custom: false,
            blocked: None,
            voice: None,
            seen: seen.into_iter().collect(),
            jumped: None,
            net: Default::default(),
            book: Default::default(),
            slim: self.is_slim,
            slim_open: None,
            narrow: false,
            phone: false,
            slim_geo: Default::default(),
            slim_shut: Vec::new(),
            slim_scroll: gpui::ScrollHandle::new(),
        });
        self.load_routing(window, cx);
        self.load_blocked(cx);
        self.load_audio(cx);
        self.load_voice(cx);
        self.load_storage(cx);
        self.load_network(cx);
        self.mark_settings_seen(None);
        cx.notify();
    }

    /// `renderBlossomInfo`: what is held paints first, then the document, then the stats it moved.
    fn load_server_dialog(&mut self, url: String, enabled: bool, cx: &mut Context<Self>) {
        use crate::serverinfo::Load;
        fn with(app: &mut VectorApp, url: &str, f: impl FnOnce(&mut crate::netdialog::NetDialog)) {
            let Some(d) = app.settings.as_mut().and_then(|s| s.dialog.as_mut()) else { return };
            if matches!(d, crate::netdialog::NetDialog::Server { info, .. } if info.url == url) {
                f(d);
            }
        }
        if !self.live {
            let demo = self.store.borrow().fixture_servers.get(&url).cloned().unwrap_or_default();
            with(self, &url, |d| {
                let crate::netdialog::NetDialog::Server { stats, doc, caps, .. } = d else { return };
                *stats = Some(demo["stats"].clone()).filter(|v| v.is_object());
                *doc = Load::Ok(Some(demo["info"].clone()).filter(|v| v.is_object()));
                *caps = Load::Ok(demo["caps"].as_array().cloned().unwrap_or_default());
            });
            return;
        }
        let (u, e) = (url.clone(), enabled);
        let rx = backend::run(async move {
            let caps = backend::network::server_caps(u.clone()).await;
            let snap = backend::network::server_snapshot(u.clone(), e).await;
            (caps, snap)
        });
        let (u, e) = (url.clone(), enabled);
        let fresh = backend::run(async move {
            let doc = backend::network::server_info(u.clone()).await;
            let stats = backend::network::server_stats(u, e).await;
            (doc, stats)
        });
        cx.spawn(async move |this, cx| {
            if let Ok((caps, (held, stats, status))) = rx.await {
                this.update(cx, |app, cx| {
                    with(app, &url, |d| {
                        let crate::netdialog::NetDialog::Server { info, stats: s, doc, caps: c, .. } = d else { return };
                        *c = match caps {
                            Ok(list) => Load::Ok(list),
                            Err(_) => Load::Error,
                        };
                        *s = stats;
                        info.status = status;
                        if held.is_some() {
                            *doc = Load::Ok(held);
                        }
                    });
                    cx.notify();
                })
                .ok();
            }
            let Ok((doc, (stats, status))) = fresh.await else { return };
            this.update(cx, |app, cx| {
                with(app, &url, |d| {
                    let crate::netdialog::NetDialog::Server { info, stats: s, doc: dd, .. } = d else { return };
                    *dd = match doc {
                        Ok(v) => Load::Ok(v),
                        Err(_) => Load::Error,
                    };
                    *s = stats;
                    info.status = status;
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_net(&mut self, act: crate::netdialog::Act, _window: &mut Window, cx: &mut Context<Self>) {
        use crate::netdialog::{Act, NetDialog};
        let Some(s) = self.settings.as_mut() else {
            return;
        };
        let Some(d) = s.dialog.as_mut() else { return };
        cx.notify();
        match (act, d) {
            (Act::Close, _) => s.dialog = None,
            (Act::Mode(m), NetDialog::AddRelay { mode, .. }) => *mode = m,
            (Act::Add, NetDialog::AddRelay { input, mode }) => {
                let typed = input.read(cx).text().trim().to_string();
                if typed.is_empty() {
                    let mut popup = kit::Popup::notice("Invalid URL");
                    popup.icon = None;
                    popup.text = Some("Please enter a relay URL.".into());
                    self.popup = Some((popup, PopupAct::Notice));
                    return;
                }
                // Plain ws:// only for .i2p and .onion hosts, where the network encrypts end to end.
                let lower = typed.to_lowercase();
                let tls = lower.starts_with("wss://");
                let bare = typed
                    .get(
                        if tls {
                            6
                        } else if lower.starts_with("ws://") {
                            5
                        } else {
                            0
                        }..,
                    )
                    .unwrap_or("")
                    .to_string();
                let host = bare
                    .split(['/', ':', '?', '#'])
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                let native = host.ends_with(".i2p") || host.ends_with(".onion");
                let url = format!("{}{bare}", if native && !tls { "ws://" } else { "wss://" });
                let mode = mode.to_string();
                let rx = backend::run(async move {
                    backend::network::add_custom_relay(url, Some(mode))
                        .await
                        .map(|_| ())
                });
                cx.spawn(async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update(cx, |this, cx| {
                        match result {
                            Ok(()) => {
                                if let Some(s) = this.settings.as_mut() {
                                    s.dialog = None;
                                }
                            }
                            Err(e) => {
                                let mut popup = kit::Popup::notice("Failed to Add Relay");
                                popup.icon = None;
                                popup.text = Some(e.into());
                                this.popup = Some((popup, PopupAct::Notice));
                            }
                        }
                        this.load_network(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            (Act::ToggleModeList, NetDialog::Relay { mode_open, .. }) => *mode_open = !*mode_open,
            (Act::RelayMode(m), NetDialog::Relay { row, mode_open, .. }) => {
                *mode_open = false;
                if row.is_default || row.mode == m {
                    return;
                }
                row.mode = m.to_string();
                let (url, mode) = (row.url.clone(), m.to_string());
                self.net_change(
                    backend::run(async move {
                        backend::network::update_relay_mode(url, mode)
                            .await
                            .map(|_| ())
                    }),
                    false,
                    cx,
                );
            }
            (
                Act::CopyLogs,
                NetDialog::Relay {
                    row, logs, copied, ..
                },
            ) => {
                let text = logs
                    .iter()
                    .map(|l| {
                        let at = chrono::DateTime::from_timestamp(l.timestamp as i64, 0)
                            .map(|t| {
                                t.with_timezone(&chrono::Local)
                                    .format("%-I:%M:%S %p")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        format!("[{at}] {}: {}", l.level.to_uppercase(), l.message)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!(
                    "Relay: {}\n\n{text}",
                    row.url
                )));
                *copied = true;
            }
            (Act::RelayAction, NetDialog::Relay { row, .. }) => {
                let url = row.url.clone();
                if row.is_default && row.enabled {
                    let mut popup = kit::Popup::notice("Disable Default Relay?");
                    popup.text = Some("This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?".into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::DisableRelay { url }));
                } else if row.is_default {
                    self.net_change(
                        backend::run(async move {
                            backend::network::toggle_default_relay(url, true)
                                .await
                                .map(|_| ())
                        }),
                        true,
                        cx,
                    );
                } else {
                    let mut popup = kit::Popup::notice("Remove Relay?");
                    popup.text = Some(
                        format!(
                            "Are you sure you want to remove <b>{}</b>?",
                            crate::settings::host(&url)
                        )
                        .into(),
                    );
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::RemoveRelay { url }));
                }
            }
            (Act::ChartHover(i), NetDialog::Server { chart, .. }) => chart.hover = i,
            (Act::Alias(a), _) => return self.alias_act(a, _window, cx),
            (Act::ServerAction, NetDialog::Server { info, .. }) => {
                let url = info.url.clone();
                if info.is_custom {
                    let mut popup = kit::Popup::notice("Remove media server?");
                    popup.text = Some(format!("<b>{url}</b> will be removed from your list. Existing uploads on that server remain accessible.").into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::RemoveServer { url }));
                } else {
                    let enabled = !info.enabled;
                    info.enabled = enabled;
                    self.net_change(
                        backend::run(
                            async move { backend::network::toggle_server(url, enabled).await },
                        ),
                        false,
                        cx,
                    );
                }
            }
            _ => {}
        }
    }

    /// Apply a relay or server change, then reread the lists; `close` ends the dialog on success.
    fn net_change(
        &mut self,
        rx: futures::channel::oneshot::Receiver<Result<(), String>>,
        close: bool,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) if close => {
                        if let Some(s) = this.settings.as_mut() {
                            s.dialog = None;
                        }
                    }
                    Ok(()) => {}
                    Err(e) => {
                        let mut popup = kit::Popup::notice("Error");
                        popup.text = Some(e.into());
                        this.popup = Some((popup, PopupAct::Notice));
                    }
                }
                this.load_network(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The relay dialog's status, ping and log, every second while it is open.
    fn refresh_relay_dialog(&mut self, cx: &mut Context<Self>) {
        use crate::netdialog::NetDialog;
        let Some(NetDialog::Relay { row, .. }) =
            self.settings.as_ref().and_then(|s| s.dialog.as_ref())
        else {
            return;
        };
        if !self.live {
            return;
        }
        let url = row.url.clone();
        let rx = backend::run(async move {
            vector_core::db::scoped(async move {
                let row = backend::network::relay_rows()
                    .await
                    .into_iter()
                    .find(|r| r.url.eq_ignore_ascii_case(&url));
                (
                    row,
                    backend::network::relay_metrics_of(&url),
                    backend::network::relay_logs_of(&url),
                )
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let Ok((fresh, m, l)) = rx.await else { return };
            let open = this
                .update(cx, |this, cx| {
                    let Some(Some(NetDialog::Relay {
                        row, metrics, logs, ..
                    })) = this.settings.as_mut().map(|s| s.dialog.as_mut())
                    else {
                        return false;
                    };
                    if let Some(f) = fresh {
                        row.status = f.status;
                        row.enabled = f.enabled;
                    }
                    *metrics = m;
                    *logs = l;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if open {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                this.update(cx, |this, cx| this.refresh_relay_dialog(cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Read the relay and server lists and the clock, and keep them fresh while Network shows.
    fn load_network(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            if let Some(s) = self.settings.as_mut() {
                let (relays, servers) = fixture_network();
                s.relays = Some(relays);
                s.servers = Some(servers);
                self.routes_settle(cx);
                let Some(s) = self.settings.as_mut() else { return };
                s.clock = Some(vector_core::clock::Status {
                    decentralised: true,
                    measured_offset_ms: 0,
                    offset_ms: 0,
                    answered: 0,
                    agreeing: 0,
                    checked_at: 0,
                });
            }
            return;
        }
        let rx = backend::run(async {
            vector_core::db::scoped(async {
                (
                    backend::network::relay_rows().await,
                    backend::network::server_rows(),
                    vector_core::clock::status(),
                )
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let Ok((relays, servers, clock)) = rx.await else {
                return;
            };
            let again = this
                .update(cx, |this, cx| {
                    let Some(s) = this.settings.as_mut() else {
                        return false;
                    };
                    s.relays = Some(relays);
                    s.servers = Some(servers);
                    s.clock = Some(clock);
                    let network = s.cat == "network";
                    this.routes_settle(cx);
                    cx.notify();
                    network
                })
                .unwrap_or(false);
            if again {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                this.update(cx, |this, cx| {
                    if this.settings.as_ref().is_some_and(|s| s.cat == "network") {
                        this.load_network(cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// The Voice page's models and switches. A selection that is gone or can't run here falls
    /// back as `loadWhisperModels` does.
    fn load_voice(&mut self, cx: &mut Context<Self>) {
        if !self.settings.as_ref().is_some_and(|s| s.offers.voice) {
            return;
        }
        fn read() -> (Vec<backend::transcribe::Model>, u64, String, bool, bool) {
            let (model, translate) = backend::transcribe::choice();
            (
                backend::transcribe::models(),
                backend::transcribe::memory_mb(),
                model,
                translate,
                backend::transcribe::auto_transcribe(),
            )
        }
        let rx = if self.live {
            backend::run(vector_core::db::scoped(async { read() }))
        } else {
            let (tx, rx) = futures::channel::oneshot::channel();
            let _ = tx.send(read());
            rx
        };
        cx.spawn(async move |this, cx| {
            let Ok((models, memory_mb, model, translate, transcribe)) = rx.await else { return };
            this.update(cx, |this, cx| {
                let Some(s) = this.settings.as_mut() else { return };
                let (download, error) = s.voice.take().map_or((None, String::new()), |v| (v.download, v.error));
                let mut v = crate::settings::VoiceView {
                    models,
                    memory_mb,
                    selected: "small",
                    auto_translate: translate,
                    auto_transcribe: transcribe,
                    download,
                    error,
                };
                let want = v.models.iter().find(|m| m.name == model).map(|m| m.name);
                v.selected = match v.models.iter().find(|m| Some(m.name) == want) {
                    Some(m) if m.downloaded && memory_mb >= m.ram as u64 => m.name,
                    _ => v.fallback(want.unwrap_or("")),
                };
                if want != Some(v.selected) && this.live {
                    backend::transcribe::set("whisper_model_name", v.selected);
                }
                s.voice = Some(v);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Refresh the storage chart; a fresh report drops the chart's selection.
    fn load_audio(&mut self, cx: &mut Context<Self>) {
        use crate::settings::AudioView;
        if !self.live {
            if let Some(s) = self.settings.as_mut() {
                let named = |v: &[&str]| v.iter().map(|n| n.to_string()).collect::<Vec<_>>();
                s.audio = Some(AudioView {
                    settings: vector_core::calls::settings::AudioSettings {
                        noise_suppress: false,
                        mic_volume: 0.8,
                        ..Default::default()
                    },
                    devices: backend::mic::DeviceList {
                        inputs: named(&["Built-in Microphone", "USB Headset"]),
                        outputs: named(&["Built-in Speakers", "USB Headset"]),
                        default_input: "Built-in Microphone".into(),
                        default_output: "Built-in Speakers".into(),
                        prefs: Default::default(),
                    },
                    direct: false,
                    testing: false,
                    level: 0.,
                    call: self.call.live_mic(),
                });
            }
            return;
        }
        let rx = backend::run(async {
            vector_core::db::scoped(async {
                let settings = backend::mic::audio_settings();
                let direct = backend::mic::direct();
                let devices = tokio::task::spawn_blocking(backend::mic::devices)
                    .await
                    .unwrap_or_default();
                (settings, devices, direct)
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let Ok((settings, devices, direct)) = rx.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some(s) = this.settings.as_mut() {
                    let testing = s.audio.as_ref().is_some_and(|a| a.testing);
                    s.audio = Some(AudioView {
                        settings,
                        devices,
                        direct,
                        testing,
                        level: 0.,
                        call: this.call.live_mic(),
                    });
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn reload_devices(&mut self, cx: &mut Context<Self>) {
        let rx = backend::run(async {
            vector_core::db::scoped(async {
                tokio::task::spawn_blocking(backend::mic::devices)
                    .await
                    .unwrap_or_default()
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let Ok(devices) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(a) = this.settings.as_mut().and_then(|s| s.audio.as_mut()) {
                    a.devices = devices;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn saved_pill_pos() -> Option<gpui::Point<gpui::Pixels>> {
        let v: Value = serde_json::from_str(&backend::device::get("call_pill_pos")?).ok()?;
        Some(gpui::point(
            px(v.get("x")?.as_f64()? as f32),
            px(v.get("y")?.as_f64()? as f32),
        ))
    }

    fn fixture_call(&mut self, state: &str, cx: &mut Context<Self>) {
        let alice = "npub190vqdjtlpcq27xslcveglfmr4ynfwg7gmw86cnun4acakxrdd6gqnp929a";
        let (phase, outgoing) = match state {
            "ringing" => ("ringing", false),
            "calling" => ("ringing", true),
            _ => ("active", true),
        };
        let video = state.starts_with("video");
        self.call.apply_state(&serde_json::json!({ "id": "fixture", "peer": alice, "outgoing": outgoing, "phase": phase, "active_ms": 83_000, "volume": 1.0,
            "video_mine": { "camera": video, "screen": false }, "video_peer": { "camera": video, "screen": video }, "peer_decodes": if video { vec!["h264"] } else { vec![] } }));
        if video && std::env::var("VECTOR_CALL_PICS").is_ok() {
            let load = |f: &str| {
                image::open(format!("reference/media/{f}"))
                    .ok()
                    .map(|i| i.to_rgba8())
            };
            self.video.show_fixture(
                [load("portrait.png"), load("cover.png")],
                [load("photo.png"), None],
                Vec::new(),
            );
        }
        if phase == "active" {
            for (i, rtt) in [
                62, 58, 64, 71, 66, 60, 59, 63, 120, 95, 70, 64, 61, 60, 58, 62, 66, 63, 61, 60,
            ]
            .iter()
            .enumerate()
            {
                let concealed = if i == 9 { 3 } else { 0 } + if i > 9 { 3 } else { 0 };
                self.call.apply_stats(&serde_json::json!({ "id": "fixture", "rtt_ms": rtt, "path": "relay", "bitrate_kbps": 64, "received": (i + 1) * 50, "concealed": concealed,
                    "video": { "camera": { "fps": 30, "kbps": 780, "width": 1280, "height": 720 }, "screen": { "fps": 0, "kbps": 0, "width": 0, "height": 0 } } }));
            }
            self.call
                .apply_level(&serde_json::json!({ "id": "fixture", "mic": 0.62, "peer": 0.7 }));
            self.call.expanded = state == "open" || state == "video-open";
            self.call.maximized = (state == "video-max").then_some(1);
        }
        self.sync_call_meter();
        cx.notify();
    }

    /// A call appeared or changed: the panel's switches are read once, and the clock ticks while it lasts.
    fn call_started(&mut self, cx: &mut Context<Self>) {
        if self.call.id.is_some() && self.call_audio.is_none() && self.live {
            self.call_audio = Some(backend::mic::audio_settings());
        }
        if self.call_ticking || self.call.id.is_none() {
            return;
        }
        self.call_ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let going = this
                    .update(cx, |this, cx| {
                        this.call.expire();
                        cx.notify();
                        if this.call.id.is_none() {
                            this.call_ticking = false;
                            this.call_audio = None;
                        }
                        this.call_ticking
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        })
        .detach();
    }

    /// Off Clearnet, a call connects only once the account agrees for this session.
    fn with_realtime_consent(&mut self, then: CallGo, cx: &mut Context<Self>) {
        if self.live
            && vector_core::transport::realtime::check()
                .is_err_and(|e| e == vector_core::transport::realtime::CONSENT_REQUIRED)
        {
            let label =
                backend::calls::off_clearnet_label().unwrap_or_else(|| "the network in use".into());
            let mut p = kit::Popup::notice(format!("Connect outside {label}?"));
            p.icon = None;
            p.text = Some("Calls and multiplayer Mini Apps connect directly.<br>The people you connect with and the iroh relay can see your IP address.<br>Allowed until you close Vector, switch accounts or change networks.".into());
            p.confirm = "Connect".into();
            p.cancel = Some("Cancel".into());
            self.popup = Some((p, PopupAct::Realtime(then)));
            cx.notify();
            return;
        }
        self.call_go(then, cx);
    }

    fn call_go(&mut self, then: CallGo, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let rx = match then {
            CallGo::Start(npub) => backend::run(backend::calls::start(npub)),
            CallGo::Accept => backend::run(backend::calls::accept()),
        };
        self.toast_err(rx, cx);
    }

    fn on_call(&mut self, act: crate::call::Act, window: &mut Window, cx: &mut Context<Self>) {
        use crate::call::{Act, Switch};
        match act {
            Act::Accept => self.with_realtime_consent(CallGo::Accept, cx),
            Act::Reject => self.call_cmd(backend::calls::reject(), cx),
            Act::Hangup => {
                if !self.live {
                    self.call.clear();
                }
                self.call_cmd(backend::calls::hangup(), cx);
            }
            Act::Mic if self.call.mic_off => self.mic_access(cx),
            Act::Mic => {
                let on = !self.call.muted;
                self.call.muted = on;
                self.call_cmd(backend::calls::set_muted(on), cx);
            }
            Act::Toggle => {
                self.call.expanded = !self.call.expanded;
                self.call.hover = None;
                if let Some(p) = self.call.pos {
                    let size = gpui::size(
                        px(if self.call.expanded {
                            300.
                        } else {
                            f32::from(self.call.pill_size().width)
                        }),
                        self.call.pill_size().height,
                    );
                    self.call.pos = Some(crate::call::clamp(
                        p,
                        size,
                        window.viewport_size(),
                        CHROME_H,
                    ));
                }
            }
            Act::Move(p) => {
                self.call.pos = Some(crate::call::clamp(
                    p,
                    self.call.pill_size(),
                    window.viewport_size(),
                    CHROME_H,
                ))
            }
            Act::Moved => {
                if let Some(p) = self.call.pos {
                    backend::device::set(
                        "call_pill_pos",
                        serde_json::json!({ "x": f32::from(p.x), "y": f32::from(p.y) }).to_string(),
                    );
                }
            }
            Act::Volume(v) => {
                if (v - self.call.volume).abs() > f32::EPSILON {
                    self.call.volume = v;
                    self.call_cmd(backend::calls::set_volume(v), cx);
                }
            }
            Act::Audio(which, on) => {
                let mut a = self.call_audio.unwrap_or_default();
                match which {
                    Switch::AutoGain => a.auto_gain = on,
                    Switch::EchoCancel => a.echo_cancel = on,
                    Switch::NoiseSuppress => a.noise_suppress = on,
                }
                self.call_audio = Some(a);
                if let Some(v) = self.settings.as_mut().and_then(|s| s.audio.as_mut()) {
                    v.settings = a;
                }
                if self.live {
                    let rx = backend::run(async move {
                        vector_core::db::scoped(async move { backend::mic::set_audio_settings(a) })
                            .await
                    });
                    self.toast_err(rx, cx);
                }
            }
            Act::Hover(i) => {
                if self.call.hover == i {
                    return;
                }
                self.call.hover = i;
            }
            Act::Video(k, on) => self.set_video(k, on, cx),
            Act::Maximize(m) => {
                self.call.maximized = m;
                self.call.max_controls = m.map(|_| std::time::Instant::now() + std::time::Duration::from_millis(2500));
            }
            Act::WakeControls => {
                let shown = self.call.max_controls.is_some_and(|t| t > std::time::Instant::now());
                self.call.max_controls = Some(std::time::Instant::now() + std::time::Duration::from_millis(2500));
                if shown {
                    return;
                }
            }
            Act::PrefOpen(o) => self.call.pref_open = o,
            Act::ChangeScreen => {
                if self.video.capturing(crate::callvideo::VideoKind::Screen) {
                    self.open_screen_pick(true, cx);
                }
            }
            Act::ShareAudio(on) => {
                self.call.share_audio_mine = on;
                self.call_cmd(backend::calls::set_share_audio(on), cx);
            }
            Act::Pref(k, fps, v) => {
                self.call.pref_open = None;
                let p = &mut self.call.prefs[k];
                if fps { p.1 = v } else { p.0 = v }
                let (rung, rate) = *p;
                let kind = [crate::callvideo::VideoKind::Camera, crate::callvideo::VideoKind::Screen][k];
                if self.live
                    && let Err(e) = backend::calls::set_video_prefs(kind, rung, rate)
                {
                    self.show_toast(&e, cx);
                }
            }
        }
        cx.notify();
    }

    /// One of our pictures on or off. Opening a camera can wait on a system prompt, so it
    /// runs off the UI thread; the call hears of it only once the camera is open.
    fn set_video(&mut self, k: usize, on: bool, cx: &mut Context<Self>) {
        use crate::callvideo::VideoKind;
        let kind = [VideoKind::Camera, VideoKind::Screen][k];
        if !on {
            let had = self.video.capturing(kind)
                || self.call.video_mine[k]
                || self
                    .voice
                    .session
                    .as_ref()
                    .is_some_and(|s| [s.camera, s.sharing][k]);
            self.video.stop(kind);
            self.call.video_mine[k] = false;
            if had && self.live {
                let rx = self.video_backend(k, false);
                self.toast_err(rx, cx);
            }
            return;
        }
        if self.video.capturing(kind) || self.video.starting[k] {
            return;
        }
        if kind == VideoKind::Screen {
            self.open_screen_pick(false, cx);
            return;
        }
        let Some(open) = self.video.camera_opener(kind) else {
            self.show_toast("Could not open the camera", cx);
            return;
        };
        self.video.starting[k] = true;
        let live = self.live;
        let voice = !self.call.live() && self.voice.session.is_some();
        cx.spawn(async move |this, cx| {
            let cap = cx.background_executor().spawn(async move { open() }).await;
            let told = match &cap {
                Ok(_) if live => {
                    let rx = if voice {
                        backend::run(backend::voice::set_video(kind, true))
                    } else {
                        backend::run(backend::calls::set_video(kind, true))
                    };
                    rx.await.unwrap_or_else(|_| Err("Not in a call".into()))
                }
                _ => Ok(()),
            };
            this.update(cx, |this, cx| {
                this.video.starting[k] = false;
                match (cap, told) {
                    (Ok(cap), Ok(())) => {
                        this.video.attach(kind, cap);
                        this.call.video_mine[k] = true;
                    }
                    (Err(e), _) | (_, Err(e)) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The screen picker; its sources are found off the UI thread, a grab apiece.
    fn open_screen_pick(&mut self, change: bool, cx: &mut Context<Self>) {
        if self.screen_pick.is_some() {
            return;
        }
        self.screen_pick = Some(crate::screenpick::Picker::new(change));
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async { crate::callvideo::screen::sources() }).await;
            this.update(cx, |this, cx| {
                if let Some(p) = this.screen_pick.as_mut() {
                    p.fill(found);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn close_screen_pick(&mut self) {
        if let Some(p) = self.screen_pick.take() {
            self.video.pics().retired.extend(p.images());
        }
    }

    fn on_screen_pick(&mut self, act: crate::screenpick::Act, cx: &mut Context<Self>) {
        use crate::callvideo::VideoKind;
        use crate::screenpick::Act;
        let Some(p) = self.screen_pick.as_mut() else { return };
        match act {
            Act::Select(i) => p.selected = Some(i),
            Act::Cancel => self.close_screen_pick(),
            Act::Share => {
                let Some(target) = p.chosen() else { return };
                let change = p.change;
                self.close_screen_pick();
                let Some(open) = self.video.screen_opener(target) else {
                    self.show_toast("Could not share the screen", cx);
                    return;
                };
                if !change {
                    self.video.starting[1] = true;
                }
                let told = (self.live && !change).then(|| self.video_backend(1, true));
                cx.spawn(async move |this, cx| {
                    let cap = cx.background_executor().spawn(async move { open() }).await;
                    let told = match (&cap, told) {
                        (Ok(_), Some(rx)) => {
                            rx.await.unwrap_or_else(|_| Err("Not in a call".into()))
                        }
                        _ => Ok(()),
                    };
                    this.update(cx, |this, cx| {
                        this.video.starting[1] = false;
                        match (cap, told) {
                            (Ok(cap), Ok(())) if change => this.video.replace(VideoKind::Screen, cap),
                            (Ok(cap), Ok(())) => {
                                this.video.attach(VideoKind::Screen, cap);
                                this.call.video_mine[1] = true;
                            }
                            (Err(e), _) | (_, Err(e)) => this.show_toast(&e, cx),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn call_cmd(
        &self,
        fut: impl std::future::Future<Output = Result<(), String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.live {
            let rx = backend::run(fut);
            self.toast_err(rx, cx);
        }
    }

    /// A drag moves the slider every frame; the store hears it a few times a second.
    fn save_audio(&mut self, now: bool, cx: &mut Context<Self>) {
        if !self.live || (!now && self.audio_save_pending) {
            return;
        }
        self.audio_save_pending = true;
        cx.spawn(async move |this, cx| {
            if !now {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(80))
                    .await;
            }
            this.update(cx, |this, cx| {
                this.audio_save_pending = false;
                let Some(settings) = this
                    .settings
                    .as_ref()
                    .and_then(|s| s.audio.as_ref())
                    .map(|a| a.settings)
                else {
                    return;
                };
                let rx = backend::run(async move {
                    vector_core::db::scoped(
                        async move { backend::mic::set_audio_settings(settings) },
                    )
                    .await
                });
                this.toast_err(rx, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Reports a failed backend step as a toast.
    fn toast_err(
        &self,
        rx: futures::channel::oneshot::Receiver<Result<(), String>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = rx.await {
                this.update(cx, |this, cx| {
                    if e == vector_media::mic_access::DENIED {
                        this.mic_access(cx)
                    } else {
                        this.show_toast(&e, cx)
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// The test runs while Calls is on screen: leaving it stops the microphone.
    fn mic_test(&mut self, cx: &mut Context<Self>) {
        let Some(a) = self.settings.as_mut().and_then(|s| s.audio.as_mut()) else {
            return;
        };
        if a.testing {
            a.testing = false;
            a.level = 0.;
            backend::mic::test_stop();
            return;
        }
        if !self.live {
            return;
        }
        let rx = backend::run(backend::mic::test_start());
        cx.spawn(async move |this, cx| {
            let started = rx
                .await
                .unwrap_or_else(|_| Err("The microphone test stopped".into()));
            let ok = this
                .update(cx, |this, cx| match started {
                    Ok(()) => {
                        if let Some(a) = this.settings.as_mut().and_then(|s| s.audio.as_mut()) {
                            a.testing = true;
                        }
                        this.reload_devices(cx);
                        cx.notify();
                        true
                    }
                    Err(e) => {
                        if e == vector_media::mic_access::DENIED {
                            this.mic_access(cx);
                        } else {
                            this.show_toast(&e, cx);
                        }
                        false
                    }
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let going = this
                    .update(cx, |this, _| {
                        this.settings
                            .as_ref()
                            .filter(|s| s.cat == "audio")
                            .and_then(|s| s.audio.as_ref())
                            .is_some_and(|a| a.testing)
                    })
                    .unwrap_or(false);
                if !going {
                    backend::mic::test_stop();
                    break;
                }
            }
        })
        .detach();
    }

    fn load_storage(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            if let Some(s) = self.settings.as_mut() {
                // Fixtures: `VECTOR_STORAGE=ext:bytes,...` stands in for a disk report.
                let dist = std::env::var("VECTOR_STORAGE").unwrap_or_default();
                s.storage = Some(
                    dist.split(',')
                        .filter_map(|kv| kv.split_once(':'))
                        .filter_map(|(k, v)| Some((k.to_string(), v.parse().ok()?)))
                        .collect(),
                );
            }
            return;
        }
        let rx = backend::run(async {
            vector_core::db::scoped(async { backend::storage::distribution() }).await
        });
        cx.spawn(async move |this, cx| {
            let Ok(dist) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(s) = this.settings.as_mut() {
                    s.storage = Some(dist);
                    s.donut.hover = None;
                    s.donut.picked = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn confirm_storage_delete(&mut self) {
        let Some(s) = self.settings.as_ref() else {
            return;
        };
        let Some(dist) = s.storage.as_ref() else {
            return;
        };
        let segs = crate::settings::segments(dist);
        let Some(seg) = s.donut.picked.and_then(|i| segs.get(i)) else {
            return;
        };
        let cat = &crate::settings::STORE_CATS[seg.cat];
        let size = crate::media::format_bytes(seg.size);
        let text = match cat.key {
            Some("/ai_models") => format!(
                "This will delete {size} of downloaded AI models from this device.<br><br>Voice transcription will download a model again when needed."
            ),
            Some(_) => format!(
                "This will delete {size} of cached avatars, banners and images.<br><br>Vector rebuilds this cache automatically over time."
            ),
            None => format!(
                "This will delete {size} of {} from this device.<br><br>You can download them again from their chats later, if they are still available.",
                cat.noun
            ),
        };
        let category = match cat.key {
            Some("/ai_models") => "ai",
            Some(_) => "cache",
            None => "files",
        };
        let exts = crate::settings::slice_exts(seg.cat, dist);
        let mut popup = kit::Popup::notice(cat.title);
        popup.icon = Some("icons/vector_warning.svg");
        popup.text = Some(text.into());
        popup.confirm = "Confirm".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::DeleteSlice { category, exts }));
    }

    /// Cached embed videos the clear took: the next sight fetches them again.
    fn forget_sealed_embeds(&mut self) {
        use crate::audio::AudioInputSpec::Sealed;
        use crate::embeds::VideoState;
        let mut media = self.media.borrow_mut();
        let media = &mut *media;
        let gone: Vec<String> = media
            .embed_videos
            .iter()
            .filter(|(_, s)| matches!(s, VideoState::Ready(Sealed(p, _)) if !p.exists()))
            .map(|(u, _)| u.clone())
            .collect();
        for url in gone {
            media.embed_videos.remove(&url);
            media.videos.forget(&crate::embeds::clip_id(&url));
        }
        let gone: Vec<String> = media
            .embed_previews
            .iter()
            .filter(|(_, s)| matches!(s, Some(Sealed(p, _)) if !p.exists()))
            .map(|(u, _)| u.clone())
            .collect();
        for url in gone {
            media.embed_previews.remove(&url);
            media.videos.forget(&crate::embeds::preview_id(&url));
        }
    }

    fn after_storage_clear(
        &mut self,
        rx: futures::channel::oneshot::Receiver<Result<u64, String>>,
        slice: bool,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                if let Some(s) = this.settings.as_mut() {
                    s.clearing = false;
                    s.donut.busy = false;
                }
                if result.is_ok() {
                    this.forget_sealed_embeds();
                }
                match result {
                    Ok(freed) if slice => this.show_toast(
                        &format!("Freed {}", vector_core::crypto::format_bytes(freed)),
                        cx,
                    ),
                    Ok(_) => {}
                    Err(e) => {
                        let mut popup = kit::Popup::notice(if slice {
                            "Delete Failed"
                        } else {
                            "Clear Failed"
                        });
                        popup.icon = Some("icons/vector_warning.svg");
                        popup.text = Some(
                            if slice {
                                format!("Could not delete: {e}")
                            } else {
                                format!("Could not clear storage: {e}")
                            }
                            .into(),
                        );
                        this.popup = Some((popup, PopupAct::Notice));
                    }
                }
                this.load_storage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keep Files on or off; the switch goes back if the change doesn't happen.
    fn set_keep_media(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(s) = self.settings.as_mut() {
            s.on.insert("keep_media", on);
            s.flipped.insert("keep_media");
        }
        if !self.live {
            return;
        }
        let rx = backend::run(backend::storage::set_keep_media(on));
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    if let Some(s) = this.settings.as_mut() {
                        s.on.insert("keep_media", !on);
                    }
                    let mut popup = kit::Popup::notice("Keep Files");
                    popup.icon = Some("icons/vector_warning.svg");
                    popup.text = Some(e.into());
                    this.popup = Some((popup, PopupAct::Notice));
                } else if !on {
                    if let Some(s) = this.settings.as_mut() {
                        s.on.insert("keep_unencrypted", false);
                    }
                    this.load_storage(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Turning Keep Files off asks first when it has kept anything.
    fn toggle_keep_media(&mut self, on: bool, cx: &mut Context<Self>) {
        if on || !self.live {
            return self.set_keep_media(on, cx);
        }
        let rx =
            backend::run(async { vector_core::db::scoped(backend::storage::kept_summary()).await });
        cx.spawn(async move |this, cx| {
            let (files, bytes, busy) = rx.await.unwrap_or((0, 0, false));
            this.update(cx, |this, cx| {
                if busy {
                    let mut popup = kit::Popup::notice("Downloads in progress");
                    popup.icon = Some("icons/vector_warning.svg");
                    popup.text = Some("Turn this off once your downloads finish.".into());
                    this.popup = Some((popup, PopupAct::Notice));
                    return cx.notify();
                }
                if files == 0 {
                    return this.set_keep_media(false, cx);
                }
                let mut popup = kit::Popup::notice("Stop keeping files?");
                popup.icon = Some("icons/vector_warning.svg");
                popup.text = Some(
                    format!("{files} kept {} ({}) will be deleted from this device. They load again when you open them.", if files == 1 { "file" } else { "files" }, crate::media::format_bytes(bytes)).into(),
                );
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                this.popup = Some((popup, PopupAct::StopKeeping));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Turning Keep Unencrypted on asks first: other apps can then read what Vector keeps.
    fn toggle_keep_unencrypted(&mut self, on: bool, cx: &mut Context<Self>) {
        if !on || !self.live {
            return self.set_keep_unencrypted(on, cx);
        }
        let mut popup = kit::Popup::notice("Keep media unencrypted?");
        popup.icon = Some("icons/vector_warning.svg");
        popup.text = Some(
            format!("Everything Vector keeps becomes a normal file in {}. Any app or person with access to this device's files can open them.", backend::mediaport::public_folder()).into(),
        );
        popup.confirm = "Confirm".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::KeepPlain));
        cx.notify();
    }

    /// Keep Unencrypted on or off; the switch goes back if the change doesn't happen.
    fn set_keep_unencrypted(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(s) = self.settings.as_mut() {
            s.on.insert("keep_unencrypted", on);
            s.flipped.insert("keep_unencrypted");
        }
        if !self.live {
            return cx.notify();
        }
        let rx = backend::run(backend::mediaport::set_keep_unencrypted(on));
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    if let Some(s) = this.settings.as_mut() {
                        s.on.insert("keep_unencrypted", !on);
                    }
                    let mut popup = kit::Popup::notice("Keep Unencrypted");
                    popup.icon = Some("icons/vector_warning.svg");
                    popup.text = Some(e.into());
                    this.popup = Some((popup, PopupAct::Notice));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Pick, check and store a custom notification sound.
    fn pick_sound(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let dir = backend::default_data_dir().join("cache").join("sounds");
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let imported = cx.background_executor().spawn(async move { backend::sound::import(&path, &dir) }).await;
            this.update(cx, |this, cx| {
                cx.notify();
                let stored = match imported {
                    Ok(p) => p,
                    Err(e) => {
                        let (title, text) = match e.as_str() {
                            "FILE_TOO_LARGE" => ("File Too Large", "Notification sounds must be under 1MB. Please choose a shorter audio clip."),
                            "AUDIO_TOO_LONG" => ("Audio Too Long", "Notification sounds must be 10 seconds or less."),
                            _ => return log::warn!("custom sound: {e}"),
                        };
                        let mut popup = kit::Popup::notice(title);
                        popup.text = Some(text.into());
                        this.popup = Some((popup, PopupAct::Notice));
                        return;
                    }
                };
                let Some(s) = this.settings.as_mut() else { return };
                s.sound_custom = false;
                s.notif_sound = format!("custom:{}", stored.display());
                backend::sound::purge();
                if this.live {
                    backend::prefs::set_sql("notif_sound", s.notif_sound.clone());
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_blocked(&mut self, cx: &mut Context<Self>) {
        let fill = |this: &mut Self, list: Vec<vector_core::SlimProfile>| {
            let store = this.store.borrow();
            let rows = list
                .into_iter()
                .map(|p| {
                    let name = [&p.nickname, &p.name, &p.display_name]
                        .into_iter()
                        .find(|n| !n.is_empty())
                        .cloned()
                        .unwrap_or_default();
                    crate::settings::Blocked {
                        avatar: store.avatar_path(&p.id),
                        npub: p.id,
                        name,
                        veiled: false,
                    }
                })
                .collect();
            drop(store);
            if let Some(s) = this.settings.as_mut() {
                s.blocked = Some(rows);
            }
        };
        if !self.live {
            let list = self
                .store
                .borrow()
                .profiles
                .values()
                .filter(|p| p.is_blocked)
                .cloned()
                .collect();
            fill(self, list);
            return;
        }
        let rx = backend::run(vector_core::profile::sync::get_blocked_users());
        cx.spawn(async move |this, cx| {
            let Ok(list) = rx.await else { return };
            this.update(cx, |this, cx| {
                fill(this, list);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The block now being read stops wearing its NEW pill.
    fn mark_settings_seen(&mut self, anchor: Option<&'static str>) {
        let Some(s) = self.settings.as_mut() else {
            return;
        };
        let cats = crate::settings::categories(&s.offers);
        let Some(cat) = cats.iter().find(|c| c.id == s.cat) else {
            return;
        };
        let Some(id) = anchor.or(cat.anchors.first().map(|a| a.id)) else {
            return;
        };
        if !s.fresh_ids(&cats).contains(&id) {
            return;
        }
        s.seen.insert(id.to_string());
        let list: Vec<&String> = s.seen.iter().collect();
        if let Ok(json) = serde_json::to_string(&list) {
            backend::device::set("settings_seen", json);
        }
    }

    fn on_settings(
        &mut self,
        act: crate::settings::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let into_network = matches!(
            act,
            crate::settings::Act::Category("network") | crate::settings::Act::Anchor("network", _)
        ) && self.settings.as_ref().is_some_and(|s| s.cat != "network");
        self.settings_act(act, window, cx);
        if into_network {
            self.load_network(cx);
        }
    }

    fn settings_act(
        &mut self,
        act: crate::settings::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::settings::{Act, Home};
        let Some(s) = self.settings.as_mut() else {
            return;
        };
        cx.notify();
        let mut pack_reload = false;
        match act {
            Act::CopyPlace(cat, anchor) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!(
                    "https://vectorapp.io/go#settings/{cat}/{anchor}"
                )));
                self.show_toast("Copied Settings Link", cx);
            }
            Act::Fold(id) => {
                s.open_select = None;
                if s.search.read(cx).text().trim().is_empty() {
                    let prev = s.slim_open;
                    s.slim_open = (s.slim_open != Some(id)).then_some(id);
                    let mut g = s.slim_geo.borrow_mut();
                    g.glide = None;
                    // The tapped row stays put while a category above it folds away.
                    if let Some(p) = prev.filter(|p| *p != id)
                        && let (Some(above), Some(here)) = (g.rows.get(p), g.rows.get(id))
                        && above.top() < here.top()
                    {
                        let h = g.bodies.get(p).copied().unwrap_or(0.);
                        let o = s.slim_scroll.offset();
                        s.slim_scroll.set_offset(gpui::point(o.x, (o.y + px(h)).min(px(0.))));
                    }
                    g.follow = s.slim_open;
                    drop(g);
                    if s.slim_open.is_some() {
                        s.cat = id;
                        self.mark_settings_seen(None);
                    }
                } else if let Some(i) = s.slim_shut.iter().position(|x| *x == id) {
                    s.slim_shut.remove(i);
                } else {
                    s.slim_shut.push(id);
                }
            }
            Act::Category(id) => {
                s.open_select = None;
                s.jumped = None;
                s.slim_open = Some(id);
                if s.cat != id {
                    s.cat = id;
                    s.scroll = gpui::ScrollHandle::new();
                    self.mark_settings_seen(None);
                }
            }
            Act::Anchor(cat, anchor) => {
                s.open_select = None;
                s.slim_open = Some(cat);
                if s.cat != cat {
                    s.cat = cat;
                    s.scroll = gpui::ScrollHandle::new();
                }
                let cats = crate::settings::categories(&s.offers);
                if let Some(ix) = cats.iter().find(|c| c.id == cat).and_then(|c| c.anchors.iter().position(|a| a.id == anchor)) {
                    s.scroll.scroll_to_top_of_item(ix);
                }
                s.jumped = Some(anchor);
                self.mark_settings_seen(Some(anchor));
            }
            Act::Scrolled => s.jumped = None,
            Act::Toggle("keep_media", value) => self.toggle_keep_media(value, cx),
            Act::Toggle("keep_unencrypted", value) => self.toggle_keep_unencrypted(value, cx),
            Act::Toggle("encryption", value) => {
                s.on.insert("encryption", value);
                s.flipped.insert("encryption");
                self.security.update(cx, |f, cx| f.toggle(value, cx));
            }
            Act::Toggle("clock_decentralised", value) => {
                s.on.insert("clock_decentralised", value);
                s.flipped.insert("clock_decentralised");
                if self.live {
                    let rx = backend::run(async move { vector_core::db::scoped(async move { vector_core::clock::set_decentralised(value).map(|_| vector_core::clock::status()) }).await });
                    cx.spawn(async move |this, cx| {
                        let Ok(Ok(status)) = rx.await else { return };
                        this.update(cx, |this, cx| {
                            if let Some(s) = this.settings.as_mut() {
                                s.clock = Some(status);
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            Act::Toggle(key @ ("voiceTranslate" | "voiceTranscribe"), value) => {
                let Some(v) = s.voice.as_mut() else { return };
                s.flipped.insert(key);
                let sql = if key == "voiceTranslate" {
                    v.auto_translate = value;
                    "whisper_auto_translate"
                } else {
                    v.auto_transcribe = value;
                    "whisper_auto_transcribe"
                };
                if self.live {
                    backend::transcribe::set(sql, value);
                }
                self.media.borrow_mut().audio.auto_transcribe = None;
            }
            Act::Toggle(key, value) => {
                let Some(t) = crate::settings::toggle_of(key) else { return };
                if key == "auto_download_enabled" {
                    self.media.borrow_mut().auto_download.on = value;
                }
                if key == "web_previews" {
                    self.media.borrow_mut().web_previews_off = !value;
                }
                if key == "emoticon_suggestions" {
                    self.emoticons = value;
                }
                if key == "time_suggestions" {
                    self.time_suggestions = value;
                }
                if key == "chat_bg_enabled" {
                    self.chat_bg = value;
                }
                if key == "cc_rail_hidden" {
                    self.slim.hidden = value;
                }
                if key == "floating_player" {
                    self.popout.enabled = value;
                    if !value && let Some(i) = self.popout.close() {
                        let mut media = self.media.borrow_mut();
                        match i.kind {
                            crate::popout::Kind::Audio => media.audio.pause(&i.att),
                            crate::popout::Kind::Video => media.videos.stop(&i.att),
                        }
                    }
                }
                if !self.live && key == "streamer_on" {
                    self.store.borrow_mut().streamer.on = value;
                }
                if key == "notif_global_mute" && value {
                    backend::sound::purge();
                }
                s.on.insert(key, value);
                s.flipped.insert(key);
                match t.home {
                    Home::Device(yes, no, _) => backend::device::set(key, if value { yes } else { no }),
                    Home::Sql(_) if self.live => backend::prefs::set_sql(key, value.to_string()),
                    Home::Synced if self.live => {
                        use vector_core::synced_prefs as sp;
                        let rx = backend::run(async move {
                            backend::prefs::edit_synced(|| match key {
                                "advanced" => sp::set_advanced(value),
                                "players" => sp::set_players(value),
                                "streamer_on" => sp::set_streamer_on(value),
                                _ => sp::set_streamer_wallpapers(value),
                            })
                        });
                        self.settle_toggle(key, rx, cx);
                    }
                    Home::Profile if self.live => {
                        let rx = backend::run(async move {
                            vector_core::db::scoped_result(vector_core::profile::sync::set_stream_consent(value, &backend::sync::ProfileHandler)).await
                        });
                        cx.spawn(async move |this, cx| {
                            let stuck = matches!(rx.await, Ok(Ok(true))) == value;
                            this.update(cx, |this, cx| {
                                if let Some(s) = this.settings.as_mut() {
                                    s.on.insert(key, if stuck { value } else { !value });
                                }
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                    }
                    _ => {}
                }
            }
            Act::Net(a) => self.net_act(a, window, cx),
            Act::Help(key) => {
                if let Some((title, text)) = crate::settings::help(key) {
                    let mut popup = kit::Popup::notice(title);
                    popup.icon = None;
                    popup.text = Some(text.into());
                    self.popup = Some((popup, PopupAct::Notice));
                }
            }
            Act::Select(id) => s.open_select = if s.open_select == Some(id) { None } else { Some(id) },
            Act::Pick(id, value) => {
                s.open_select = None;
                match id {
                    "whisper-model" => {
                        if let Some(v) = s.voice.as_mut()
                            && let Some(m) = v.models.iter().find(|m| m.name == value)
                        {
                            v.selected = m.name;
                            v.error.clear();
                            if self.live {
                                backend::transcribe::set("whisper_model_name", m.name);
                            }
                            self.media.borrow_mut().audio.auto_transcribe = None;
                        }
                    }
                    "theme" => {
                        s.theme = value.to_string();
                        if self.live {
                            backend::prefs::set_sql("theme", value.to_string());
                            pack_reload = true;
                        }
                    }
                    "notif-privacy" => {
                        s.notif_privacy = value.to_string();
                        if self.live {
                            backend::prefs::set_sql("notif_content_privacy", value.to_string());
                        }
                    }
                    "auto-limit" => {
                        let bytes: u64 = value.parse().unwrap_or(10_485_760);
                        s.auto_limit = bytes;
                        self.media.borrow_mut().auto_download.limit = bytes;
                        if self.live {
                            backend::prefs::set_sql("max_auto_download_bytes", value.to_string());
                        }
                    }
                    "notif-sound" if value == "custom" => s.sound_custom = true,
                    "notif-sound" => {
                        s.sound_custom = false;
                        s.notif_sound = value.to_string();
                        backend::sound::purge();
                        if self.live {
                            backend::prefs::set_sql("notif_sound", value.to_string());
                        }
                    }
                    "streamer-notif" => {
                        s.streamer_notif = value.to_string();
                        if self.live {
                            let _ = backend::run(async move { backend::prefs::edit_synced(|| vector_core::synced_prefs::set_streamer_notif(value)) });
                        }
                    }
                    _ => {}
                }
            }
            Act::Unblock(npub) => {
                let name = self.store.borrow().name(&npub);
                let mut popup = kit::Popup::notice("Unblock User");
                popup.text = Some(format!("Are you sure you want to unblock {name}?").into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::Unblock { npub }));
            }
            Act::CopyLogs => {
                let log = backend::default_data_dir().join("vector.log");
                match std::fs::read_to_string(log).ok().filter(|l| !l.trim().is_empty()) {
                    Some(text) => {
                        let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        self.show_toast(&format!("Copied {lines} log entries to clipboard"), cx);
                    }
                    None => self.show_toast("No logs to copy!", cx),
                }
            }
            Act::AddServer => {
                let input = cx.new(|cx| TextInput::new("blossom.primal.net", cx).with_look(kit::field_look()).keep_on_submit());
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| app.answer_popup(true, window, cx)).detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.answer_popup(false, window, cx)).detach();
                window.focus(&input.focus_handle(cx), cx);
                let mut popup = kit::Popup::notice("Add Media Server");
                popup.icon = None;
                let i2p = self
                    .net_view
                    .as_ref()
                    .and_then(|v| v.get("supported")?.as_array().map(|a| a.iter().any(|k| k == "i2p")))
                    .unwrap_or(false);
                popup.text = Some(
                    format!(
                        "Enter the address of a Blossom-compatible server. A bare domain like <b>blossom.primal.net</b> works. Vector adds <b>https://</b> automatically.{}",
                        if i2p { "<br><br>An <b>.i2p</b> server gets <b>http://</b>. Only people using I2P can load files from it." } else { "" }
                    )
                    .into(),
                );
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                popup.input = Some(input);
                self.popup = Some((popup, PopupAct::AddServer));
            }
            Act::AddRelay => {
                let input = cx.new(|cx| TextInput::new("relay.example.com", cx).with_look(kit::field_look()).keep_on_submit());
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| app.on_net(crate::netdialog::Act::Add, window, cx)).detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.on_net(crate::netdialog::Act::Close, window, cx)).detach();
                cx.observe(&input, |_, _, cx| cx.notify()).detach();
                window.focus(&input.focus_handle(cx), cx);
                s.dialog = Some(crate::netdialog::NetDialog::AddRelay { input, mode: "both" });
            }
            Act::OpenRelay(url) => {
                let Some(row) = s.relays.iter().flatten().find(|r| r.url == url).cloned() else { return };
                let alias = crate::routes::Alias::new(&s.book, &row.url, true, cx);
                let url = row.url.clone();
                s.dialog = Some(crate::netdialog::NetDialog::Relay { row, metrics: Default::default(), logs: Vec::new(), mode_open: false, copied: false, alias, circuit: None });
                self.dialog_route(url, cx);
                self.refresh_relay_dialog(cx);
            }
            Act::OpenServer(url) => {
                let Some(info) = s.servers.iter().flatten().find(|r| r.url == url).cloned() else { return };
                let (enabled, url) = (info.enabled, info.url.clone());
                s.dialog = Some(crate::netdialog::NetDialog::Server {
                    info,
                    stats: None,
                    doc: crate::serverinfo::Load::Loading,
                    caps: crate::serverinfo::Load::Loading,
                    chart: Default::default(),
                    alias: crate::routes::Alias::new(&s.book, &url, false, cx),
                    circuit: None,
                });
                self.dialog_route(url.clone(), cx);
                self.load_server_dialog(url, enabled, cx);
            }
            Act::DonutHover(at) => s.donut.hover = at,
            Act::DonutPick(i) => s.donut.picked = if s.donut.picked == Some(i) { None } else { Some(i) },
            Act::DonutDelete => self.confirm_storage_delete(),
            Act::VoiceDownload => {
                let Some(v) = s.voice.as_mut() else { return };
                if v.download.is_some() || v.current().is_none_or(|m| m.downloaded) {
                    return;
                }
                v.download = Some("0%".into());
                v.error.clear();
                let rx = backend::run(backend::transcribe::download(v.selected.to_string()));
                cx.spawn(async move |this, cx| {
                    let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update(cx, |this, cx| {
                        if let Some(v) = this.settings.as_mut().and_then(|s| s.voice.as_mut()) {
                            v.download = None;
                            if let Err(e) = &res
                                && !e.contains("cancelled")
                            {
                                v.error = format!("Download failed: {e}");
                            }
                        }
                        this.media.borrow_mut().audio.auto_transcribe = None;
                        this.load_voice(cx);
                    })
                    .ok();
                })
                .detach();
            }
            Act::VoiceCancel => backend::transcribe::cancel_download(),
            Act::VoiceDelete => {
                let Some(m) = s.voice.as_ref().and_then(|v| v.current()) else { return };
                let mut popup = kit::Popup::notice("Delete Model?");
                popup.text = Some(format!("Are you sure you want to delete the \"{}\" model? This will free up disk space but you'll need to download it again to use it.", m.name).into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::DeleteModel(m.name)));
            }
            Act::ClearStorage => {
                if !s.clearing {
                    let mut popup = kit::Popup::notice("Clear Storage?");
                    popup.icon = Some("icons/vector_warning.svg".into());
                    popup.text = Some("Deletes this account's files, plus the cache. This can't be undone.".into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::ClearStorage));
                }
            }
            Act::PreviewSound => backend::sound::play(backend::sound::Sound::parse(&s.notif_sound)),
            Act::PickSound => self.pick_sound(cx),
            Act::ChangeCredential => self.security.update(cx, |f, cx| f.change(cx)),
            Act::ShowKeys => self.security.update(cx, |f, cx| f.show_keys(cx)),
            Act::Reauthorize => self.open_reauth(window, cx),
            Act::Transfer => self.security.update(cx, |f, cx| f.open_transfer(true, cx)),
            Act::SecAdvanced => s.sec_advanced = !s.sec_advanced,
            Act::CheckUpdates => {
                if self.updates.phase == crate::settings::UpdPhase::Available {
                    if let Some(url) = self.update_url.clone() {
                        self.open_web(&url, cx);
                    }
                } else if self.live {
                    self.check_updates(false, cx);
                }
            }
            Act::Volume(key, v) => {
                if let Some(a) = s.audio.as_mut() {
                    if key == "mic" {
                        a.settings.mic_volume = v;
                    } else {
                        a.settings.speaker_volume = v;
                    }
                    self.save_audio(false, cx);
                }
            }
            Act::AudioSwitch(key, v) => {
                s.flipped.insert(key);
                if let Some(a) = s.audio.as_mut() {
                    match key {
                        "autoGain" => a.settings.auto_gain = v,
                        "echoCancel" => a.settings.echo_cancel = v,
                        _ => a.settings.noise_suppress = v,
                    }
                    self.save_audio(true, cx);
                }
            }
            Act::Direct(on) => {
                s.flipped.insert("direct");
                if let Some(a) = s.audio.as_mut() {
                    a.direct = on;
                }
                if self.live {
                    let rx = backend::run(async move { vector_core::db::scoped(backend::mic::set_direct(on)).await });
                    self.toast_err(rx, cx);
                }
            }
            Act::MicTest => self.mic_test(cx),
            Act::Device(kind, name) => {
                s.open_select = None;
                let Some(a) = s.audio.as_mut() else { return };
                let mut prefs = a.devices.prefs.clone();
                if kind == "input" {
                    prefs.input = name;
                } else {
                    prefs.output = name;
                }
                a.devices.prefs = prefs.clone();
                if self.live {
                    let rx = backend::run(async move { vector_core::db::scoped(async move { tokio::task::spawn_blocking(move || backend::mic::set_devices(prefs)).await.map_err(|e| e.to_string())? }).await });
                    cx.spawn(async move |this, cx| {
                        let result = rx.await.unwrap_or(Ok(()));
                        this.update(cx, |this, cx| {
                            if let Err(e) = result {
                                this.show_toast(&e, cx);
                            }
                            this.reload_devices(cx);
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            Act::Beta(on) => {
                s.flipped.insert("beta");
                backend::updates::set_beta(on);
                self.update_url = None;
                self.set_updates(|u| {
                    *u = crate::settings::Updates { beta: on, ..Default::default() };
                });
                if self.live {
                    self.check_updates(false, cx);
                }
            }
            Act::ClearSound => {
                s.sound_custom = false;
                s.notif_sound = "default".into();
                backend::sound::purge();
                if self.live {
                    backend::prefs::set_sql("notif_sound", "default".into());
                }
            }
            Act::Logout => {
                let mut popup = kit::Popup::notice("Going Incognito?");
                popup.text = Some("Logging out of Vector will fully erase the database, <b>ensure you have a backup of your keys before logging out!</b><br><br>That said, would you like to continue?".into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::Logout));
            }
            Act::Link(key) => self.open_web(match key {
                "donate" => "https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/more/donations",
                "gitbook" => "https://docs.vectorapp.io",
                _ => "https://vectorapp.io/privacy-policy",
            }, cx),
            Act::ClearSearch => {
                s.slim_shut.clear();
                let search = s.search.clone();
                search.update(cx, |input, cx| input.clear(window, cx));
                window.focus(&search.focus_handle(cx), cx);
            }
        }
        if pack_reload {
            self.reload_picker_packs(cx);
        }
    }

    /// A synced toggle shows what stuck once the save answers.
    fn settle_toggle(
        &mut self,
        key: &'static str,
        rx: futures::channel::oneshot::Receiver<
            Result<vector_core::synced_prefs::SyncedSettings, String>,
        >,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let Ok(Ok(view)) = rx.await else { return };
            this.update(cx, |this, cx| {
                this.store.borrow_mut().streamer.set(&view.streamer);
                this.store.borrow_mut().players_off = view.players_off;
                if let Some(s) = this.settings.as_mut() {
                    let stuck = match key {
                        "advanced" => view.advanced,
                        "players" => !view.players_off,
                        "streamer_on" => view.streamer.on,
                        _ => view.streamer.hide_wallpapers,
                    };
                    s.on.insert(key, stuck);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filter = cx.new(|cx| {
            TextInput::new("Search (paste npub, invite link, etc.)", cx).with_look(
                crate::composer::Look {
                    placeholder: white_a(0.35),
                    ..kit::field_look()
                },
            )
        });
        cx.subscribe_in(
            &filter,
            window,
            |app, input, _: &crate::composer::InputChanged, _, cx| {
                let text = input.read(cx).text().to_string();
                if let Some(key) = crate::newchat::invite_key(&text) {
                    app.resolve_invite(key, cx);
                }
                app.pasted_npub(&text, cx);
                cx.notify();
            },
        )
        .detach();
        window.focus(&filter.focus_handle(cx), cx);
        let dm: Option<HashSet<String>> = if self.live {
            None
        } else {
            Some(
                self.store
                    .borrow()
                    .chats
                    .iter()
                    .filter(|c| matches!(c.chat_type, vector_core::ChatType::DirectMessage))
                    .map(|c| c.id.clone())
                    .collect(),
            )
        };
        self.profile = None;
        self.switcher = None;
        self.new_chat = Some(crate::newchat::NewChat {
            filter,
            picked: None,
            strangers: Vec::new(),
            contacts: dm,
            joining: false,
        });
        if self.live {
            let rx = backend::run(async {
                vector_core::db::events::get_dm_contact_npubs().unwrap_or_default()
            });
            cx.spawn(async move |this, cx| {
                let Ok(list) = rx.await else { return };
                this.update(cx, |this, cx| {
                    if let Some(n) = this.new_chat.as_mut() {
                        n.contacts = Some(list.into_iter().collect());
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn upload_new_community_icon(
        &mut self,
        community: String,
        icon: crate::commsettings::Pick,
        cx: &mut Context<Self>,
    ) {
        let rx = backend::run(backend::community::set_image(
            community,
            icon.path,
            false,
            icon.crop,
            |_| {},
        ));
        cx.spawn(async move |this, cx| {
            let ok = matches!(rx.await, Ok(Ok(())));
            this.update(cx, |this, cx| {
                if ok {
                    this.reload_communities(cx);
                } else {
                    this.show_toast("Community created, but the avatar upload failed", cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_create_community(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| {
            TextInput::new("Community Name", cx)
                .max_chars(crate::createcommunity::NAME_MAX)
                .keep_on_submit()
                .with_look(crate::composer::Look {
                    weight: FontWeight::MEDIUM,
                    size: px(16.),
                    line_height: px(24.),
                    color: c(0xffffff),
                    placeholder: white_a(0.35),
                    ..kit::field_look()
                })
        });
        cx.subscribe(&name, |_, _, _: &crate::composer::InputChanged, cx| {
            cx.notify()
        })
        .detach();
        cx.subscribe_in(
            &name,
            window,
            |app, _, _: &crate::composer::SendText, window, cx| {
                app.on_create_community(crate::createcommunity::Act::Next, window, cx)
            },
        )
        .detach();
        let filter = cx.new(|cx| {
            TextInput::new("Search friends, or paste an npub", cx).with_look(
                crate::composer::Look {
                    placeholder: white_a(0.35),
                    ..kit::field_look()
                },
            )
        });
        cx.subscribe(&filter, |_, _, _: &crate::composer::InputChanged, cx| {
            cx.notify()
        })
        .detach();
        window.focus(&name.focus_handle(cx), cx);
        let contacts: Option<HashSet<String>> = if self.live {
            None
        } else {
            Some(
                self.store
                    .borrow()
                    .chats
                    .iter()
                    .filter(|c| matches!(c.chat_type, vector_core::ChatType::DirectMessage))
                    .map(|c| c.id.clone())
                    .collect(),
            )
        };
        self.leave_community();
        self.profile = None;
        self.settings = None;
        self.new_chat = None;
        self.open = None;
        self.create_community = Some(crate::createcommunity::CreateCommunity {
            step: 1,
            name,
            public: false,
            busy: false,
            progress: None,
            error: None,
            created: None,
            link: None,
            link_qr: None,
            link_error: None,
            copied: false,
            filter,
            picked: HashSet::new(),
            contacts,
            icon: None,
        });
        if self.live {
            let rx = backend::run(async {
                vector_core::db::events::get_dm_contact_npubs().unwrap_or_default()
            });
            cx.spawn(async move |this, cx| {
                let Ok(list) = rx.await else { return };
                this.update(cx, |this, cx| {
                    if let Some(m) = this.create_community.as_mut() {
                        m.contacts = Some(list.into_iter().collect());
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn make_invite_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.create_community.as_mut() else {
            return;
        };
        let Some((id, _)) = m.created.clone() else {
            return;
        };
        m.link_error = None;
        m.progress = Some(("Making your link".into(), 0.5));
        let side = (114. * window.scale_factor()).round() as u32;
        let rx = backend::run(backend::community::create_public_invite(id, None));
        cx.spawn(async move |this, cx| {
            let Ok(res) = rx.await else { return };
            this.update(cx, |this, cx| {
                let Some(m) = this.create_community.as_mut() else {
                    return;
                };
                m.progress = None;
                match res {
                    Ok(link) => {
                        m.link_qr = crate::qr::image(&link, side);
                        m.link = Some(link);
                    }
                    Err(e) => m.link_error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_create_community(
        &mut self,
        act: crate::createcommunity::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::createcommunity::Act;
        let Some(m) = self.create_community.as_mut() else {
            return;
        };
        match act {
            Act::Next => {
                if m.step == 1 && !m.name.read(cx).text().trim().is_empty() {
                    m.step = 2;
                }
            }
            Act::Back => {
                if !m.busy && m.step == 2 {
                    m.step = 1;
                    window.focus(&m.name.focus_handle(cx), cx);
                }
            }
            Act::Public(public) => m.public = public,
            Act::PickIcon => return self.pick_new_community_icon(window, cx),
            Act::Create => {
                if m.busy {
                    return;
                }
                let name = m.name.read(cx).text().trim().to_string();
                if !self.live {
                    m.created = Some(("fixture".into(), "fixture".into()));
                    m.step = 3;
                    cx.notify();
                    return;
                }
                m.busy = true;
                m.error = None;
                m.progress = Some(("Creating your community".into(), 0.1));
                let rx = backend::run(backend::community::create_community(name));
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(res) = rx.await else { return };
                    this.update_in(cx, |this, window, cx| {
                        let Some(m) = this.create_community.as_mut() else {
                            return;
                        };
                        m.busy = false;
                        m.progress = None;
                        match res {
                            Ok(ids) => {
                                if let Some(icon) = m.icon.take() {
                                    this.upload_new_community_icon(ids.0.clone(), icon, cx);
                                }
                                let Some(m) = this.create_community.as_mut() else {
                                    return;
                                };
                                m.created = Some(ids);
                                m.step = 3;
                                let public = m.public;
                                if public {
                                    this.make_invite_link(window, cx);
                                } else if let Some(m) = this.create_community.as_ref() {
                                    window.focus(&m.filter.focus_handle(cx), cx);
                                }
                                this.reload_communities(cx);
                            }
                            Err(e) => {
                                m.error = Some(e.clone());
                                this.notice("Community creation failed", e, cx);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Act::RetryLink => self.make_invite_link(window, cx),
            Act::Copy => {
                if let Some(link) = m.link.clone() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(link));
                    m.copied = true;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(1600))
                            .await;
                        this.update(cx, |this, cx| {
                            if let Some(m) = this.create_community.as_mut() {
                                m.copied = false;
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            Act::ShowQr => {
                let Some(link) = m.link.clone() else { return };
                let side = (((self.vw * 0.72).min(320.) - 32.) * window.scale_factor()).round() as u32;
                if let Some(image) = crate::qr::image(&link, side) {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    self.qr = Some((image, focus));
                }
            }
            Act::Toggle(npub) => {
                if !m.picked.remove(&npub) {
                    m.picked.insert(npub);
                }
            }
            Act::Invite => {
                let (Some((id, _)), false) = (m.created.clone(), m.busy || m.picked.is_empty())
                else {
                    return;
                };
                if !self.live {
                    return self.on_create_community(Act::Finish, window, cx);
                }
                let picked: Vec<String> = m.picked.iter().cloned().collect();
                let n = picked.len();
                m.busy = true;
                m.error = None;
                m.progress = Some(("Getting ready".into(), 0.05));
                let (step, mut steps) = futures::channel::mpsc::unbounded::<usize>();
                let rx = backend::run(async move {
                    let mut failed = 0usize;
                    for (i, npub) in picked.into_iter().enumerate() {
                        step.unbounded_send(i).ok();
                        if backend::community::invite(id.clone(), npub).await.is_err() {
                            failed += 1;
                        }
                    }
                    failed
                });
                cx.spawn_in(window, async move |this, cx| {
                    use futures::StreamExt;
                    while let Some(i) = steps.next().await {
                        let live = this.update(cx, |this, cx| {
                            if let Some(m) = this.create_community.as_mut() {
                                m.progress = Some((
                                    format!("Inviting {} of {n}", i + 1),
                                    (i as f32 / n as f32).max(0.05),
                                ));
                                cx.notify();
                            }
                        });
                        if live.is_err() {
                            return;
                        }
                    }
                    let Ok(failed) = rx.await else { return };
                    let done = this.update(cx, |this, cx| {
                        let m = this.create_community.as_mut()?;
                        let label = if failed == n { "Invites failed" } else { "Invites sent" };
                        m.progress = Some((label.into(), 1.));
                        cx.notify();
                        Some(())
                    });
                    if !matches!(done, Ok(Some(()))) {
                        return;
                    }
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(350))
                        .await;
                    this.update_in(cx, |this, window, cx| {
                        let Some(m) = this.create_community.as_mut() else {
                            return;
                        };
                        m.busy = false;
                        m.progress = None;
                        if failed > 0 {
                            this.show_toast(
                                &format!(
                                    "{failed} invite{} failed to send",
                                    if failed == 1 { "" } else { "s" }
                                ),
                                cx,
                            );
                        }
                        this.on_create_community(Act::Finish, window, cx);
                    })
                    .ok();
                })
                .detach();
            }
            Act::Finish => {
                let id = m.created.as_ref().map(|c| c.0.clone());
                self.create_community = None;
                if let Some(id) = id.filter(|_| self.live) {
                    self.open_community(id, window, cx);
                }
            }
            Act::Cancel => {
                if m.busy {
                    return;
                }
                self.create_community = None;
            }
        }
        cx.notify();
    }

    /// A pasted npub is picked: a known profile directly, anyone else as a stranger whose
    /// profile is fetched.
    fn pasted_npub(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(npub) = crate::newchat::extract_npub(text) else {
            return;
        };
        let Some(n) = self.new_chat.as_mut() else {
            return;
        };
        let store = self.store.borrow();
        if npub == store.my_npub {
            return;
        }
        let known = store.profiles.contains_key(&npub);
        drop(store);
        n.picked = Some(npub.clone());
        if known || n.strangers.contains(&npub) {
            return;
        }
        n.strangers.push(npub.clone());
        if self.live {
            let _ = backend::run(async move {
                vector_core::profile::sync::load_profile(npub, &sync::ProfileHandler).await
            });
        }
        cx.notify();
    }

    fn on_new_chat(
        &mut self,
        act: crate::newchat::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::newchat::Act;
        let Some(n) = self.new_chat.as_mut() else {
            return;
        };
        match act {
            Act::Toggle(npub) => {
                n.picked = if n.picked.as_ref() == Some(&npub) {
                    None
                } else {
                    Some(npub)
                };
                cx.notify();
            }
            Act::Start => {
                let Some(npub) = n.picked.clone() else { return };
                self.new_chat = None;
                self.store.borrow_mut().ensure_dm(&npub);
                self.open_chat(npub.into(), window, cx);
            }
            Act::Cancel => {
                self.new_chat = None;
                self.open = None;
                cx.notify();
            }
            Act::Hub => self.open_web("https://vectorapp.io/hub", cx),
            Act::Join => {
                let text = n.filter.read(cx).text().to_string();
                let Some(key) = crate::newchat::invite_key(&text) else {
                    if let Some(link) = crate::newchat::go_link(&text) {
                        self.new_chat = None;
                        self.open_go_link(link, window, cx);
                    }
                    return;
                };
                let Some(crate::newchat::Preview::Ready(info)) =
                    self.invite_previews.get(&key).cloned()
                else {
                    return;
                };
                if n.joining {
                    return;
                }
                if self.store.borrow().community(&info.community_id).is_some() {
                    self.new_chat = None;
                    self.open_community(info.community_id, window, cx);
                    return;
                }
                n.joining = true;
                cx.notify();
                self.join_by_link(key, info.name, window, cx);
            }
        }
    }

    /// Fixtures: ourselves and the first two contacts by name, as the reference mock lists them.
    fn fixture_accounts(&self) -> Vec<crate::switcher::Row> {
        let store = self.store.borrow();
        let active = &store.my_npub;
        let mut others: Vec<&vector_core::SlimProfile> = store
            .profiles
            .values()
            .filter(|p| &p.id != active)
            .collect();
        others.sort_by_key(|p| p.name.to_lowercase());
        std::iter::once(store.profiles.get(active))
            .flatten()
            .chain(others.into_iter().take(2))
            .map(|p| crate::switcher::Row {
                npub: p.id.to_string(),
                name: [&p.nickname, &p.display_name, &p.name]
                    .into_iter()
                    .find(|n| !n.is_empty())
                    .map(|n| n.to_string()),
                avatar: store.avatar_path(&p.id),
            })
            .collect()
    }

    fn show_switcher(
        &mut self,
        rows: Vec<crate::switcher::Row>,
        active: String,
        at_cap: bool,
        cx: &mut Context<Self>,
    ) {
        // A refresh after a removal keeps delete mode.
        let editing = self.switcher.as_ref().is_some_and(|s| s.editing);
        match self.profile.as_mut() {
            Some(p) => p.switcher = Some(editing),
            None if !self.is_slim => return,
            None => {}
        }
        self.switcher = Some(crate::switcher::Switcher {
            rows,
            active,
            editing,
            at_cap,
        });
        cx.notify();
    }

    fn close_switcher(&mut self, cx: &mut Context<Self>) {
        self.switcher = None;
        if let Some(p) = self.profile.as_mut() {
            p.switcher = None;
        }
        cx.notify();
    }

    fn on_switcher(
        &mut self,
        act: crate::switcher::Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::switcher::Act;
        match act {
            Act::Close => self.close_switcher(cx),
            Act::Pick(npub) => {
                self.close_switcher(cx);
                if !self.live {
                    return;
                }
                let rx = backend::run(account::switch(npub));
                cx.spawn_in(window, async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
                    this.update_in(cx, |this, window, cx| match result {
                        Ok(()) => this.reboot(window, cx),
                        Err(e) => this.notice_plain("Switch failed", &e, cx),
                    })
                    .ok();
                })
                .detach();
            }
            Act::Delete(npub) => {
                let Some(s) = self.switcher.as_ref() else {
                    return;
                };
                // A veiled row can't say which account it is, and this can't be undone.
                if self.store.borrow().streamer.on && npub != self.store.borrow().my_npub {
                    return self.show_toast("Turn off Streamer Mode to delete another account.", cx);
                }
                let row = s.rows.iter().find(|r| r.npub == npub);
                let label = row
                    .and_then(|r| r.name.clone())
                    .unwrap_or_else(|| "This account".into());
                let last = s.rows.len() == 1;
                let mut text = format!(
                    "{label} will be permanently removed from this device, with the files it keeps in Vector. Make sure you have the seed phrase or nsec backed up if you want to recover it later, and save any files you want to keep."
                );
                if last {
                    text.push_str("\n\nThis is your only Vector account on this device. All downloaded attachments will also be removed. Copy any files you want to keep before continuing.");
                }
                let mut popup = kit::Popup::notice("Remove Profile?");
                popup.text = Some(text.into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::RemoveAccount { npub }));
                cx.notify();
            }
            Act::Add => {
                self.close_switcher(cx);
                if !self.live {
                    return;
                }
                let me = self.store.borrow().my_npub.clone();
                self.adding = Some((me, false));
                let view = cx.new(|cx| LoginView::adding(window, cx));
                self.attach_login(view, cx, window);
            }
        }
    }

    fn remove_account(&mut self, npub: String, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_account_or(npub, "Delete failed", window, cx);
    }

    fn remove_account_or(
        &mut self,
        npub: String,
        fail: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.live {
            if let Some(s) = self.switcher.as_mut() {
                s.rows.retain(|r| r.npub != npub);
            }
            return;
        }
        let rx = backend::run(account::delete(npub));
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(true) => this.reboot(window, cx),
                Ok(false) => this.open_switcher(cx),
                Err(e) => this.notice_plain(fail, &e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn on_add_profile(
        &mut self,
        e: &crate::login::AddProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::login::AddProfile;
        let Some((back, committed)) = self.adding.as_mut() else {
            return;
        };
        match e {
            AddProfile::Committed => *committed = true,
            AddProfile::Back if !*committed => {
                self.adding = None;
                self.screen = Screen::Main;
                cx.notify();
            }
            AddProfile::Back => {
                let back = back.clone();
                self.adding = None;
                let _ = account::set_active(&back);
                self.reboot(window, cx);
            }
        }
    }

    /// Pick a new banner or avatar in Edit Mode; an avatar is cropped square first.
    fn pick_picture(&mut self, banner: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                if banner {
                    "Choose Banner Image"
                } else {
                    "Choose Profile Picture"
                }
                .into(),
            ),
        });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let Some(ext) = backend::upload::picture_ext(&path) else {
                this.update(cx, |this, cx| {
                    this.notice(
                        "Unsupported File",
                        "Only PNG, JPEG, GIF and WebP images can be used.".into(),
                        cx,
                    )
                })
                .ok();
                return;
            };
            let file = path.clone();
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    std::fs::read(&file)
                        .ok()
                        .and_then(|b| crate::media::decode(&b, &ext, (2048, 2048)))
                })
                .await;
            let Some((image, w, h)) = decoded else {
                this.update(cx, |this, cx| {
                    this.notice(
                        "Image Unreadable",
                        "That picture couldn't be opened.".into(),
                        cx,
                    )
                })
                .ok();
                return;
            };
            let image = Arc::new(image);
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        let Some(edit) = this.profile.as_mut().and_then(|p| p.edit.as_mut()) else {
                            return;
                        };
                        if banner {
                            edit.banner = Some(crate::profile::Pick {
                                path,
                                crop: None,
                                preview: image,
                            });
                            cx.notify();
                            return;
                        }
                        let cropper = cx.new(|cx| {
                            crate::cropper::Cropper::new(
                                image,
                                (w, h),
                                "Crop Profile Picture",
                                "Drag to move, or drag a corner to resize.",
                                true,
                                window,
                                cx,
                            )
                        });
                        cx.subscribe_in(
                            &cropper,
                            window,
                            |this, _, answer: &crate::cropper::Answer, window, cx| {
                                this.on_crop(answer, window, cx)
                            },
                        )
                        .detach();
                        this.cropper = Some((cropper, path));
                        cx.notify();
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    /// The avatar as it will upload, previewed in place.
    fn on_crop(&mut self, answer: &crate::cropper::Answer, _: &mut Window, cx: &mut Context<Self>) {
        let Some((_, path)) = self.cropper.take() else {
            return;
        };
        cx.notify();
        let crate::cropper::Answer::Crop(crop) = answer else {
            return;
        };
        let crop = crop.clone();
        cx.spawn(async move |this, cx| {
            let file = path.clone();
            let c2 = crop.clone();
            let preview = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
                    let (out, ext) = backend::upload::prepared(bytes, false, Some(c2))?;
                    crate::media::decode(&out, ext, (512, 512))
                        .map(|(i, _, _)| i)
                        .ok_or_else(|| "Couldn't read the image".to_string())
                })
                .await;
            this.update(cx, |this, cx| match preview {
                Ok(image) => {
                    if let Some(edit) = this.profile.as_mut().and_then(|p| p.edit.as_mut()) {
                        edit.avatar = Some(crate::profile::Pick {
                            path,
                            crop: Some(crop),
                            preview: Arc::new(image),
                        });
                        cx.notify();
                    }
                }
                Err(e) => this.notice("Crop Failed", e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Upload a picked picture and publish it; an uncropped banner shows as picked meanwhile.
    fn save_picture(
        &mut self,
        npub: &str,
        pick: crate::profile::Pick,
        banner: bool,
        cx: &mut Context<Self>,
    ) {
        let label = if banner { "Banner" } else { "Avatar" };
        let prev = self
            .store
            .borrow()
            .profiles
            .get(npub)
            .map(|p| p.banner_cached.clone());
        if banner && let Some(me) = self.store.borrow_mut().profiles.get_mut(npub) {
            me.banner_cached = pick.path.to_string_lossy().into_owned();
        }
        if !self.live {
            return;
        }
        let rx = backend::run(backend::upload::picture(pick.path, banner, pick.crop));
        let npub = npub.to_string();
        cx.spawn(async move |this, cx| {
            let err = match rx.await {
                Ok(Ok(_)) => return,
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            this.update(cx, |this, cx| {
                if banner
                    && let (Some(me), Some(prev)) =
                        (this.store.borrow_mut().profiles.get_mut(&npub), prev)
                {
                    me.banner_cached = prev;
                }
                this.notice(&format!("{label} Upload Failed!"), err, cx);
            })
            .ok();
        })
        .detach();
    }

    fn confirm_block_request(&mut self, npub: String, cx: &mut Context<Self>) {
        let mut popup = kit::Popup::notice("Block User");
        popup.text = Some(
            "Are you sure you want to block this user? You will no longer receive DMs from them."
                .into(),
        );
        popup.confirm = "Confirm".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::BlockRequest { npub }));
        cx.notify();
    }

    /// Accept lets what they sent load as the settings allow, and moves the chat to Messages.
    fn accept_request(&mut self, npub: String, cx: &mut Context<Self>) {
        let lift = |this: &mut Self, npub: &str, cx: &mut Context<Self>| {
            if let Some(chat) = this.store.borrow_mut().chat_mut(npub) {
                chat.metadata
                    .insert(vector_core::chat::ACCEPTED_KEY, "true".into());
            }
            this.media.borrow_mut().meta_asked.clear();
            cx.notify();
        };
        if !self.live {
            lift(self, &npub, cx);
            return;
        }
        let rx = backend::run(backend::prefs::accept(npub.clone()));
        cx.spawn(async move |this, cx| {
            let ok = matches!(rx.await, Ok(Ok(())));
            this.update(cx, |this, cx| {
                if ok {
                    lift(this, &npub, cx)
                } else {
                    this.show_toast("Failed to accept", cx)
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-ask which request senders share a Community with us when the requests change.
    fn sync_request_mutuals(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let asking: Vec<String> = {
            let store = self.store.borrow();
            store
                .chats
                .iter()
                .filter(|c| store.is_request(c))
                .map(|c| c.id.clone())
                .collect()
        };
        if asking == self.mutuals_asked {
            return;
        }
        self.mutuals_asked = asking.clone();
        if asking.is_empty() {
            self.store.borrow_mut().mutuals.clear();
            return;
        }
        let rx = backend::run(backend::prefs::request_mutuals());
        cx.spawn(async move |this, cx| {
            let Ok(next) = rx.await else { return };
            this.update(cx, |this, cx| {
                if this.mutuals_asked == asking {
                    this.store.borrow_mut().mutuals = next.into_iter().collect();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// `blockedBySync`: account edits wait until the first sync lands, or it would undo them.
    pub(super) fn blocked_by_sync(&mut self, cx: &mut Context<Self>) -> bool {
        if self.live && self.syncing {
            self.show_toast("Syncing your account — try again in a moment", cx);
            return true;
        }
        false
    }

    fn set_blocked(&mut self, npub: String, block: bool, cx: &mut Context<Self>) {
        self.set_blocked_toast(npub, block, if block { "User Blocked" } else { "User Unblocked" }, cx);
    }

    fn set_blocked_toast(&mut self, npub: String, block: bool, done: &'static str, cx: &mut Context<Self>) {
        if !self.live {
            if let Some(p) = self.store.borrow_mut().profiles.get_mut(&npub) {
                p.is_blocked = block;
            }
            self.show_toast(done, cx);
            return;
        }
        let rx = if block {
            backend::run(backend::prefs::block(npub.clone()))
        } else {
            backend::run(backend::prefs::unblock(npub.clone()))
        };
        cx.spawn(async move |this, cx| {
            let Ok(true) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(p) = this.store.borrow_mut().profiles.get_mut(&npub) {
                    p.is_blocked = block;
                }
                this.show_toast(done, cx);
            })
            .ok();
        })
        .detach();
    }

    fn set_nickname(&mut self, npub: String, nick: String, cx: &mut Context<Self>) {
        if nick.chars().count() >= 30 {
            let mut popup = kit::Popup::notice("Woah woah!");
            popup.text = Some(
                format!(
                    "A {}-character nickname seems excessive!",
                    nick.chars().count()
                )
                .into(),
            );
            self.popup = Some((popup, PopupAct::Notice));
            cx.notify();
            return;
        }
        if let Some(p) = self.store.borrow_mut().profiles.get_mut(&npub) {
            p.nickname = nick.clone();
        }
        cx.notify();
        if self.live {
            let _ = backend::run(backend::prefs::set_nickname(npub, nick));
        }
    }

    /// `showToast`: one line over a bottom gradient, for 1.5s plus 40ms a character.
    /// The account another device sent has arrived: the login screen signs in with it.
    fn finish_transfer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let security = self.security.clone();
        let Screen::Login(view) = &self.screen else {
            security.update(cx, |f, cx| {
                f.transfer_finished(Err("Sign out of this account first".into()), cx)
            });
            return;
        };
        let rx = view.update(cx, |v, cx| v.finish_transfer(window, cx));
        cx.spawn(async move |_, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            security.update(cx, |f, cx| f.transfer_finished(result, cx));
        })
        .detach();
    }

    fn on_security(
        &mut self,
        e: &crate::security::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match e {
            crate::security::Event::State { enabled, kind } => {
                if let Some(s) = self.settings.as_mut() {
                    s.on.insert("encryption", *enabled);
                    s.sec_kind = kind.clone();
                }
                cx.notify();
            }
            crate::security::Event::Toast(text) => self.show_toast(text, cx),
            crate::security::Event::Received => self.finish_transfer(window, cx),
        }
    }

    fn show_toast(&mut self, text: &str, cx: &mut Context<Self>) {
        let at = std::time::Instant::now();
        self.toast = Some((text.to_string().into(), at));
        let hold = (1500 + text.chars().count() as u64 * 40).min(6000);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(hold))
                .await;
            this.update(cx, |_, cx| cx.notify()).ok();
            cx.background_executor()
                .timer(std::time::Duration::from_millis(320))
                .await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|(_, t)| *t == at) {
                    this.toast = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn toast_view(&self, window: &mut Window) -> Option<AnyElement> {
        let (text, at) = self.toast.as_ref()?;
        let hold = (1500 + text.chars().count() as u64 * 40).min(6000) as f32 / 1000.;
        let e = at.elapsed().as_secs_f32();
        let opacity = if e < 0.3 {
            e / 0.3
        } else if e < hold {
            1.
        } else {
            (1. - (e - hold) / 0.3).max(0.)
        };
        if opacity < 1. && opacity > 0. {
            window.request_animation_frame();
        }
        Some(
            gpui::deferred(
                div()
                    .absolute()
                    .inset_0()
                    .opacity(opacity)
                    .child(div().absolute().inset_0().bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(ca(0, 0, 0, 0.), 0.5),
                        gpui::linear_color_stop(ca(0, 0, 0, 0.8), 1.),
                    )))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(80.))
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .px(px(24.))
                                    .py(px(12.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(c(PRIMARY))
                                    .bg(ca(0, 0, 0, 0.8))
                                    .text_size(px(14.))
                                    .line_height(px(24.))
                                    .text_color(c(0xffffff))
                                    .child(text.clone()),
                            ),
                    ),
            )
            .with_priority(20)
            .into_any_element(),
        )
    }
}

// ── #status-dialog ──────────────────────────────────────────────────────────────────────

/// A status is one line of at most this many characters.
const STATUS_MAX: usize = 120;

impl VectorApp {
    fn open_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .store
            .borrow()
            .me()
            .map(|p| p.status.title.clone())
            .unwrap_or_default();
        let look = crate::composer::Look {
            family: RUBIK,
            size: px(14.5),
            line_height: px(20.),
            color: c(0xffffff),
            placeholder: white_a(0.3),
            ..Default::default()
        };
        let input = cx.new(|cx| {
            let mut i = TextInput::new("What's happening?", cx)
                .with_look(look)
                .twemoji()
                .keep_on_submit()
                .submit_empty();
            i.set_text(&current, cx);
            i
        });
        cx.subscribe(&input, |app, _, _: &SendText, cx| app.save_status(None, cx))
            .detach();
        cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
            if app.picker_open && app.picker_status {
                app.close_picker(window, cx);
                return;
            }
            app.status = None;
            cx.notify();
        })
        .detach();
        cx.subscribe(&input, |_, input, _: &crate::composer::InputChanged, cx| {
            let text = input.read(cx).text().to_string();
            let clean: String = text.replace('\n', " ").chars().take(STATUS_MAX).collect();
            if clean != text {
                input.update(cx, |i, cx| i.set_text(&clean, cx));
            }
            cx.notify();
        })
        .detach();
        window.focus(&input.focus_handle(cx), cx);
        self.status = Some(input);
        cx.notify();
    }

    /// Publish the status (`Some("")` clears it), echoing it at once and rolling back on failure.
    fn save_status(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        let Some(input) = self.status.take() else {
            return;
        };
        if self.picker_status {
            self.picker_open = false;
            self.picker_status = false;
        }
        let status = text.unwrap_or_else(|| input.read(cx).text().trim().to_string());
        let me = self.store.borrow().my_npub.clone();
        let old = self
            .store
            .borrow_mut()
            .profiles
            .get_mut(&me)
            .map(|p| std::mem::replace(&mut p.status.title, status.clone()));
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(vector_core::profile::sync::update_status(status));
        cx.spawn(async move |this, cx| {
            if let Ok(true) = rx.await {
                return;
            }
            this.update(cx, |this, cx| {
                if let (Some(old), Some(p)) = (old, this.store.borrow_mut().profiles.get_mut(&me)) {
                    p.status.title = old;
                }
                let mut popup = kit::Popup::notice("Status Update Failed!");
                popup.text = Some("Failed to broadcast status update to the network.".into());
                this.popup = Some((popup, PopupAct::Notice));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn status_view(
        &self,
        store: &Store,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let input = self.status.clone()?;
        let panel = self.picker_open && self.picker_status;
        let raw = input.read(cx).text().to_string();
        let text = raw.trim().to_string();
        let empty = text.is_empty();
        let remaining = STATUS_MAX as i64 - raw.chars().count() as i64;
        let had = store.me().is_some_and(|p| !p.status.title.is_empty());
        let card =
            div()
                .id("status-dialog-card")
                .w(px(380.))
                .flex()
                .flex_col()
                .pt(px(14.))
                .px(px(16.))
                .pb(px(12.))
                .rounded(px(18.))
                .border_1()
                .border_color(c(0x2c2e2f))
                .bg(c(0x101010))
                .shadow(vec![gpui::BoxShadow {
                    color: ca(0, 0, 0, 0.55),
                    offset: gpui::point(px(0.), px(18.)),
                    blur_radius: px(60.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .h(px(24.))
                        .mb(px(12.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(white_a(0.45))
                                .child("STATUS"),
                        )
                        .child(
                            div()
                                .id("status-dialog-x")
                                .px(px(6.))
                                .py(px(4.))
                                .text_size(px(14.))
                                .line_height(px(14.))
                                .text_color(white_a(0.4))
                                .hover(|s| s.text_color(c(0xffffff)))
                                .cursor_pointer()
                                .on_click(cx.listener(|app, _, _, cx| {
                                    app.status = None;
                                    cx.notify();
                                }))
                                .child("✕"),
                        ),
                )
                .child(
                    div()
                        .mb(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .min_w_0()
                        .child(
                            div()
                                .size(px(34.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(c(0x1a1a1a))
                                .child(avatar(store, &store.my_npub, 34.)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .px(px(14.))
                                .py(px(7.))
                                .rounded_full()
                                .bg(c(0x1a1a1a))
                                .border_1()
                                .border_color(c(0x262626))
                                .when(empty, |d| d.opacity(0.45))
                                .child(
                                    div()
                                        .size(px(8.))
                                        .flex_shrink_0()
                                        .rounded_full()
                                        .bg(c(if empty { 0x555555 } else { ACCENT }))
                                        .when(!empty, |d| {
                                            d.shadow(vec![gpui::BoxShadow {
                                                color: ca(89, 252, 179, 0.6),
                                                offset: gpui::point(px(0.), px(0.)),
                                                blur_radius: px(6.),
                                                spread_radius: px(0.),
                                                inset: false,
                                            }])
                                        }),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(13.5))
                                        .line_height(px(24.))
                                        .text_color(c(0xffffff))
                                        .child(crate::inline::emoji_line(
                                            if empty { "No status" } else { &text },
                                            crate::inline::Look::new(INTER, 13.5, 24., c(0xffffff)),
                                            16.,
                                            3.,
                                        )),
                                ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .p(px(2.))
                        .pl(px(14.))
                        .rounded(px(12.))
                        .bg(c(0x1a1a1a))
                        .border_1()
                        .border_color(c(0x3d4040))
                        .child(div().flex_1().min_w_0().py(px(9.)).px(px(4.)).child(input))
                        .child(
                            div()
                                .min_w(px(22.))
                                .text_right()
                                .text_size(px(11.))
                                .text_color(if remaining <= 10 {
                                    c(0xffb347)
                                } else {
                                    white_a(0.35)
                                })
                                .when(remaining <= 30, |d| d.child(remaining.to_string())),
                        )
                        .child(
                            div()
                                .id("status-emoji-btn")
                                .ml(px(2.))
                                .size(px(40.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.7)
                                .hover(|s| s.opacity(1.))
                                .cursor_pointer()
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.toggle_status_picker(window, cx)
                                }))
                                .child(
                                    svg()
                                        .path(if self.picker_open && self.picker_status {
                                            "icons/wink-face.svg"
                                        } else {
                                            "icons/smile-face.svg"
                                        })
                                        .size(px(20.))
                                        .text_color(c(0xffffff)),
                                ),
                        ),
                )
                .child(
                    div()
                        .mt(px(12.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .id("status-btn-clear")
                                .px(px(2.))
                                .py(px(6.))
                                .text_size(px(12.5))
                                .text_color(white_a(0.4))
                                .hover(|s| s.text_color(c(0xffffff)))
                                .cursor_pointer()
                                .when(!had, |d| d.invisible())
                                .on_click(cx.listener(|app, _, _, cx| {
                                    app.save_status(Some(String::new()), cx)
                                }))
                                .child("Clear Status"),
                        )
                        .child(
                            div()
                                .id("status-btn-save")
                                .px(px(26.))
                                .py(px(9.))
                                .rounded(px(10.))
                                .bg(c(ACCENT))
                                .text_color(c(0x06130d))
                                .text_size(px(14.))
                                .line_height(px(17.))
                                .font_weight(FontWeight::BOLD)
                                .cursor_pointer()
                                .on_click(cx.listener(|app, _, _, cx| app.save_status(None, cx)))
                                .child("Save"),
                        ),
                );
        Some(
            gpui::deferred(
                div()
                    .id("status-dialog")
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .map(|d| {
                        if panel {
                            d.items_start().pt(px(
                                24f32.max(f32::from(window.viewport_size().height) * 0.07)
                            ))
                        } else {
                            d.items_center()
                        }
                    })
                    .bg(ca(0, 0, 0, 0.72))
                    .occlude()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|app, _, window, cx| {
                            // The first outside press dismisses the panel; the next, the dialog.
                            if app.picker_open && app.picker_status {
                                app.close_picker(window, cx);
                                return;
                            }
                            app.status = None;
                            cx.notify();
                        }),
                    )
                    .child(card)
                    .when(panel, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(10.))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(self.emoji_picker.clone()),
                                ),
                        )
                    }),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }
}

// ── #qr-overlay ─────────────────────────────────────────────────────────────────────────

impl VectorApp {
    fn qr_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (image, focus) = self.qr.clone()?;
        let tile = (self.vw * 0.72).min(320.);
        let close = |app: &mut Self, cx: &mut Context<Self>| {
            app.qr = None;
            cx.notify();
        };
        let card = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(22.))
            .p(px(24.))
            .rounded(px(18.))
            .border_1()
            .border_color(c(0x2c2e2f))
            .bg(c(0x101010))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .w(px(tile))
                    .p(px(16.))
                    .rounded(px(10.))
                    .bg(c(0xffffff))
                    .child(img(image).size(px(tile - 32.))),
            )
            .child(
                div()
                    .id("qr-overlay-close")
                    .w_full()
                    .py(px(12.))
                    .rounded(px(12.))
                    .bg(c(ACCENT))
                    .text_color(c(0x000000))
                    .text_size(px(16.))
                    .line_height(px(19.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |app, _, _, cx| close(app, cx)))
                    .child("Close"),
            );
        Some(
            gpui::deferred(
                div()
                    .id("qr-overlay")
                    .track_focus(&focus)
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(ca(0, 0, 0, 0.88))
                    .occlude()
                    .on_key_down(cx.listener(move |app, e: &gpui::KeyDownEvent, _, cx| {
                        if e.keystroke.key == "escape" {
                            close(app, cx);
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |app, _, _, cx| close(app, cx)),
                    )
                    .child(card),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }
}

/// Fixtures: the reference's relay and server rows.
fn fixture_network() -> (
    Vec<backend::network::RelayRow>,
    Vec<vector_core::blossom_servers::BlossomServerInfo>,
) {
    use backend::network::RelayRow;
    use vector_core::blossom_stats::ServerStatus;
    let relay =
        |url: &str, status: &'static str, is_default: bool, enabled: bool, mode: &str| RelayRow {
            url: url.into(),
            status,
            is_default,
            enabled,
            mode: mode.into(),
        };
    let relays = vec![
        relay("wss://jskitty.com/nostr", "connected", true, true, "both"),
        relay(
            "wss://asia.vectorapp.io/nostr",
            "connected",
            true,
            true,
            "both",
        ),
        relay(
            "wss://nostr.computingcache.com",
            "connecting",
            true,
            true,
            "both",
        ),
        relay("wss://relay.ditto.pub", "disabled", true, false, "both"),
        relay(
            "wss://relay.example.org",
            "disconnected",
            false,
            true,
            "read",
        ),
    ];
    let server =
        |url: &str,
         enabled: bool,
         state: &'static str,
         label: &'static str,
         tone: &'static str,
         latency_ms: Option<u32>| vector_core::blossom_servers::BlossomServerInfo {
            url: url.into(),
            is_default: true,
            is_custom: false,
            enabled,
            status: ServerStatus {
                state,
                label,
                tone,
                latency_ms,
            },
        };
    let servers = vec![
        server(
            "https://magnitude.vectorapp.io",
            true,
            "online",
            "Online",
            "connected",
            Some(84),
        ),
        server(
            "https://blossom.ditto.pub",
            true,
            "untested",
            "Untested",
            "disabled",
            None,
        ),
        server(
            "https://blossom.primal.net",
            false,
            "disabled",
            "Disabled",
            "disabled",
            None,
        ),
    ];
    (relays, servers)
}

// ── voice recorder ──────────────────────────────────────────────────────────────────────

/// Run blocking recorder work off the UI thread.
fn rec_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> futures::channel::oneshot::Receiver<T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    // spawn-detached: device I/O on bytes in hand, no account state.
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

impl VectorApp {
    fn rec_ui(&mut self, ui: crate::recorder::Ui, _: &mut Window, cx: &mut Context<Self>) {
        use crate::recorder::{Effect, State, Ui};
        match ui {
            Ui::Press(at) => {
                if self.open.is_none() {
                    return;
                }
                let Some(press) = self.rec.press(at) else { return };
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(crate::recorder::HOLD_MS))
                        .await;
                    this.update(cx, |app, cx| {
                        if app.rec.press == press && app.rec.state == State::Pending {
                            app.rec_start(press, cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Ui::Move(at) => match self.rec.moved(at) {
                Effect::Cancel => self.rec_cancel(),
                Effect::Lock => self.rec.lock(),
                _ => {}
            },
            Ui::Release => {
                if self.rec.released() == Effect::Stop {
                    self.rec_stop(cx);
                }
            }
            Ui::DotClick => {
                if self.rec.state == State::Locked {
                    self.rec_stop(cx);
                }
            }
            Ui::Delete => {
                if let Some(id) = self.rec.source {
                    let _ = crate::audio::engine().map(|e| e.stop(id));
                }
                vector_media::voice::AudioRecorder::global().cancel();
                self.rec.idle();
            }
            Ui::PlayPause => {
                let Some(id) = self.rec.source else { return };
                let Ok(e) = crate::audio::engine() else { return };
                if self.rec.playing {
                    let _ = e.pause(id);
                    self.rec.paused();
                } else {
                    let pos = self.rec.position();
                    let pos = if pos >= self.rec.duration { 0 } else { pos };
                    let _ = e.seek(id, pos);
                    if e.play(id).is_ok() {
                        self.rec.played(pos);
                    }
                }
            }
            Ui::Dictate => {
                if self.rec.state != crate::recorder::State::Preview || self.rec.dictating {
                    return;
                }
                self.rec.dictating = true;
                let source = self.rec.source;
                let rx = rec_thread(|| tauri::async_runtime::block_on(backend::transcribe::transcribe_recording()));
                cx.spawn(async move |this, cx| {
                    let res = rx.await.unwrap_or_else(|e| Err(e.to_string()));
                    this.update_in(cx, |app, window, cx| {
                        // Deleted meanwhile: the words go with it.
                        if app.rec.source != source || app.rec.state != crate::recorder::State::Preview {
                            return;
                        }
                        app.rec.dictating = false;
                        let text = match res {
                            Ok(t) if !t.is_empty() => t,
                            Ok(_) => return app.show_toast("No words heard in that recording", cx),
                            Err(_) => return app.show_toast("Couldn't turn that into text", cx),
                        };
                        if let Some(id) = app.rec.source {
                            let _ = crate::audio::engine().map(|e| e.stop(id));
                        }
                        app.rec.idle();
                        if matches!(app.mode, Mode::Edit { .. }) {
                            app.mode = Mode::Normal;
                            app.input.update(cx, |i, cx| i.set_text("", cx));
                        }
                        app.input.update(cx, |i, cx| {
                            let now = i.text().trim().to_string();
                            let joined = if now.is_empty() { text } else { format!("{} {text}", i.text()) };
                            i.set_text(&joined, cx)
                        });
                        window.focus(&app.input.read(cx).focus_handle(cx), cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Ui::Seek(f) => {
                let Some(id) = self.rec.source else { return };
                let ms = (f.clamp(0., 1.) * self.rec.duration as f32) as u64;
                if let Ok(e) = crate::audio::engine() {
                    let _ = e.seek(id, ms);
                }
                self.rec.seek(ms);
            }
        }
        cx.notify();
    }

    fn rec_start(&mut self, press: u64, cx: &mut Context<Self>) {
        let rx = rec_thread(|| {
            futures::executor::block_on(vector_media::mic_access::require())?;
            crate::audio::engine()?;
            vector_media::voice::AudioRecorder::global().start()
        });
        cx.spawn(async move |this, cx| {
            let done = rx.await.unwrap_or_else(|e| Err(e.to_string()));
            this.update(cx, |app, cx| {
                use crate::recorder::State;
                let live = app.rec.press == press && app.rec.state == State::Pending;
                match done {
                    Ok(()) if live => app.rec.recording(),
                    Ok(()) => vector_media::voice::AudioRecorder::global().cancel(),
                    Err(e) if e == vector_media::mic_access::DENIED => {
                        app.rec.idle();
                        app.mic_access(cx);
                    }
                    Err(e) => {
                        app.rec.idle();
                        app.notice("Recording Error", e, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rec_cancel(&mut self) {
        vector_media::voice::AudioRecorder::global().cancel();
        self.rec.cancel();
    }

    fn rec_stop(&mut self, cx: &mut Context<Self>) {
        self.rec.status = "Processing...";
        let rx = rec_thread(|| vector_media::voice::AudioRecorder::global().stop());
        cx.spawn(async move |this, cx| {
            let done = rx.await.unwrap_or_else(|e| Err(e.to_string()));
            this.update(cx, |app, cx| {
                match done {
                    Ok(r) => {
                        app.rec.preview(r.id, r.duration_ms);
                        app.rec.can_dictate = {
                            let model = backend::transcribe::choice().0;
                            backend::transcribe::downloaded(&model)
                        };
                    }
                    Err(e) => {
                        log::warn!("recording stop failed: {e}");
                        app.rec.idle();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_voice(&mut self, cx: &mut Context<Self>) {
        let Some(receiver) = self.open.clone() else { return };
        let reply = match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Reply { id, .. } => id,
            other => {
                self.mode = other;
                String::new()
            }
        };
        self.rec.idle();
        cx.notify();
        let rx = backend::run(messaging::send_voice(receiver.to_string(), reply));
        cx.spawn(async move |this, cx| {
            let err = match rx.await {
                Ok(Ok(())) => return,
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            this.update(cx, |this, cx| this.notice("Voice Message Failed", err, cx))
                .ok();
        })
        .detach();
    }
}

/// `VECTOR_SYNC_REPORT=epoch-error` fails the epoch read alone.
fn sync_report_fixture(f: Option<serde_json::Value>) -> (Result<serde_json::Value, String>, Result<serde_json::Value, String>) {
    let f = f.unwrap_or_default();
    let epoch = if std::env::var("VECTOR_SYNC_REPORT").is_ok_and(|v| v == "epoch-error") {
        Err("relay timed out".to_string())
    } else {
        Ok(f["epoch"].clone())
    };
    (Ok(f["control"].clone()), epoch)
}

/// `startKeyLink`: a pin's link to share, when it names a place a link can carry.
fn start_key_link(key: &str) -> Option<String> {
    const PLACE: &str = "https://vectorapp.io/go#";
    let mut parts = key.split('/');
    match parts.next()? {
        "c" => {
            let community = parts.next()?;
            Some(match parts.next() {
                Some(ch) if !ch.is_empty() => format!("{PLACE}c/{community}/{ch}"),
                _ => format!("{PLACE}c/{community}"),
            })
        }
        "app" => Some(format!("{PLACE}nexus/{}", parts.next()?)),
        "settings" | "nexus" | "requests" => Some(format!("{PLACE}{key}")),
        _ => None,
    }
}

/// What would make a relay move worse than it looks, each said once.
fn relay_cautions(checks: &[vector_core::community::v2::service::RelayCheck], relays: &[String]) -> Vec<String> {
    let named = |list: Vec<&str>| list.iter().map(|r| format!("<b>{}</b>", kit::escape(r))).collect::<Vec<_>>().join(", ");
    let mut out = Vec::new();
    let throttled: Vec<&str> = checks.iter().filter(|c| c.rate_limited).map(|c| c.relay.as_str()).collect();
    if !throttled.is_empty() {
        out.push(format!(
            "{} slowed down a short burst of test events. A community on a rate-limited relay can drop posts and fall out of sync when it's busy.",
            named(throttled)
        ));
    }
    for (net, others, tld) in [("Tor", "Standard or I2P", ".onion"), ("I2P", "Standard or Tor", ".i2p")] {
        let only: Vec<&str> = checks.iter().filter(|c| c.only_on == Some(net)).map(|c| c.relay.as_str()).collect();
        if only.is_empty() {
            continue;
        }
        let host = |r: &str| url::Url::parse(r).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
        let no_other_way_in = relays.iter().all(|r| host(r).ends_with(tld));
        let n = only.len();
        out.push(format!(
            "{} can only be reached over {net}. Members using Vector over {others} won't reach {}{}.",
            named(only),
            if n == 1 { "it" } else { "them" },
            if no_other_way_in { ", and with no other relay left they'd lose this community" } else { "" },
        ));
    }
    out
}

/// The settings search: the nav's compact field, or a phone's full-width one.
fn settings_search_look(slim: bool) -> crate::composer::Look {
    crate::composer::Look {
        size: px(if slim { 17. } else { 14. }),
        line_height: px(if slim { 22. } else { 17. }),
        weight: FontWeight::NORMAL,
        placeholder: c(if slim { 0x6f6f6f } else { 0xb2b2b2 }),
        ..kit::field_look()
    }
}
