// The window chrome: Vector's own title strip on desktop. js/chrome.js decides `on`
// once at boot from the platform and keeps `maximized` in step with the window.
const chrome = $state({ on: false, maximized: false });
export function chromeState() { return chrome; }
export function setChrome(values) { Object.assign(chrome, values); }
