/**
 * The desktop window chrome (src/components/shell/WindowChrome.svelte).
 *
 * Decided from the user agent rather than get_platform_features: that answer
 * arrives after login, and the strip has to be in place for the first paint.
 * Desktop windows are undecorated (tauri.conf.json), so the strip is the only
 * place the window controls exist.
 */

const chromeOn = !/Android|iPhone|iPad/i.test(navigator.userAgent);

/**
 * ChromeHelpers: what the strip's controls do.
 * @typedef {Object} ChromeHelpers
 * @property {() => void} openDmHome
 * @property {() => void} openUpdates
 * @property {() => void} openHelp
 * @property {() => void} minimize
 * @property {() => void} toggleMaximize
 * @property {() => void} close
 */
function chromeWindow() { return window.__TAURI__.window.getCurrentWindow(); }

function chromeInit() {
    document.body.classList.toggle('chrome', chromeOn);
    VectorSvelte.setChrome({ on: chromeOn });
    if (!chromeOn) return;
    VectorSvelte.setScreen('chrome', {
        h: {
            openDmHome: () => wsOpenDmHome(),
            openUpdates: () => { openSettings(); VectorSvelte.requestSettingsScroll('updates'); },
            openHelp: () => openUrl('https://docs.vectorapp.io'),
            minimize: () => chromeWindow().minimize(),
            toggleMaximize: () => chromeWindow().toggleMaximize(),
            close: () => chromeWindow().close(),
        },
    });
    const syncMaximized = async () => {
        VectorSvelte.setChrome({ maximized: await chromeWindow().isMaximized() });
    };
    chromeWindow().onResized(syncMaximized);
    syncMaximized();
}

chromeInit();
