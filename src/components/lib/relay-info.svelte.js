// What each relay says of itself (NIP-11), by URL: { name, iconSrc, auth }, null while it loads.
// Every relay list reads the same map, so a relay is looked up once.
const info = $state({});

export function relayInfoState() { return info; }
export function setRelayInfo(url, value) { info[url] = value; }
