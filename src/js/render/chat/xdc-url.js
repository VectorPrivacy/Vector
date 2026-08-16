// URL-shared Mini Apps: a message containing an .xdc link renders the same
// file-box state machine an uploaded Mini App attachment uses — generic box,
// tap-to-download with progress, snap to the game card. The backend owns the
// fetch (SSRF-guarded, Tor-aware, size-capped); render time never touches the
// network, only the local cache probe.

const XDC_URL_REGEX = /https?:\/\/[^\s"'<>]+\.xdc(?=[?#]|\s|$)/i;

/** account|msg|url -> resolved info, or null when probed-and-absent this
 * session. Keyed per MESSAGE: the topic inside is derived from the message id,
 * so a re-share of the same URL must not inherit another message's session —
 * and per account, so a swap can't reuse the previous account's resolution. */
const xdcUrlResolved = new Map();

function xdcUrlCacheKey(msg, url) {
    return `${strPubkey}|${msg.id}|${url}`;
}

function findXdcUrl(text) {
    const m = (text || '').match(XDC_URL_REGEX);
    return m ? m[0] : null;
}

function xdcUrlHost(url) {
    try { return new URL(url).host; } catch (_) { return ''; }
}

/** The attachment shape the file-box renderer expects, synthesized from a URL. */
function xdcUrlSyntheticAttachment(url, info) {
    const rawName = url.split(/[?#]/)[0].split('/').pop() || 'app.xdc';
    let urlName;
    try { urlName = decodeURIComponent(rawName); } catch (_) { urlName = rawName; }
    return {
        // Pre-download the URL stands in as the id — it keys the progress spinner
        id: info?.hash || url,
        path: info?.path || '',
        extension: 'xdc',
        name: info?.name || urlName,
        downloaded: !!info,
        downloading: false,
        size: 0,
        webxdc_topic: info?.topic || null,
    };
}

/** Render the card for a message's .xdc URL into `target` (async cache probe). */
async function renderXdcUrlCard(target, msg, url) {
    const cacheKey = xdcUrlCacheKey(msg, url);
    let info;
    if (xdcUrlResolved.has(cacheKey)) {
        info = xdcUrlResolved.get(cacheKey);
    } else {
        info = await invoke('miniapp_resolve_url_xdc', { url, msgId: msg.id, download: false }).catch(() => null);
        xdcUrlResolved.set(cacheKey, info);
    }
    // No isConnected guard: rows are built detached and attached after — a
    // cache hit resolves before attachment, and painting a dead row is harmless.
    if (info) {
        // Full parity: the downloaded card IS the attachment renderer
        _dmsgRenderFileAttachment(target, msg, xdcUrlSyntheticAttachment(url, info));
        return;
    }
    const synth = xdcUrlSyntheticAttachment(url, null);
    const { fileDiv, statusSpan } = createFileBox(synth, 'download');
    if (statusSpan) statusSpan.innerText = `Tap to Load · ${xdcUrlHost(url)}`;
    fileDiv.addEventListener('click', () => startXdcUrlDownload(target, msg, url), { once: true });
    target.appendChild(fileDiv);
}

async function startXdcUrlDownload(target, msg, url) {
    const synth = xdcUrlSyntheticAttachment(url, null);
    target.replaceChildren(createFileBox(synth, 'downloading').fileDiv);
    try {
        const info = await invoke('miniapp_resolve_url_xdc', { url, msgId: msg.id, download: true });
        xdcUrlResolved.set(xdcUrlCacheKey(msg, url), info);
        target.replaceChildren();
        _dmsgRenderFileAttachment(target, msg, xdcUrlSyntheticAttachment(url, info));
    } catch (e) {
        xdcUrlResolved.delete(xdcUrlCacheKey(msg, url));
        const { fileDiv, statusSpan } = createFileBox(synth, 'download');
        if (statusSpan) statusSpan.innerText = `Failed: ${String(e).slice(0, 48)} · Tap to Retry`;
        fileDiv.addEventListener('click', () => startXdcUrlDownload(target, msg, url), { once: true });
        target.replaceChildren(fileDiv);
    }
}

// Drive the conical spinner exactly like attachment downloads do — the
// synthetic attachment's pre-download id is the URL, so it keys the lookup
listen('webxdc_url_progress', (evt) => {
    const { url, progress } = evt.payload || {};
    if (!url) return;
    const spinners = document.querySelectorAll(`.miniapp-downloading-spinner[data-attachment-id="${CSS.escape(url)}"]`);
    for (const spinner of spinners) {
        spinner.style.setProperty('--progress', `${progress}%`);
    }
});
