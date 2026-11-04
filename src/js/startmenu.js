// The widescreen Main Menu (components/shell/StartMenu.svelte): what each pin shows and
// opens, the search over everything that can be opened or pinned, and the footer's
// account and audio controls. Pins are vector-core's `start_menu` keys, synced as
// `vector/start`; the menu is the navigation unless Appearance picks the Navbar.

const START_NAV_KEY = 'ws_nav';

/** Whether the Main Menu, rather than the rail's own tabs, is how this device navigates. */
function startMenuActive() {
    return VectorSvelte.startMenuState().mode === 'start';
}

/** Appearance's choice between the Main Menu and the Navbar. Per device. */
function setStartNavMode(mode) {
    try { localStorage.setItem(START_NAV_KEY, mode); } catch (_) { /* per-device nicety only */ }
    VectorSvelte.setStartMode(mode);
    document.body.classList.toggle('ws-start', VectorSvelte.startMenuState().mode === 'start');
}

function loadStartNavMode() {
    let mode = 'start';
    try { mode = localStorage.getItem(START_NAV_KEY) === 'navbar' ? 'navbar' : 'start'; } catch (_) { /* default stands */ }
    setStartNavMode(mode);
}

/** The account's pins, from the local mirror; cleared first so a swap never shows the last account's. */
async function loadStartLayout() {
    VectorSvelte.setStartLayout(null);
    try {
        VectorSvelte.setStartLayout(await invoke('get_start_layout'));
    } catch (e) {
        console.warn('[Start] layout load failed:', e);
    }
}

async function startLayoutEdit(cmd, args) {
    try {
        VectorSvelte.setStartLayout(await invoke(cmd, args));
    } catch (e) {
        showToast(String(e));
    }
}

function startPinned(key) {
    const nodes = VectorSvelte.startMenuState().layout.nodes || [];
    return nodes.some(n => (n.type === 'folder' ? (n.keys || []).includes(key) : n.key === key));
}

function toggleStartPin(key) {
    return startLayoutEdit(startPinned(key) ? 'start_unpin' : 'start_pin', { key });
}

/** A menu's Pin to Main Menu entry; nothing where the Main Menu isn't in use. */
function startPinItems(key) {
    if (!key || !wsActive() || !startMenuActive()) return [];
    const pinned = startPinned(key);
    return [{ label: pinned ? 'Unpin' : 'Pin to Main Menu', icon: 'pin', danger: pinned, onClick: () => toggleStartPin(key) }];
}

/** The community whose channels include `channelId`, from the chats this device holds. */
function startCommunityName(communityId) {
    const chat = arrChats.find(c => communityIdOfChat(c) === communityId);
    return chat?.metadata?.custom_fields?.name || null;
}

function startCommunityIcon(communityId) {
    const chat = arrChats.find(c => communityIdOfChat(c) === communityId && c.metadata?.avatar_cached);
    return chat ? convertFileSrc(chat.metadata.avatar_cached) : null;
}

function startInitial(name) {
    return (String(name || '?').match(/[\p{L}\p{N}]/u)?.[0] || '?').toUpperCase();
}

/** Settings' categories and sections as the screen names them. */
function startSettingsPlace(path) {
    const [cat, section] = path.split('/');
    const places = VectorSvelte.settingsScreen().places || {};
    const c = places[cat];
    if (!c) return null;
    return {
        label: section ? (c.sections[section] || section) : c.label,
        sub: section ? `Settings › ${c.label}` : 'Settings',
        icon: (section && c.sectionIcons?.[section]) || c.icon || 'settings',
    };
}

/**
 * What a pin key shows: `{ label, sub, missing }` and one of `app` (a Nexus listing),
 * `src` (+ `round`), `initial` or `icon`.
 */
function resolveStartKey(key) {
    const [kind, a, b] = String(key).split('/');
    switch (kind) {
        case 'app': {
            const app = marketplaceApps.find(x => x.id === a);
            if (!app) return { label: a, sub: 'Mini App', icon: 'grid-fill', missing: true };
            resolveMarketplaceIcon(app);
            return { label: app.name, sub: 'Mini App', app };
        }
        case 'dm': {
            const profile = getProfile(a);
            const label = getName(profile || a);
            const src = profile ? getProfileAvatarSrc(profile) : null;
            return src ? { label, sub: 'Direct Message', src, round: true } : { label, sub: 'Direct Message', initial: startInitial(label), round: true };
        }
        case 'c': {
            const name = startCommunityName(a);
            if (!name) return { label: 'Community', sub: "A community you're not in", icon: 'users-multi', missing: true };
            if (b) {
                const ch = arrChats.find(c => c.id === b);
                const chName = ch?.metadata?.custom_fields?.channel_name;
                return { label: chName ? `#${chName}` : 'Channel', sub: name, icon: 'channel-hash', missing: !ch };
            }
            const src = startCommunityIcon(a);
            return src ? { label: name, sub: 'Community', src } : { label: name, sub: 'Community', initial: startInitial(name) };
        }
        case 'settings': {
            const rest = String(key).slice('settings'.length + 1);
            return (rest && startSettingsPlace(rest)) || { label: 'Settings', sub: 'Settings', icon: 'settings' };
        }
        case 'nexus':
            return { label: 'App Store', sub: 'Mini Apps', icon: 'nexus' };
        case 'requests':
            return { label: 'Requests', sub: 'Messages', icon: 'mail' };
        default:
            return { label: key, icon: 'share', missing: true };
    }
}

/** The chat on screen, when its composer could send a Mini App there. */
function startInviteTarget() {
    const chat = getChat(strOpenChat);
    if (!chat || chat._joining || !chatOnScreen(chat.id)) return null;
    const communityId = communityIdOfChat(chat);
    if (communityId) {
        const channel = communityChannelsCache.get(communityId)?.find(c => c.id === chat.id);
        if (channel?.view === 'voice' || !channelAllowsSend(chat) || channelAwaitsWriterKey(chat)) return null;
    } else if (getProfile(chat.id)?.is_blocked) {
        return null;
    }
    return chat.id;
}

/** The installed app as the composer's grid holds it: its history entry, which knows the version on disk. */
async function startLaunchEntry(app) {
    const path = app.local_path || await checkAppInstalled(app.id).catch(() => null);
    if (!path) return null;
    const history = await invoke('miniapp_get_history', { limit: null }).catch(() => []);
    return history.find(h => h.marketplace_id === app.id && h.src_url === path) || {
        src_url: path, name: app.name, marketplace_id: app.id,
        installed_version: app.update_available ? null : app.version,
        categories: (app.categories || []).join(','),
    };
}

/** Open what a pin key names. */
async function openStartKey(key) {
    const [kind, a, b] = String(key).split('/');
    switch (kind) {
        case 'app': {
            const app = marketplaceApps.find(x => x.id === a);
            if (app && (app.installed || app.local_path)) {
                // In a chat, the composer's choice: alone, or invite the chat.
                const entry = startInviteTarget() && await startLaunchEntry(app);
                return entry ? showMiniAppLaunchDialog(entry) : launchApp(app);
            }
            await showMarketplacePanel();
            return showAppDetailsById(a);
        }
        case 'dm':
            return openChat(a);
        case 'c': {
            if (!startCommunityName(a)) return showToast("You're not in this community");
            const target = b || wsChannelForCommunity(a);
            if (b && !arrChats.some(c => c.id === b)) return showToast("That channel isn't available to you");
            if (target && !chatOnScreen(target)) return openChat(target);
            return;
        }
        case 'settings':
            return executeDeepLinkAction({ action_type: 'settings', target: String(key).split('/').slice(1).join('/') });
        case 'nexus':
            return showMarketplacePanel();
        case 'requests':
            return executeDeepLinkAction({ action_type: 'requests', target: '' });
    }
}

/** A pin's link to share, when it names a place a link can carry. */
function startKeyLink(key) {
    const [kind, a] = String(key).split('/');
    if (kind === 'c') {
        const [, community, channel] = String(key).split('/');
        return goLinkUrl({ community, channel: channel || null, message: null });
    }
    if (kind === 'app') return `${PLACE_BASE}nexus/${a}`;
    if (kind === 'settings' || kind === 'nexus' || kind === 'requests') return PLACE_BASE + key;
    return null;
}

/** The left column: the app's own places. */
function startNavItems() {
    const shell = VectorSvelte.shellState();
    const st = VectorSvelte.startMenuState();
    // More than one account: pick among them in the pins' place. One: straight to adding another.
    const account = st.accounts.length > 1
        ? [{ id: 'accounts', label: 'Accounts', icon: 'users-multi-fill', pane: 'accounts' }]
        : st.canAddAccount ? [{ id: 'add-account', label: 'Accounts', icon: 'add-user', run: () => addAccountFlow.start() }] : [];
    const items = [
        ...account,
        { id: 'profile', label: 'Profile', icon: 'start-profile', run: () => openProfile() },
        { id: 'chat', label: 'Chat', icon: 'chats-fill', run: () => openChatlist(), dot: !!shell.chatBadge },
        { id: 'bookmarks', label: 'Bookmarks', icon: 'bookmark-fill', run: () => openChat(strPubkey) },
        { id: 'miniapps', label: 'Mini Apps', icon: 'grid-fill', run: () => showMiniApps(), key: 'nexus' },
        { id: 'settings', label: 'Settings', icon: 'start-settings', run: () => openSettings(), key: 'settings', dot: !!shell.updateDot },
    ];
    if (shell.invitesTab) items.push({ id: 'invites', label: 'Invites', icon: 'gift', run: () => openInvites() });
    return items;
}

/** This device's accounts, with the Add gate the My Profile switcher applies. */
async function loadStartAccounts() {
    try {
        const [list, tier] = await Promise.all([multiAccount.list(), invoke('get_max_account_tier').catch(() => _maxAccountTier)]);
        _maxAccountTier = tier | 0;
        const canAdd = platformFeatures?.multi_account !== false && list.length < maxAccountsForTier();
        VectorSvelte.setStartAccounts(list, canAdd);
    } catch (e) {
        console.warn('[Start] accounts load failed:', e);
    }
}

/** Switch to another account on this device; the session reloads into it. */
async function startSwitchAccount(npub) {
    try {
        await multiAccount.setActiveAndSwap(npub);
    } catch (e) {
        console.error('[Start] switch failed:', e);
        popupConfirm('Switch failed', escapeHtml(String(e)), true);
    }
}

/** Everything the search can open: the left column's places, pins and what can be pinned. */
function startCandidates(withChannels) {
    const out = [];
    const places = VectorSvelte.settingsScreen().places || {};
    for (const [cat, c] of Object.entries(places)) {
        out.push({ key: `settings/${cat}`, label: c.label, sub: 'Settings', icon: c.icon || 'settings' });
        for (const [sec, label] of Object.entries(c.sections || {})) {
            if (label !== c.label) out.push({ key: `settings/${cat}/${sec}`, label, sub: `Settings › ${c.label}`, icon: c.sectionIcons?.[sec] || c.icon || 'settings' });
        }
    }
    out.push({ key: 'nexus', ...resolveStartKey('nexus') }, { key: 'requests', ...resolveStartKey('requests') });
    const seen = new Set();
    for (const chat of arrChats) {
        const community = communityIdOfChat(chat);
        if (community) {
            if (!seen.has(community)) {
                seen.add(community);
                out.push({ key: `c/${community}`, ...resolveStartKey(`c/${community}`), recent: chat.messages?.[chat.messages.length - 1]?.at || 0 });
            }
            if (withChannels && chat.metadata?.custom_fields?.channel_name) {
                out.push({ key: `c/${community}/${chat.id}`, ...resolveStartKey(`c/${community}/${chat.id}`) });
            }
        } else if (chat.id !== strPubkey && chat.id.startsWith('npub1') && !chatIsRequest(chat) && !getProfile(chat.id)?.is_blocked) {
            out.push({ key: `dm/${chat.id}`, ...resolveStartKey(`dm/${chat.id}`), recent: chat.messages?.[chat.messages.length - 1]?.at || 0 });
        }
    }
    for (const app of marketplaceApps) {
        out.push({ key: `app/${app.id}`, ...resolveStartKey(`app/${app.id}`), installed: !!(app.installed || app.local_path) });
    }
    return out;
}

const START_PROFILE_LINK_RE = /^(?:nostr:|(?:https?:\/\/)?(?:www\.)?vectorapp\.io\/profile\/)?(npub1[a-z0-9]{58})\/?$/;
const startFetchedPeople = new Set();

/** The person a search names by npub or profile link; never someone blocked. */
function startPersonIn(query) {
    const npub = query.toLowerCase().match(START_PROFILE_LINK_RE)?.[1];
    return npub && isNpub(npub) && !getProfile(npub)?.is_blocked ? npub : null;
}

// Once per pasted npub per account, the fetch any unknown profile gets. Its arrival
// touches the profile, which the menu's results read.
function startFetchPerson(npub) {
    const p = getProfile(npub);
    const tag = `${strPubkey}:${npub}`;
    if (p?.name || p?.display_name || startFetchedPeople.has(tag)) return;
    startFetchedPeople.add(tag);
    invoke('load_profile', { npub }).catch(() => {});
}

/**
 * Search: the left column's places and everything pinnable, best matches first. Empty
 * while adding, it suggests: installed apps, then recent conversations, then places.
 * A whole npub (or profile link) finds that person, known or not.
 */
function searchStart(query, { adding = false } = {}) {
    const q = query.toLowerCase();
    const run = (c) => () => openStartKey(c.key);
    const person = q && startPersonIn(q);
    let list;
    if (person) {
        startFetchPerson(person);
        list = [{ key: `dm/${person}`, ...resolveStartKey(`dm/${person}`), sub: person === strPubkey ? 'You' : 'Person', person }];
    } else if (!q) {
        const all = startCandidates(false).filter(c => !startPinned(c.key));
        const apps = all.filter(c => c.app && c.installed);
        const recent = all.filter(c => c.recent).sort((x, y) => y.recent - x.recent).slice(0, 8);
        const places = all.filter(c => !c.app && !c.recent && !c.key.startsWith('dm/'));
        list = [...apps, ...recent, ...places];
    } else {
        // A place that opens a pane in the menu, not a page, has nothing for a result to run.
        const nav = adding ? [] : startNavItems().filter(n => n.run).map(n => ({ ...n, nav: true }));
        const score = (label) => {
            const l = String(label || '').toLowerCase();
            return l.startsWith(q) ? 2 : l.includes(q) ? 1 : 0;
        };
        list = [...nav, ...startCandidates(true)]
            .map(c => ({ c, s: score(c.label) * 2 + (score(c.sub) ? 1 : 0) }))
            .filter(x => x.s > 0)
            .sort((x, y) => y.s - x.s)
            .map(x => x.c);
    }
    // One row per place, keyed by it, so a row that moves keeps its element (and its icon).
    const seen = new Set();
    return list
        .filter(c => { const id = c.nav ? `nav:${c.id}` : c.key; return !seen.has(id) && seen.add(id); })
        .slice(0, 40)
        .map(c => c.nav
            ? { id: `nav:${c.id}`, label: c.label, sub: 'Vector', icon: c.icon, run: c.run }
            : {
                id: c.key, key: c.key, label: c.label, sub: c.sub, app: c.app, src: c.src, round: c.round, initial: c.initial, icon: c.icon,
                pinned: startPinned(c.key),
                person: c.person || null,
                run: c.person ? () => openProfile(getProfile(c.person) || { id: c.person }) : run(c),
                message: c.person ? () => openChat(c.person) : null,
            });
}

/** A tile's own menu. `inFolder` is the open folder's id; `openFolder` shows one, `renameFolder` names it. */
function startTileMenu(x, y, node, inFolder, openFolder, renameFolder) {
    const items = [];
    if (node.type === 'folder') {
        items.push({ label: 'Open', icon: 'folder', onClick: () => openFolder(node.id) });
        items.push({ label: 'Rename', icon: 'edit', onClick: () => renameFolder(node.id) });
        items.push({ label: 'Ungroup', icon: 'grid-fill', onClick: () => startLayoutEdit('start_dissolve_folder', { folderId: node.id }) });
    } else {
        const [kind, appId] = String(node.key).split('/');
        if (kind === 'app') {
            items.push({
                label: 'View Details',
                icon: 'info',
                onClick: async () => {
                    VectorSvelte.setStartOpen(false);
                    await showMarketplacePanel();
                    showAppDetailsById(appId);
                },
            });
        }
        if (inFolder) {
            items.push({ label: 'Move out of Folder', icon: 'chevron-left', onClick: () => startLayoutEdit('start_apply_drop', { source: { kind: 'item', key: node.key }, target: { at: 'end' } }) });
        }
        const link = startKeyLink(node.key);
        if (link) {
            items.push({
                label: 'Copy Link',
                icon: 'share',
                onClick: () => navigator.clipboard.writeText(link).then(() => showToast('Copied Link')).catch(() => showToast('Failed to Copy')),
            });
        }
        items.push({ label: 'Unpin', icon: 'pin', danger: true, onClick: () => startLayoutEdit('start_unpin', { key: node.key }) });
    }
    showContextMenu({ x, y, items });
}

/** Muted and deafened, for every call from now and the one under way. */
async function setMicSwitches(next) {
    const before = { ...VectorSvelte.micState() };
    VectorSvelte.setMicState(next);
    try {
        await invoke('calls_mic_set', next);
    } catch (e) {
        VectorSvelte.setMicState(await invoke('calls_mic_get').catch(() => before));
        showToast(String(e));
    }
}

/** The output or input device picker under the footer's audio buttons. */
async function startDeviceMenu(kind, anchor) {
    await loadAudioDevices();
    const d = VectorSvelte.callAudio().devices;
    const list = kind === 'output' ? d.outputs : d.inputs;
    const current = kind === 'output' ? d.output : d.input;
    const fallback = kind === 'output' ? d.defaultOutput : d.defaultInput;
    const items = [{ header: kind === 'output' ? 'Output Device' : 'Input Device' }];
    items.push({ label: fallback ? `System Default (${fallback})` : 'System Default', checked: !current, onClick: () => setAudioDevice(kind, null) });
    for (const name of list) items.push({ label: name, checked: current === name, onClick: () => setAudioDevice(kind, name) });
    const r = anchor.getBoundingClientRect();
    showContextMenu({ x: r.right, y: r.top - 6, items });
}

/**
 * StartMenuHelpers: what the Main Menu resolves, opens and edits.
 * @typedef {Object} StartMenuHelpers
 * @property {() => void} prepare   on opening: what the pins name, ready to show
 * @property {(query: string) => void} searchNexus   the Nexus, searching for what Start found nothing for
 * @property {(key: string) => object} resolve   what a pin shows
 * @property {(key: string) => Promise<void>} open
 * @property {(query: string, opts: { adding: boolean }) => object[]} search
 * @property {() => object[]} navItems
 * @property {(key: string) => Promise<void>} togglePin
 * @property {(source: object, target: object, folderId: string|null) => Promise<void>} drop
 * @property {(folderId: string, name: string) => Promise<void>} renameFolder
 * @property {(x: number, y: number, node: object, inFolder: string|null, openFolder: (id: string) => void, renameFolder: (id: string) => void) => void} tileMenu
 * @property {(x: number, y: number, item: object) => void} navMenu
 * @property {() => void} closeMenu   a long-press menu giving way to a drag
 * @property {() => object} mkt   the Nexus's helpers, for app icons
 * @property {() => string} myNpubShort
 * @property {() => string} myNpub   the account signed in
 * @property {(meta: object) => string|null} accountSrc   an account's cached avatar, never its remote URL
 * @property {(npub: string) => Promise<void>} switchAccount
 * @property {() => void} addAccount   the Login / Create Account flow
 * @property {() => void} openProfile
 * @property {() => void} toggleMute
 * @property {() => void} toggleDeafen
 * @property {(kind: 'input'|'output', anchor: HTMLElement) => void} deviceMenu
 */
const START_MENU_HELPERS = {
    // The pins as this device holds them (a local read), and the Nexus catalogue, which
    // otherwise loads only with the Nexus, for pinned apps.
    prepare: () => {
        invoke('get_start_layout').then((l) => VectorSvelte.setStartLayout(l)).catch(() => {});
        loadStartAccounts();
        if (marketplaceApps.length) return;
        getCachedMarketplaceApps()
            .then(() => VectorSvelte.setStartLayout(VectorSvelte.startMenuState().layout))
            .catch(() => {});
    },
    searchNexus: async (query) => {
        await showMarketplacePanel();
        VectorSvelte.mktSetQuery(query);
    },
    resolve: resolveStartKey,
    open: openStartKey,
    search: searchStart,
    navItems: startNavItems,
    togglePin: toggleStartPin,
    drop: (source, target) => startLayoutEdit('start_apply_drop', { source, target }),
    renameFolder: (folderId, name) => startLayoutEdit('start_rename_folder', { folderId, name }),
    tileMenu: startTileMenu,
    closeMenu: () => hideContextMenu(),
    navMenu: (x, y, item) => {
        if (!item.key) return;
        const link = startKeyLink(item.key);
        showContextMenu({
            x, y,
            items: [...startPinItems(item.key), ...(link ? [{ label: 'Copy Link', icon: 'share', onClick: () => copyPlaceLink(item.label, item.key) }] : [])],
        });
    },
    mkt: () => VectorSvelte.mktHandlers(),
    myNpubShort: () => (strPubkey ? `${strPubkey.slice(0, 16)}…` : ''),
    myNpub: () => strPubkey,
    accountSrc: (meta) => (meta.avatar_cached ? convertFileSrc(meta.avatar_cached) : null),
    switchAccount: startSwitchAccount,
    addAccount: () => addAccountFlow.start(),
    openProfile: () => openProfile(),
    // Unmuting while deafened hears again too, as in a call.
    toggleMute: () => {
        const m = VectorSvelte.micState();
        setMicSwitches(m.deafened ? { muted: false, deafened: false } : { muted: !m.muted, deafened: false });
    },
    toggleDeafen: () => {
        const m = VectorSvelte.micState();
        setMicSwitches({ muted: m.muted, deafened: !m.deafened });
    },
    deviceMenu: startDeviceMenu,
};

VectorSvelte.setScreen('startMenu', { h: START_MENU_HELPERS });
loadStartNavMode();
