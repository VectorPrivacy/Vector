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
 * @property {(clicks: number) => void} dragStart
 */
function chromeWindow() { return window.__TAURI__.window.getCurrentWindow(); }

/* macOS animates zoom, and the webview only re-lays out once the animation ends,
   so the UI sits cut off for its whole length. Maximize there is done by hand: an
   instant jump to the work area, with the frame it left kept for the way back. */
const chromeManualMaximize = /Macintosh/.test(navigator.userAgent);
let chromeRestore = null;

async function chromeIsMaximized() {
    return chromeManualMaximize ? !!chromeRestore : chromeWindow().isMaximized();
}

async function chromeToggleMaximize() {
    const w = chromeWindow();
    if (!chromeManualMaximize) { await w.toggleMaximize(); return; }
    if (chromeRestore) {
        const { pos, size } = chromeRestore;
        chromeRestore = null;
        await w.setSize(size);
        await w.setPosition(pos);
    } else {
        const m = await window.__TAURI__.window.currentMonitor();
        if (!m) return;
        chromeRestore = { pos: await w.outerPosition(), size: await w.outerSize() };
        const area = m.workArea || { position: m.position, size: m.size };
        const { PhysicalPosition, PhysicalSize } = window.__TAURI__.dpi;
        await w.setPosition(new PhysicalPosition(area.position.x, area.position.y));
        await w.setSize(new PhysicalSize(area.size.width, area.size.height));
    }
    VectorSvelte.setChrome({ maximized: !!chromeRestore });
}

/** One drag surface: a press moves the window, a double-press toggles maximize. */
function chromeDragStart(clicks) {
    if (clicks === 2) chromeToggleMaximize();
    else chromeWindow().startDragging();
}

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
            toggleMaximize: chromeToggleMaximize,
            close: () => chromeWindow().close(),
            dragStart: chromeDragStart,
        },
    });
    // A hand-maximized window that gets dragged or resized is no longer maximized.
    const syncMaximized = async () => {
        if (chromeRestore) {
            const m = await window.__TAURI__.window.currentMonitor();
            const size = await chromeWindow().outerSize();
            const area = m?.workArea?.size || m?.size;
            if (!area || size.width !== area.width || size.height !== area.height) chromeRestore = null;
        }
        VectorSvelte.setChrome({ maximized: await chromeIsMaximized() });
    };
    chromeWindow().onResized(syncMaximized);
    syncMaximized();
}

chromeInit();
