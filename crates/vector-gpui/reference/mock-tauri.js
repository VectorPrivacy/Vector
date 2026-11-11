// A stand-in backend for the Tauri frontend: boots it through the debug hot-reload path with
// the fixture data, so the reference render needs no Rust, no account and no network.
(() => {
    const FIX = window.__VECTOR_FIXTURES__;
    const answers = {
        get_platform_features: { transcription: false, notification_sounds: true, os: 'linux', is_mobile: false, debug_mode: true, media_url: null, self_update: false, share_audio: false, players: false },
        debug_hot_reload_sync: { success: true, npub: FIX.npub, profiles: FIX.profiles, chats: FIX.chats, pinned: [] },
        get_storage_info: { total_bytes: 41300000, type_distribution: { jpg: 9200000, png: 3100000, mp4: 21000000, mp3: 1400000, '/cache': 4100000, pdf: 2500000 } }, get_theme: 'vector', get_notification_settings: { global_mute: false, sound: { type: 'Default' }, mute_everyone: false }, transport_get_state: { supported: ['clearnet'], prelogin: 'clearnet', kind: 'clearnet' }, get_encryption_status: { enabled: true, security_type: 'pin' }, audio_devices_list: { inputs: ['Built-in Microphone', 'USB Headset'], outputs: ['Built-in Speakers', 'USB Headset'], default_input: 'Built-in Microphone', default_output: 'Built-in Speakers', prefs: { input: null, output: null } }, call_audio_settings_get: { auto_gain: true, echo_cancel: true, noise_suppress: false, mic_volume: 0.8, speaker_volume: 1 }, realtime_direct_get: false, get_start_layout: { v: 1, nodes: [{ type: 'item', key: 'dm/npub190vqdjtlpcq27xslcveglfmr4ynfwg7gmw86cnun4acakxrdd6gqnp929a' }, { type: 'item', key: 'settings/audio' }, { type: 'item', key: 'requests' }, { type: 'folder', id: 'f1', name: 'Work', keys: ['dm/npub1sxmr0k8u6trd5c6eu6trzyapzux7090ykujmsng7pdx0m8k93n5sd02fwn', 'dm/npub1fsndjp6vylvfahjeyuxq4s2tw8s8rv2j89ge7a28fvhnhf35s86sg3uctf'] }] }, export_keys: { nsec: 'nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5', seed_phrase: 'abandon ability able about above absent absorb abstract absurd abuse access accident' }, verify_credential: null, transfer_start: { id: 1, code: '7-orbit-lemon-stage', qr: 'Vector transfer code: 7-orbit-lemon-stage' }, get_archived_chats: [], list_emoji_packs: [], get_relays: [{ url: 'wss://jskitty.com/nostr', status: 'connected', is_default: true, is_custom: false, enabled: true, mode: 'both' }, { url: 'wss://asia.vectorapp.io/nostr', status: 'connected', is_default: true, is_custom: false, enabled: true, mode: 'both' }, { url: 'wss://nostr.computingcache.com', status: 'connecting', is_default: true, is_custom: false, enabled: true, mode: 'both' }, { url: 'wss://relay.ditto.pub', status: 'disabled', is_default: true, is_custom: false, enabled: false, mode: 'both' }, { url: 'wss://relay.example.org', status: 'disconnected', is_default: false, is_custom: true, enabled: true, mode: 'read' }], get_blossom_servers_config: [{ url: 'https://magnitude.vectorapp.io', is_default: true, is_custom: false, enabled: true, status: { state: 'online', label: 'Online', tone: 'connected', latency_ms: 84 } }, { url: 'https://blossom.ditto.pub', is_default: true, is_custom: false, enabled: true, status: { state: 'untested', label: 'Untested', tone: 'disabled', latency_ms: null } }, { url: 'https://blossom.primal.net', is_default: true, is_custom: false, enabled: false, status: { state: 'disabled', label: 'Disabled', tone: 'disabled', latency_ms: null } }], get_accounts: [{ npub: FIX.npub }], get_current_account: FIX.npub,
    };
    const msgs = (id) => (FIX.chats.find((c) => c.id === id)?.messages || []).slice().sort((a, b) => a.at - b.at);
    const fns = {
        audio_probe: ({ path }) => (String(path).includes("loop") ? 12000 : 7400),
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
        get_chat_commands: () => [], list_communities: () => FIX.communities || [], get_community: ({ communityId }) => (FIX.communities || []).find((c) => c.community_id === communityId), get_hidden_banners: () => [], cache_community_image: ({ communityId, isBanner }) => { const c = (FIX.communities || []).find((x) => x.community_id === communityId); return (isBanner ? c?.banner : c?.icon) || null; },
        get_community_capabilities: () => Object.fromEntries(['manage_metadata','manage_channels','create_invite','kick','ban','manage_messages','manage_roles','manage_admin_role','pin_messages','control_write','moderate_members','view_audit_log','manage_nicknames','change_nickname'].map((k) => [k, true])),
        get_community_members: ({ communityId }) => ((FIX.communities || []).find((c) => c.community_id === communityId)?.members || []).map((npub, i) => ({ npub, last_active: 1794275880 - i * 600 })),
        get_channel_pins: () => [], sync_community_channel: () => ({ new_messages: 0, reached_start: true, oldest_ms: null }), fill_community_gap: () => 0,
        get_community_timeouts: () => ({}), get_community_mentions: () => ({ anyone: true, pingers: [], roles: [] }), get_community_admins: () => [], get_community_role_graph: () => ({ roles: [], grants: [], activated: false, readers: {} }), get_community_banlist: () => (FIX.profiles || []).filter((p) => p.name === 'frank' || p.name === 'heidi').map((p) => p.id), get_channel_send_denied: () => [], check_community_raid: () => ({ detected: !!FIX.moderation, suspects: FIX.moderation ? FIX.moderation.report.suspects : 0 }),
        get_community_join_order: () => [],
        get_moderation_intel: () => FIX.moderation,
        fetch_nostr_embed: ({ reference }) => { const e = (FIX.embeds || {})[reference]; if (!e) throw 'Not found on any relay'; return e; }, cache_url_image: ({ url }) => url, list_emoji_packs: () => Object.values(FIX.packs || {}), refresh_emoji_packs: () => Object.values(FIX.packs || {}), get_theme_slot_anchor: () => '', fetch_emoji_pack_by_naddr: ({ naddr }) => { const p = (FIX.packs || {})[naddr]; if (!p) throw 'Pack not found'; return p; }, get_or_cache_image: ({ url }) => url.replace('https://ref.invalid/', ''),
        policy_presets: () => FIX.policy_presets, list_community_policies: () => ({ policies: [], using_builtin: true }),
        get_community_roles_view: () => FIX.roles_view,
        get_community_audit_log: () => FIX.audit || { entries: [], compacted: false },
        create_community: ({ name }) => ({ community_id: 'f'.repeat(64), channel_id: 'e'.repeat(64), channel_name: 'general', owner_npub: FIX.npub, name }),
        create_public_invite: () => 'https://vectorapp.io/invite/' + 'Ab3'.repeat(20),
        list_public_invites: () => [{ url: 'https://vectorapp.io/invite/' + 'Ab3'.repeat(20), token: 'tok1', label: 'Reddit', join_count: 3 }, { url: 'https://vectorapp.io/invite/' + 'Zx9'.repeat(20), token: 'tok2', label: null, join_count: 0 }],
        get_community_invite_summary: () => ({ is_public: true, creators: [{ npub: FIX.npub, count: 2 }] }),
        list_community_invites: () => FIX.invites || [],
        preview_public_invite: () => ({ name: 'Night Owls', description: 'Late-night builders, ships and shenanigans.', icon: null, community_id: 'c'.repeat(64) }),
        get_notify_prefs: ({ communityId, scopeIds }) => [...new Set([communityId, ...(scopeIds || [])].filter(Boolean))].map((scope_id) => ({ scope_id, level: null, effective_level: scope_id === communityId ? 'mentions' : 'mentions', muted: false, mute_until: 0, suppress_everyone: false, everyone_muted_globally: false })), get_emoji_usage: () => [],
        cache_android_file: ({ filePath }) => {
            const name = filePath.split('/').pop();
            return { size: (window.__FILE_SIZES__ || {})[name] || 0, name, extension: name.split('.').pop() };
        },
        blossom_upload_verdict: () => ({ likely: true, reasons: [] }),
        get_compression_status: () => window.__COMPRESSION__ || null,
        file_has_metadata: () => false,
        list_accounts_with_metadata: () => FIX.profiles.slice(0, 3).map((p, i) => ({
            npub: p.id, display_name: p.nickname || p.display_name || p.name || null,
            avatar_url: null, avatar_cached: p.avatar_cached || null, has_encryption: false, last_active: 100 - i,
        })),
        get_max_account_tier: () => window.__ACCOUNT_TIER__ ?? 1,
        get_my_badges: () => window.__BADGES__ || { vector: false, vector_2026: false, tier: 0, bug_hunter: 0 },
        get_invited_users: () => window.__BADGES__?.invites || 0,
        check_fawkes_badge: () => !!window.__BADGES__?.vector,
        check_vector_2026_badge: () => !!window.__BADGES__?.vector_2026,
        get_bug_hunter_tier: () => window.__BADGES__?.bug_hunter || 0,
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
