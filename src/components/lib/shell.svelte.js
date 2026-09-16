// The shell: which top-level panes show, which navbar tab is lit, and the chrome flags
// nav toggles. JS nav writes here and the shell components bind to it. Every write is
// flushed synchronously because callers measure layout (adjustSize) right after.
import { flushSync } from 'svelte';

const panes = $state({
    navbar: false, chats: true, chat: false, profile: false, settings: false,
    invites: false, groupOverview: false, chatNew: false, createGroup: false,
});
// Every key the panes bind to, declared up front: setShellFlag takes any name, so a key
// that first appears in a setter call is invisible to a reader of this file.
const shell = $state({ tab: 'chat-btn', invitesTab: false, settingsTab: true, updateDot: false, chatBadge: '', newChatButtons: false, ws: false, railCollapsed: false, railLocked: false });
// Registered by two owners: main.js (the nav actions) and widescreen.js (the list
// resizer), so writes must merge. { openProfile, openChatlist, openSettings, openInvites,
// openNewChat, openCreateGroup, listResizeStart, listResizeReset }
let handlers = $state.raw({});

export function shellPanes() { return panes; }
export function shellState() { return shell; }
export function shellHandlers() { return handlers; }
export function mergeShellHandlers(partial) { handlers = { ...handlers, ...partial }; }

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
export function setShellFlag(key, on) {
    if (!(key in shell)) throw new Error(`setShellFlag: unknown flag '${key}'`);
    shell[key] = !!on;
    flushSync();
}
/** The rail's mail badge text ('' hides it). */
export function setChatBadge(text) { shell.chatBadge = text || ''; }

// The screens the app registers: each entry is the props its component takes, and App
// renders the component once the entry lands. Registered from the script that owns the
// screen's helpers, so a screen appears exactly when its bag is ready.
const screens = $state({ profile: null, settings: null, invites: null, chatNew: null, createGroup: null, chatlist: null, login: null,
    communityHead: null, rail: null, chrome: null, overview: null, roster: null, packDetails: null,
    // Body-level singletons whose helpers live in the vanilla side; App renders each once registered.
    composerPopups: null, filePreview: null, miniProfile: null, reactionPopups: null, editHistory: null,
    qrOverlay: null, qrScanner: null, statusDialog: null, downgradeBlock: null,
    modConsole: null, policyDesigner: null, pivx: null, network: null });
export function shellScreens() { return screens; }
export function setScreen(name, props) {
    if (!(name in screens)) throw new Error(`setScreen: unknown screen '${name}'`);
    screens[name] = props;
    flushSync();
}

// One-shot entrance animations: a tick per surface; the `reveal` action adds the class
// and drops it on animationend. `revealPane` resolves when that end fires, for the
// flows that sequence on it (the login fade-out).
const reveals = $state({ profile: null, chats: null, chat: null, groupOverview: null, navbar: null, chatList: null, newChat: null, login: null });   // { n, cls } once revealed
const revealActive = $state({});
const revealWaiters = new Map();
export function shellReveals() { return reveals; }
export function revealPending(name) { return !!revealActive[name]; }
export function revealPane(name, cls = 'fadein-anim') {
    if (!(name in reveals)) throw new Error(`revealPane: unknown surface '${name}'`);
    return new Promise((resolve) => {
        // A second reveal before the first settles takes its place; the first must not hang.
        revealWaiters.get(name)?.();
        // An animation that never ends (a hidden surface, an occluded webview) must not hang either.
        const failsafe = setTimeout(() => { if (revealWaiters.get(name) === settle) settle(); }, 600);
        const settle = () => { clearTimeout(failsafe); revealWaiters.delete(name); resolve(); };
        revealWaiters.set(name, settle);
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

// Pane changes fan out to the layout controllers, which measure these).
const paneListeners = new Set();
export function onPaneChange(fn) { paneListeners.add(fn); return () => paneListeners.delete(fn); }

// The few elements the layout math measures, bound by the shell components.
const shellEls = { chatList: null, chats: null, chat: null, navbar: null, newChat: null, profile: null };
export function shellElements() { return shellEls; }
export function bindShellEl(name) { return (node) => { shellEls[name] = node; return { destroy() { shellEls[name] = null; } }; }; }
