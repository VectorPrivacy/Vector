// The network the account connects through: the backend's last TransportStateView, which
// network's panel Settings shows, I2P's settings, the per-server I2P addresses and the route
// each listed relay or server takes. js/transport.js writes here; the components render.
const t = $state({
    view: null,      // TransportStateView, or null before the first read
    at: 0,           // when `view` arrived (ms): `retry_in` counts down from here
    viewKind: '',    // the network whose panel is on screen; '' follows the one in use
    locked: false,   // a switch or a network setting is being applied
    config: null,    // I2pConfigView, or null until read
    aliases: [],     // AliasView[]
    routes: {},      // lowercased url → RouteView
    stranded: false, // I2P-Only is on and none of the account's relays are inside I2P
    seq: 0,          // config, aliases or routes changed
});

export function transportState() { return t; }
/** The network whose server addresses the dialogs show: only the one in use. Saved ones stay. */
export function aliasKinds() { return t.view?.kind === 'tor' || t.view?.kind === 'i2p' ? [t.view.kind] : []; }

export function setTransportView(view) {
    if (!view) return;
    // Another network in use (a switch, an account change): the panel follows it.
    if (t.view && t.view.kind !== view.kind) t.viewKind = '';
    t.view = view;
    t.at = Date.now();
}
export function setTransportViewKind(kind) { t.viewKind = kind || ''; }
export function setTransportLocked(locked) { t.locked = !!locked; }
export function setI2pConfig(config) { t.config = config || null; t.seq++; }
export function setAliases(list) { t.aliases = Array.isArray(list) ? list : []; t.seq++; }
export function setI2pStranded(on) { t.stranded = !!on; }
export function setRoutes(list) {
    const next = { ...t.routes };
    for (const r of list || []) if (r?.url) next[r.url.toLowerCase()] = r;
    t.routes = next;
    t.seq++;
}

/** The network whose panel is on screen: the user's pick, else the one in use, else the first offered. */
export function viewedKind() {
    if (t.viewKind) return t.viewKind;
    const v = t.view;
    if (!v) return 'clearnet';
    if (v.kind !== 'unknown') return v.kind;
    return v.supported?.[0] || 'clearnet';
}

/** The route `url` takes right now, once read. */
export function routeOf(url) {
    return url ? t.routes[url.toLowerCase()] || null : null;
}

/** Whether this build offers a choice of network (Vector Web and bare builds offer Clearnet only). */
export function routingOffered() {
    const v = t.view;
    return !!v && (v.supported.length > 1 || v.kind !== 'clearnet');
}

/** An I2P address is 52 characters of base32: its ends tell entries apart (`nostra…kn5a.b32.i2p`). */
export function shortI2pHost(text) {
    return (text || '').replace(/([a-z2-7]{6})[a-z2-7]{42,}([a-z2-7]{4})\.b32\.i2p/i, '$1…$2.b32.i2p');
}

/** How long a kind's start may take before it reads as stuck (its start budget, in seconds). */
const STARTUP_SECS = { tor: 120, i2p: 180 };

/**
 * A start nothing drives (no instance reports steps or a bootstrap), or one past its kind's start
 * budget: the user needs a way to try again, and to know why nothing loads.
 */
export function startStuck(view, nowMs) {
    if (!view || view.phase !== 'starting' || view.kind === 'unknown' || view.kind === 'clearnet') return false;
    const driven = view.kind === 'tor' ? view.detail?.status === 'bootstrapping' : (view.steps || []).length > 0;
    return !driven || nowMs / 1000 - view.since > (STARTUP_SECS[view.kind] || 180);
}
