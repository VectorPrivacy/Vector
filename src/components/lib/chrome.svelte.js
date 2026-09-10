// The window chrome: Vector's own title strip on desktop. js/chrome.js decides `on`
// and `mac` once at boot from the platform and keeps `maximized` in step with the window.
const chrome = $state({ on: false, mac: false, maximized: false });
export function chromeState() { return chrome; }
export function setChrome(values) { Object.assign(chrome, values); }
