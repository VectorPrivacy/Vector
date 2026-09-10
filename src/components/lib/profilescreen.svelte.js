// The Profile screen's chrome state: whether the My Profile account switcher is open
// (the panel itself lives outside the screen; accounts.js owns it and reports here).
// `ownProfile` is the screen's answer to whose profile is open; the shell's #profile
// container wears it as a class, so the screen never writes to its parent's element.
const screen = $state({ switcherOpen: false, ownProfile: false });

export function profileScreen() { return screen; }
export function setProfileSwitcherOpen(on) { screen.switcherOpen = !!on; }
export function setOwnProfileShown(on) { screen.ownProfile = !!on; }

// The screen's scroll body, for the open that resets it to the top.
const els = { content: null };
export function profileEls() { return els; }
export function bindProfileEl(name) { return (node) => { els[name] = node; return { destroy() { els[name] = null; } }; }; }
