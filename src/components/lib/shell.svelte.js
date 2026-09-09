// The shell: which top-level panes show, which navbar tab is lit, and the chrome flags
// nav toggles. JS nav writes here and the shell components bind to it. Every write is
// flushed synchronously because callers measure layout (adjustSize) right after.
import { flushSync } from 'svelte';

const panes = $state({
    navbar: false, chats: true, chat: false, profile: false, settings: false,
    invites: false, groupOverview: false, chatNew: false, createGroup: false,
});
const shell = $state({ tab: 'chat-btn', invitesTab: false, settingsTab: true, updateDot: false, ws: false });
let handlers = {};   // { openProfile, openChatlist, openSettings, openInvites }

export function shellPanes() { return panes; }
export function shellState() { return shell; }
export function shellHandlers() { return handlers; }
export function setShellHandlers(h) { handlers = h || {}; }

export function showPane(name, on) {
    if (!(name in panes)) throw new Error(`showPane: unknown pane '${name}'`);
    if (panes[name] === !!on) return;
    panes[name] = !!on;
    flushSync();
    for (const fn of paneListeners) fn(name, panes[name]);
}
export function paneShown(name) { return !!panes[name]; }
export function panesSnapshot() { return { ...panes }; }
export function restorePanes(snap) { Object.assign(panes, snap); flushSync(); for (const fn of paneListeners) fn(null, null); }

export function setTab(id) { shell.tab = id; flushSync(); }
export function setShellFlag(key, on) { shell[key] = !!on; flushSync(); }
/** The rail's mail badge text ('' hides it). */
export function setMailBadge(text) { shell.mailBadge = text || ''; }

// The screens the app registers: each entry is the props its component takes, and App
// renders the component once the entry lands. Registered from the script that owns the
// screen's helpers, so a screen appears exactly when its bag is ready.
const screens = $state({ profile: null, settings: null, invites: null, chatNew: null, createGroup: null, chatlist: null, login: null });
export function shellScreens() { return screens; }
export function setScreen(name, props) {
    if (!(name in screens)) throw new Error(`setScreen: unknown screen '${name}'`);
    screens[name] = props;
    flushSync();
}

// One-shot entrance animations: a tick per surface; the `reveal` action adds the class
// and drops it on animationend. `revealPane` resolves when that end fires, for the
// flows that sequence on it (the login fade-out).
const reveals = $state({ profile: 0, chats: 0, chat: 0, groupOverview: 0, navbar: 0, chatList: 0, newChat: 0, login: 0 });
const revealActive = $state({});
const revealWaiters = new Map();
export function shellReveals() { return reveals; }
export function revealPending(name) { return !!revealActive[name]; }
export function revealPane(name, cls = 'fadein-anim') {
    if (!(name in reveals)) throw new Error(`revealPane: unknown surface '${name}'`);
    return new Promise((resolve) => {
        revealWaiters.set(name, resolve);
        reveals[name] = { n: (reveals[name]?.n || 0) + 1, cls };
        flushSync();
    });
}
/** use:reveal={[name, tick]} on the surface: plays the tick's class, settles the waiter. */
export function reveal(node, [name, tick]) {
    const play = (t) => {
        if (!t) return;
        node.classList.add(t.cls);
        revealActive[name] = true;
        node.addEventListener('animationend', () => {
            node.classList.remove(t.cls);
            revealActive[name] = false;
            revealWaiters.get(name)?.();
            revealWaiters.delete(name);
        }, { once: true });
    };
    play(tick);
    return { update: ([, t]) => play(t) };
}

// The chat list's sync line: the centre reveal (`active`), the determinate fill and the
// retract that keeps the fill width until the line has shrunk.
const sync = $state({ active: false, fadeOut: false, progress: null });
export function syncLineState() { return sync; }
export function setSyncLine(patch) { Object.assign(sync, patch); }

// Pane changes fan out to the layout controllers that used to observe the elements.
const paneListeners = new Set();
export function onPaneChange(fn) { paneListeners.add(fn); return () => paneListeners.delete(fn); }

// The few elements the layout math measures, bound by the shell components.
const shellEls = $state.raw({ chatList: null, navbar: null, newChat: null, profile: null });
export function shellElements() { return shellEls; }
export function bindShellEl(name) { return (node) => { shellEls[name] = node; return { destroy() { shellEls[name] = null; } }; }; }
