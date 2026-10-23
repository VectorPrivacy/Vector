// The signed-in account: the widescreen rail footer's row and the slim header's
// avatar, and the reveal that fades them in.
// profile.js writes it whenever your own profile renders; the row derives from it.
const s = $state({
    visible: false, bookmarks: false, revealTick: 0,
    name: '', hasName: false, statusText: 'Set a Status', emojiTags: [], avatarSrc: null,
});
// Reactive: the row renders before main.js hands these over, and the emoji passes
// must run once they exist.
let handlers = $state.raw({});   // { openProfile, setStatus, switchAccount, openBookmarks, twemojify, renderCustomEmojiShortcodes }

export function accountState() { return s; }
export function accountHandlers() { return handlers; }
export function setAccountHandlers(h) { handlers = h || {}; }
export function setAccount(fields) { Object.assign(s, fields); }
/** Show the row and the bookmarks button, replaying their intro fade. */
export function revealAccount() { s.visible = true; s.bookmarks = true; s.revealTick++; }
