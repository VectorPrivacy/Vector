// The network the account connects through: Clearnet, Tor or I2P. Keeps the transport store
// current (the backend's `transport_state` events, a poll while something is starting), and
// holds the switch, I2P's settings, per-server I2P addresses and the routes the relay list
// shows. Settings, the welcome screen, the network dialogs and the chat list render from it.

const TRANSPORT_LABELS = { clearnet: 'Clearnet', tor: 'Tor', i2p: 'I2P' };

/** The core's I2P-Only refusal, which the relay list words as a status of its own. */
const I2P_ONLY_REFUSAL = 'I2P-Only is on, so this server is off.';

let _transportPoll = null;
let _transportConfigSeq = null;
const _transportRouteUrls = new Set();
let _transportRoutesTimer = null;
const _transportWaiters = new Set();

function transportLabel(kind) {
    return TRANSPORT_LABELS[kind] || kind;
}

/** The card's status line for the network in use. */
function formatTransportStatus(view) {
    if (!view) return '';
    if (view.kind === 'unknown') return 'Choose how Vector connects.';
    if (view.phase === 'unsupported') return view.reason?.text || `This build doesn't include ${transportLabel(view.kind)}.`;
    if (view.kind === 'clearnet') return 'Connects straight to relays and servers.';
    if (view.kind === 'tor') {
        if (view.ready) return 'Connected.';
        const d = view.detail || {};
        if (d.status === 'bootstrapping') return Number.isFinite(d.bootstrap_progress) ? `Bootstrapping ${d.bootstrap_progress}%…` : 'Bootstrapping…';
        return view.phase === 'failed' ? 'Failed to start.' : 'Starting…';
    }
    if (view.ready) return transportExitFailing(view) ? 'Connected inside I2P only.' : 'Connected.';
    if (view.phase === 'starting') {
        const router = (view.steps || []).find((s) => s.id === 'router');
        if (view.kind !== 'i2p' || !router) return 'Starting…';
        return router.state !== 'ok' ? 'Looking for your router…' : 'Building tunnels…';
    }
    return view.reason?.text || 'Starting…';
}

/** I2P is up but no outproxy answers: only I2P servers are reachable. */
function transportExitFailing(view) {
    return !!view?.ready && (view.steps || []).some((s) => s.id === 'exit' && s.state === 'fail');
}

/** The glyph's state for `kind` as shown: only the network in use is ever lit. */
function transportStateClass(view, kind) {
    if (!view || view.kind !== kind || view.phase === 'unsupported') return 'tor-state-disabled';
    if (view.ready) return 'tor-state-connected';
    // A lost I2P session rebuilds on its own: that reads as connecting, not as a failure.
    if (view.phase === 'starting' || view.reason?.code === 'session_lost') return 'tor-state-bootstrapping';
    return 'tor-state-failed';
}

function isTransportTransitional(view) {
    return !!view && view.phase === 'starting';
}

/** The relay list's word for a route that can't connect on the network in use, or null. */
function transportRouteTag(route) {
    if (!route) return null;
    if (route.class === 'blocked') {
        // A network that is still starting holds traffic as a matter of course: not an error.
        if (isTransportTransitional(VectorSvelte.transportState().view)) return { cls: 'connecting', text: 'Connecting', title: route.last_error || route.text };
        return { cls: 'blocked', text: 'Blocked', title: route.last_error || route.text };
    }
    if (route.class !== 'refused') return null;
    if (route.text === I2P_ONLY_REFUSAL) return { cls: 'refused', text: 'Off in I2P-Only', short: 'I2P-Only', title: route.text };
    // An .onion is out of every network's reach in this build: no tag may promise Tor.
    const needs = /\.i2p$/i.test(route.host) ? 'I2P' : '';
    return { cls: 'refused', text: needs ? `Needs ${needs}` : 'Unreachable', title: route.text };
}

/** A mark beside a row that connects: it rides its I2P address, or the one saved for it failed its check. */
function transportRouteBadge(route) {
    if (!route) return null;
    if (route.class === 'twin') return { cls: 'twin', text: 'I2P', title: route.text };
    if (route.kind !== 'i2p' || route.class !== 'exit') return null;
    const alias = VectorSvelte.transportState().aliases.find((a) => a.host === route.host);
    if (!alias?.twins?.i2p || alias.check?.state !== 'failed') return null;
    return { cls: 'failed', text: 'I2P address failed', title: alias.check.text || route.text };
}

/** Take a view the backend sent or answered with, and everything that follows from it. */
function applyTransportView(view) {
    if (!view || !view.kind) return;
    const prev = VectorSvelte.transportState().view;
    VectorSvelte.setTransportView(view);
    if (view.kind !== 'tor' || !view.ready) forgetTorCircuits();
    if (view.config_seq !== _transportConfigSeq) {
        _transportConfigSeq = view.config_seq;
        loadTransportConfig();
    }
    if (!prev || prev.kind !== view.kind || prev.ready !== view.ready || prev.phase !== view.phase || prev.config_seq !== view.config_seq) {
        refreshTransportRoutesSoon();
    }
    for (const waiter of [..._transportWaiters]) waiter(view);
    loginNetSync(view);
    if (isTransportTransitional(view)) ensureTransportPolling();
    if (!prev || prev.kind !== view.kind || prev.ready !== view.ready || prev.config_seq !== view.config_seq) checkI2pStranded();
}

/** Whether `relay` (a RelayInfo) is reachable inside I2P: an .i2p host, or one with a working I2P address. */
function relayOnI2p(relay) {
    let host = '';
    try { host = new URL(relay.url).hostname.toLowerCase(); } catch { return false; }
    if (host.endsWith('.i2p')) return true;
    const alias = VectorSvelte.transportState().aliases.find((a) => a.host === host);
    return !!alias?.twins?.i2p && alias.check?.state !== 'failed';
}

/** The account's enabled relays, or null when they can't be read. */
async function enabledRelays() {
    try { return (await invoke('get_relays')).filter((r) => r.enabled); } catch { return null; }
}

/** I2P-Only with none of the account's relays inside I2P: nothing sends or arrives. */
async function checkI2pStranded() {
    const s = VectorSvelte.transportState();
    if (s.view?.kind !== 'i2p' || s.config?.exit !== 'off') { VectorSvelte.setI2pStranded(false); return; }
    const relays = await enabledRelays();
    VectorSvelte.setI2pStranded(!!relays && !relays.some(relayOnI2p));
}

async function fetchTransportView() {
    try {
        const view = await invoke('transport_get_state');
        applyTransportView(view);
        return view;
    } catch (e) {
        console.warn('[Transport] transport_get_state failed:', e);
        return VectorSvelte.transportState().view;
    }
}

/** While a network starts or a change is applied, re-read every 1.5 s; events usually beat it. */
function ensureTransportPolling() {
    if (_transportPoll) return;
    _transportPoll = setInterval(async () => {
        const view = await fetchTransportView();
        if (!VectorSvelte.transportState().locked && !isTransportTransitional(view)) {
            clearInterval(_transportPoll);
            _transportPoll = null;
        }
    }, 1500);
}

/**
 * Resolves true once `kind` is connected, false once it reports why it can't be (or `ms` pass):
 * I2P returns from its start at once and connects in the background.
 * @param {string} kind
 * @param {number} ms
 * @returns {Promise<boolean>}
 */
function waitForTransport(kind, ms) {
    return new Promise((resolve) => {
        let timer = 0;
        const settle = (ok) => {
            clearTimeout(timer);
            _transportWaiters.delete(check);
            resolve(ok);
        };
        const check = (view) => {
            if (!view || view.kind === 'unknown') return;
            // Another network took over: this one won't connect now.
            if (view.kind !== kind) settle(false);
            else if (view.ready) settle(true);
            // A lost I2P session rebuilds itself; anything else waiting needs the user.
            else if (view.phase !== 'starting' && view.reason?.code !== 'session_lost') settle(false);
        };
        timer = setTimeout(() => settle(false), ms);
        _transportWaiters.add(check);
        // The backend's view, not the store's: a command that just switched may not have painted it.
        fetchTransportView();
        ensureTransportPolling();
    });
}

/** I2P's settings and the per-server I2P addresses, where this build has I2P. */
async function loadTransportConfig() {
    const view = VectorSvelte.transportState().view;
    if (!view?.supported?.includes('i2p')) return;
    try { VectorSvelte.setI2pConfig(await invoke('i2p_get_config')); } catch (e) { console.warn('[Transport] i2p_get_config failed:', e); }
    await loadTransportAliases();
    checkI2pStranded();
}

async function loadTransportAliases() {
    try { VectorSvelte.setAliases(await invoke('transport_get_aliases')); } catch (e) { console.warn('[Transport] transport_get_aliases failed:', e); }
}

/** Read the route each of `urls` takes now; they are re-read whenever the network changes. */
async function refreshTransportRoutes(urls) {
    for (const u of urls) _transportRouteUrls.add(u);
    if (!urls.length) return;
    try {
        VectorSvelte.setRoutes(await invoke('transport_get_routes', { urls }));
    } catch (e) {
        console.warn('[Transport] transport_get_routes failed:', e);
    }
}

function refreshTransportRoutesSoon() {
    clearTimeout(_transportRoutesTimer);
    _transportRoutesTimer = setTimeout(() => refreshTransportRoutes([..._transportRouteUrls]), 250);
}

/**
 * Switch the account to `kind`. A Mini App session joined outside the network is kept or ended
 * as the user answers, before anything changes.
 * @param {string} kind
 */
async function useTransportKind(kind) {
    const s = VectorSvelte.transportState();
    const view = s.view;
    if (s.locked || !view) return;
    // Held from the first click: the router check can take seconds, and a second click must not
    // stack a second prompt behind the first.
    VectorSvelte.setTransportLocked(true);
    try {
        // I2P needs a router the user runs: switching without one would cut every connection off.
        if (kind === 'i2p' && s.config?.sam_port) {
            const port = s.config.sam_port;
            let r = null;
            try { r = await testI2pRouter(port); } catch { r = null; }
            if (!r?.ok) {
                const why = !r?.text || r.text.startsWith('No router answered') ? `No I2P router answered on port ${port}.` : r.text;
                if (!(await popupConfirm('Switch to I2P?', `${escapeHtml(why)} Switch anyway?`, false, '', '', '', 'Switch'))) return;
            }
        }
        let keepRealtime;
        if (view.realtime_active && kind !== 'clearnet') {
            const label = escapeHtml(transportLabel(kind));
            const answer = await popupConfirm(
                'Keep your Mini App session?',
                `It connects directly, outside ${label}.<br>End it to keep everything on ${label}.`,
                false, '', '', '', 'Keep', false, null, 'End', true,
            );
            // Dismissed (Escape, back): no switch at all.
            if (answer === null) return;
            keepRealtime = !!answer;
        }
        ensureTransportPolling();
        try {
            applyTransportView(await invoke('transport_set', { kind, keepRealtime }));
        } catch (err) {
            const now = await fetchTransportView();
            // A network that failed to start says so on its own card.
            if (!(now && now.kind === kind && now.reason)) showToast(String(err));
        }
    } finally {
        VectorSvelte.setTransportLocked(false);
        fetchTransportView();
    }
}

/** Try the network in use again now: I2P skips its wait for the router, Tor starts over. */
async function retryTransport() {
    if (VectorSvelte.transportState().locked) return;
    VectorSvelte.setTransportLocked(true);
    ensureTransportPolling();
    try {
        applyTransportView(await invoke('transport_retry', {}));
    } catch (err) {
        showToast(String(err));
    } finally {
        VectorSvelte.setTransportLocked(false);
    }
}

/** Fresh I2P addresses for this account; every server sees the new ones from now on. */
async function renewTransportIdentity() {
    try {
        const view = await invoke('transport_retry', { newIdentity: true });
        applyTransportView(view);
        // The new sessions take a while to build: the toast waits until they are in use.
        if (view?.kind === 'i2p' && (await waitForTransport('i2p', 180000))) showToast('New I2P address in use.');
    } catch (err) {
        showToast(String(err));
    }
}

/**
 * Save the I2P router. Leaving `user` out keeps the saved SAM password; empty strings remove it.
 * @param {number} port
 * @param {string} [user]
 * @param {string} [password]
 */
async function setI2pRouter(port, user, password) {
    const args = user === undefined ? { port } : { port, user, password };
    VectorSvelte.setTransportLocked(true);
    try {
        applyTransportView(await invoke('i2p_set_router', args));
    } finally {
        VectorSvelte.setTransportLocked(false);
        await loadTransportConfig();
    }
}

/** Re-read the state every 5 s while I2P's settings are on screen; returns the stop. */
function watchI2pStatus() {
    const timer = setInterval(fetchTransportView, 5000);
    return () => clearInterval(timer);
}

/** Ask the router on `port` whether it speaks SAM, without opening a session. */
function testI2pRouter(port, user, password) {
    return invoke('i2p_test_router', user === undefined ? { port } : { port, user, password });
}

/**
 * Turn I2P-Only on or off. Turning it on with none of the account's relays inside I2P asks first.
 * @param {boolean} on
 * @returns {Promise<boolean>} false when the user backed out
 */
async function setI2pOnly(on) {
    if (on) {
        const relays = await enabledRelays();
        if (relays && !relays.some(relayOnI2p)
            && !(await popupConfirm('Turn on I2P-Only?', 'None of your relays are on I2P. Turn on I2P-Only anyway?', false, '', '', '', 'Turn On'))) {
            return false;
        }
    }
    VectorSvelte.setTransportLocked(true);
    try {
        applyTransportView(await invoke('i2p_set_exit', { mode: on ? 'off' : 'allow' }));
    } finally {
        VectorSvelte.setTransportLocked(false);
        await loadTransportConfig();
    }
    return true;
}

/** Save the outproxy list in failover order; `null` restores the built-in one. */
async function saveI2pOutproxies(list) {
    VectorSvelte.setI2pConfig(await invoke('i2p_set_outproxies', { list }));
}

/**
 * TransportAliasHandlers: a server's I2P address, in the relay and media server dialogs.
 * @typedef {{ help(key: string): void, save(host: string, address: string | null): Promise<object | null>,
 *   find(url: string): Promise<object>, check(host: string): Promise<object> }} TransportAliasHandlers
 */
/** @type {TransportAliasHandlers} */
const TRANSPORT_ALIAS_HANDLERS = {
    help: (key) => showSettingsHelp(key),
    save: async (host, address) => {
        try { return await invoke('transport_set_alias', { host, kind: 'i2p', address }); } finally { await loadTransportAliases(); }
    },
    find: async (url) => {
        try { return await invoke('transport_find_alias', { url }); } finally { await loadTransportAliases(); }
    },
    check: async (host) => {
        try { return await invoke('transport_check_alias', { host }); } finally { await loadTransportAliases(); }
    },
};

/**
 * TransportHandlers: Settings > Privacy > Routing (the picker, the card, I2P's panel).
 * @typedef {{ selectView(kind: string): void, useKind(kind: string): Promise<void>, retry(): Promise<void>,
 *   newIdentity(): Promise<void>, help(key: string): void, openLink(key: string): void,
 *   injectGlyph(svg: SVGElement): void, stateClass(view: object, kind: string): string, formatStatus(view: object): string,
 *   isTransitional(view: object): boolean, label(kind: string): string, openNetwork(): void,
 *   i2p: { load(): Promise<void>, watch(): () => void, setRouter(port: number, user?: string, password?: string): Promise<void>,
 *   testRouter(port: number, user?: string, password?: string): Promise<object>, setI2pOnly(on: boolean): Promise<boolean>,
 *   saveOutproxies(list: object[] | null): Promise<void>, confirmReset(own: boolean): Promise<boolean>,
 *   confirmRemove(name: string): Promise<boolean>, copyAddress(b32: string): void } }} TransportHandlers
 */
/** @type {TransportHandlers} */
const TRANSPORT_HANDLERS = {
    selectView: (kind) => VectorSvelte.setTransportViewKind(kind),
    useKind: useTransportKind,
    retry: retryTransport,
    newIdentity: renewTransportIdentity,
    help: (key) => showSettingsHelp(key),
    openLink: (key) => openUrl(SETTINGS_LINKS[key]),
    injectGlyph: (svg) => injectTorGlyph(svg),
    stateClass: transportStateClass,
    formatStatus: formatTransportStatus,
    isTransitional: isTransportTransitional,
    label: transportLabel,
    openNetwork: () => VectorSvelte.requestSettingsScroll('relays'),
    i2p: {
        load: loadTransportConfig,
        watch: watchI2pStatus,
        setRouter: setI2pRouter,
        testRouter: testI2pRouter,
        setI2pOnly,
        saveOutproxies: saveI2pOutproxies,
        confirmReset: (own) => popupConfirm('Use the built-in outproxies?',
            own ? 'Your own outproxies are removed.' : 'The built-in order comes back.', false, '', '', '', 'Reset'),
        confirmRemove: (name) => popupConfirm(`Remove ${name}?`, 'Vector stops using this outproxy.', false, '', '', '', 'Remove'),
        copyAddress: (b32) => navigator.clipboard.writeText(b32).then(() => showToast('Address copied'), () => {}),
    },
};

/** Settings at Routing, on the panel of the network in use (the one a notice is about). */
function openRoutingSettings() {
    if (VectorSvelte.loginUp()) return;
    VectorSvelte.setTransportViewKind('');
    openSettings();
    VectorSvelte.requestSettingsScroll('routing');
}

document.addEventListener('DOMContentLoaded', () => {
    listen('transport_state', (e) => applyTransportView(e.payload));
    VectorSvelte.mergeShellHandlers({ openRouting: openRoutingSettings });
    fetchTransportView();
}, { once: true });
