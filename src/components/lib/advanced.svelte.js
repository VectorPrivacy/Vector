// Advanced Mode: an account setting synced across its devices that reveals identifiers
// and other developer-facing detail, such as Copy ID on communities, channels and messages.
const s = $state({ on: false });

export function advancedState() { return s; }
export function setAdvancedMode(on) { s.on = !!on; }
