// A stand-in backend for the Tauri frontend: boots it through the debug hot-reload path with
// the fixture data, so the reference render needs no Rust, no account and no network.
(() => {
    const FIX = window.__VECTOR_FIXTURES__;
    const answers = {
        get_platform_features: { transcription: false, notification_sounds: false, os: 'linux', is_mobile: false, debug_mode: true, media_url: null, self_update: false, share_audio: false, players: false },
        debug_hot_reload_sync: { success: true, npub: FIX.npub, profiles: FIX.profiles, chats: FIX.chats, pinned: [] },
        get_theme: 'vector', transport_get_state: { supported: ['clearnet'], prelogin: 'clearnet', kind: 'clearnet' }, get_encryption_status: { enabled: false }, get_archived_chats: [], list_emoji_packs: [], get_relays: [], get_accounts: [{ npub: FIX.npub }], get_current_account: FIX.npub,
    };
    const msgs = (id) => (FIX.chats.find((c) => c.id === id)?.messages || []).slice().sort((a, b) => a.at - b.at);
    const fns = {
        get_chat_message_count: ({ chatId }) => msgs(chatId).length,
        get_message_views: ({ chatId, limit = 50, offset = 0 }) => msgs(chatId).reverse().slice(offset, offset + limit),
        get_messages_around: ({ chatId, anchorId, before = 0, after = 0 }) => {
            const all = msgs(chatId); const i = all.findIndex((m) => m.id === anchorId);
            return i < 0 ? all.slice(-before) : all.slice(Math.max(0, i - before + 1), i + after + 1);
        },
        get_message_position: ({ chatId, messageId }) => msgs(chatId).findIndex((m) => m.id === messageId),
        get_unread_counts: () => Object.fromEntries(FIX.chats.map((c) => {
            const i = c.messages.findIndex((m) => m.id === c.last_read);
            return [c.id, c.messages.slice(i + 1).filter((m) => !m.mine).length];
        })),
        mark_as_read: ({ chatId, messageId }) => { const c = FIX.chats.find((x) => x.id === chatId); if (c && messageId) c.last_read = messageId; return null; },
        create_account: () => ({ public: FIX.npub, existing: false }), login: () => { throw 'Invalid nsec'; },
        generate_thumbhash_preview: ({ npub, msgId }) => {
            const m = msgs(npub).find((x) => x.id === msgId);
            const hash = m?.attachments?.find((a) => a.img_meta)?.img_meta?.thumbhash;
            if (!hash || !FIX.thumbs?.[hash]) throw 'No thumbhash';
            return FIX.thumbs[hash];
        },
        get_system_events: () => [], pivx_get_chat_payments: () => [], get_message_delete_meta_bulk: () => [],
        get_chat_commands: () => [], list_communities: () => [], get_hidden_banners: () => [],
        get_community_join_order: () => [], get_my_badges: () => [], get_emoji_usage: () => [],
    };
    // MOCK_MODE: 'app' (signed in), 'login' (no account) or 'unlock' (a PIN-locked account).
    const MODE = window.__MOCK_MODE__ || 'app';
    if (MODE !== 'app') {
        delete answers.debug_hot_reload_sync;
        fns.debug_hot_reload_sync = () => { throw 'Backend not initialized - perform normal login'; };
        answers.get_encryption_and_key = MODE === 'login'
            ? { account_exists: false, enabled: false, security_type: 'pin', signer_type: 'local' }
            : { account_exists: true, enabled: true, security_type: 'pin', signer_type: 'local' };
        answers.get_accounts = MODE === 'login' ? [] : [{ npub: FIX.npub }];
        answers.get_current_account = MODE === 'login' ? null : FIX.npub;
    }
    const seen = new Set();
    window.__mockMissed = [];
    const invoke = async (cmd, args) => {
        if (cmd in answers) return structuredClone(answers[cmd]);
        if (cmd in fns) return structuredClone(fns[cmd](args || {}));
        if (!seen.has(cmd)) { seen.add(cmd); window.__mockMissed.push(cmd); }
        return null;
    };
    const noop = async () => {};
    const win = new Proxy({}, { get: (_, k) => k === 'label' ? 'main' : (k === 'then' ? undefined : (async () => (k.startsWith('is') ? false : (k === 'innerSize' || k === 'outerSize' ? { width: innerWidth, height: innerHeight, toLogical: () => ({ width: innerWidth, height: innerHeight }) } : (k === 'scaleFactor' ? 1 : (k.startsWith('on') || k === 'listen' ? () => {} : null)))))) });
    window.__TAURI__ = {
        core: { invoke, convertFileSrc: (p) => '/__ref/' + p, Channel: class { constructor() { this.onmessage = null; } } },
        event: { listen: async () => () => {}, once: async () => () => {}, emit: noop },
        window: { getCurrentWindow: () => win, currentMonitor: async () => null, primaryMonitor: async () => null, availableMonitors: async () => [] },
        webviewWindow: { getCurrentWebviewWindow: () => win },
        webview: { getCurrentWebview: () => win },
        dpi: { PhysicalSize: class {}, LogicalSize: class { constructor(w, h) { this.width = w; this.height = h; } }, PhysicalPosition: class {}, LogicalPosition: class {} },
        app: { getVersion: async () => '0.5.1', getName: async () => 'Vector' },
        opener: { openUrl: noop, openPath: noop, revealItemInDir: noop },
        dialog: { open: async () => null, save: async () => null, message: noop, ask: async () => false, confirm: async () => false },
        process: { exit: noop, relaunch: noop },
        updater: { check: async () => null },
        notification: { isPermissionGranted: async () => false, requestPermission: async () => 'denied', sendNotification: () => {} },
        clipboardManager: { writeText: noop, readText: async () => '' },
    };
})();
