// The Profile screen's chrome state: whether the My Profile account switcher is open
// (the panel itself lives outside the screen; accounts.js owns it and reports here).
const screen = $state({ switcherOpen: false });

export function profileScreen() { return screen; }
export function setProfileSwitcherOpen(on) { screen.switcherOpen = !!on; }
