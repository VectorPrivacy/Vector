// Nostr posts, articles and videos referenced in a message, shown as cards when web previews
// are on. Only the backend fetches: the event from relays, its pictures and video into local
// caches. One global scope: loads after message-row.js and pack-previews.js.

const NOSTR_EMBED_KINDS = new Set([1, 30023, 21, 22, 34235, 34236, 6, 16]);
const NOSTR_EMBEDS_PER_MSG = 3;
const NOSTR_EMBED_ERR_TTL_MS = 30000;
const NOSTR_ARTICLE_MAX_IMAGES = 50;
const NOSTR_EMBED_CACHE_MAX = 300;

const _NOSTR_ENTITY = '(?:note|nevent|naddr)1[ac-hj-np-z02-9]{20,}';
const _NOSTR_ENTITY_ONLY = new RegExp(`^${_NOSTR_ENTITY}$`, 'i');
const _NOSTR_URL_RE = /https:\/\/[^\s<>"'`{}|\\^\[\]]+/gi;
// Bare or nostr:-prefixed, at a text boundary. Lookahead only: WKWebView before Safari 16.4
// can't parse a lookbehind, and that SyntaxError would take the whole script down.
const _NOSTR_REF_RE = new RegExp(`(^|[\\s([{<"'])(nostr:)?(${_NOSTR_ENTITY})(?=$|[\\s)\\]}>"'.,;:!?])`, 'gi');
const _NOSTR_CODE_RE = /```[\s\S]*?```|~~~[\s\S]*?~~~|`[^`\n]*`/g;

const _nostrScanCache = new Map();
const _nostrKindCache = new Map();

/**
 * The event kind a nevent or naddr names (TLV type 3), or null when it names none (a note,
 * a nevent without the hint) or doesn't decode. The checksum is the backend's to judge.
 */
function _nostrRefKind(entity) {
    const key = entity.toLowerCase();
    if (_nostrKindCache.has(key)) return _nostrKindCache.get(key);
    let kind = null;
    if (!key.startsWith('note1')) {
        const data = key.slice(key.indexOf('1') + 1, -6);
        const bytes = [];
        let acc = 0, bits = 0;
        for (const ch of data) {
            const v = _BECH32_CHARSET.indexOf(ch);
            if (v === -1) { bytes.length = 0; break; }
            acc = ((acc << 5) | v) & 0xffff;
            bits += 5;
            if (bits >= 8) {
                bits -= 8;
                bytes.push((acc >> bits) & 0xff);
            }
        }
        for (let i = 0; i + 2 <= bytes.length;) {
            const t = bytes[i], len = bytes[i + 1];
            if (i + 2 + len > bytes.length) break;
            if (t === 3 && len === 4) {
                kind = ((bytes[i + 2] << 24) | (bytes[i + 3] << 16) | (bytes[i + 4] << 8) | bytes[i + 5]) >>> 0;
                break;
            }
            i += 2 + len;
        }
    }
    _nostrKindCache.set(key, kind);
    return kind;
}

/** Whether a reference can be a post, article or video: an naddr must say so, the others may not. */
function _nostrRefSupported(entity) {
    const kind = _nostrRefKind(entity);
    if (kind === null) return !entity.toLowerCase().startsWith('naddr1');
    return NOSTR_EMBED_KINDS.has(kind);
}

/** The reference a web link ends in (njump, primal, habla and the like), or null. */
function _nostrEntityFromUrl(url) {
    const path = url.slice('https://'.length).split(/[?#]/)[0];
    const slash = path.indexOf('/');
    if (slash === -1) return null;
    let last = path.slice(slash + 1).replace(/\/+$/, '').split('/').pop() || '';
    last = last.replace(/\.html$/i, '').replace(/^nostr:/i, '');
    return _NOSTR_ENTITY_ONLY.test(last) ? last : null;
}

function _nostrBlank(text, re) {
    re.lastIndex = 0;
    return text.replace(re, (m) => ' '.repeat(m.length));
}

/**
 * The first few distinct references in `text`, in order, each with where it sits so the bare
 * ones can be cut. Code is never scanned, and an entity inside a web link belongs to the link.
 * @returns {{ key: string, entity: string, url: string|null, start: number, end: number }[]}
 */
function _nostrScan(text) {
    const hit = _nostrScanCache.get(text);
    if (hit) return hit;
    const found = [];
    const noCode = _nostrBlank(text, _NOSTR_CODE_RE);
    _NOSTR_URL_RE.lastIndex = 0;
    for (const m of noCode.matchAll(_NOSTR_URL_RE)) {
        const url = m[0].replace(/[.,;:!?)\]}'"]+$/, '');
        // `<https://…>` is the no-preview form.
        if (noCode[m.index - 1] === '<' && noCode[m.index + m[0].length] === '>') continue;
        const entity = _nostrEntityFromUrl(url);
        if (entity) found.push({ entity, url, start: m.index, end: m.index });
    }
    const bare = _nostrBlank(noCode, _NOSTR_URL_RE);
    _NOSTR_REF_RE.lastIndex = 0;
    for (const m of bare.matchAll(_NOSTR_REF_RE)) {
        // A markdown link's target, `[label](nostr:…)`: cutting it would leave `[label]()`.
        if (m[1] === '(' && bare[m.index - 1] === ']') continue;
        const start = m.index + m[1].length;
        const end = start + (m[2] || '').length + m[3].length;
        // A link's label, `[nostr:…](https://…)`: cutting it would leave an empty link.
        if (bare[end] === ']' && bare[end + 1] === '(') continue;
        found.push({ entity: m[3], url: null, start, end });
    }
    found.sort((a, b) => a.start - b.start);
    const refs = [];
    const seen = new Set();
    for (const f of found) {
        const key = f.entity.toLowerCase();
        if (seen.has(key) || !_nostrRefSupported(f.entity)) continue;
        seen.add(key);
        refs.push({ key, ...f });
        if (refs.length === NOSTR_EMBEDS_PER_MSG) break;
    }
    _nostrScanCache.set(text, refs);
    if (_nostrScanCache.size > 500) _nostrScanCache.delete(_nostrScanCache.keys().next().value);
    return refs;
}

/** The references in a message that get a card. */
function nostrEmbedRefs(text) {
    return text ? _nostrScan(text) : [];
}

/** Cut the bare and nostr: references the cards stand in for; web links stay as written. */
function stripNostrEmbedRefs(text) {
    if (!text) return text;
    const cuts = _nostrScan(text).filter((r) => !r.url);
    if (!cuts.length) return text;
    let out = '';
    let at = 0;
    for (const r of cuts) {
        out += text.slice(at, r.start);
        at = r.end;
    }
    out += text.slice(at);
    return out.replace(/[ \t]{2,}/g, ' ').replace(/\n{3,}/g, '\n\n').trim();
}

/** The text without the web links a Nostr card covers, so no website preview is fetched for them. */
function withoutNostrEmbedUrls(text) {
    if (!text) return text;
    let out = text;
    for (const r of _nostrScan(text)) if (r.url) out = out.split(r.url).join('');
    return out;
}

/** Chat-list snippets: a reference reads as what it points at. */
function replaceNostrEmbedRefsForPreview(text) {
    if (!text) return text;
    const cuts = _nostrScan(text).filter((r) => !r.url);
    if (!cuts.length) return text;
    let out = '';
    let at = 0;
    for (const r of cuts) {
        out += text.slice(at, r.start) + _nostrRefLabel(r.entity);
        at = r.end;
    }
    return out + text.slice(at);
}

function _nostrRefLabel(entity) {
    return 'Nostr ' + _nostrRefNoun(entity);
}

function _nostrRefNoun(entity) {
    switch (_nostrRefKind(entity)) {
        case 30023: return 'Article';
        case 21: case 34235: return 'Video';
        case 22: case 34236: return 'Short';
        case 6: case 16: return 'Repost';
        default: return 'Post';
    }
}

// A reference inside an embedded post, as a text node sees it.
const _NOSTR_CHIP_RE = new RegExp(`(^|[\\s([{<"'])(nostr:)?(${_NOSTR_ENTITY})(?=$|[\\s)\\]}>"'.,;:!?])`, 'gi');

/**
 * References inside an embedded post become chips that open it: a quote of a quote is never
 * fetched until asked for, which is what keeps nesting to one level.
 */
function _nostrChipify(root) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
        acceptNode: (n) => (n.parentElement?.closest('a,code,pre,.mention,.ne-chip') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    const nodes = [];
    while (walker.nextNode()) if (/(?:note|nevent|naddr)1/i.test(walker.currentNode.nodeValue)) nodes.push(walker.currentNode);
    for (const node of nodes) {
        const text = node.nodeValue;
        const frag = document.createDocumentFragment();
        let at = 0;
        _NOSTR_CHIP_RE.lastIndex = 0;
        for (const m of text.matchAll(_NOSTR_CHIP_RE)) {
            const entity = m[3];
            if (!_nostrRefSupported(entity)) continue;
            const start = m.index + m[1].length;
            frag.append(text.slice(at, start));
            const chip = document.createElement('span');
            chip.className = 'ne-chip btn';
            chip.textContent = 'Quoted ' + _nostrRefNoun(entity);
            chip.addEventListener('click', (e) => {
                e.stopPropagation();
                _nostrOpenRef(entity);
            });
            frag.append(chip);
            at = start + (m[2] || '').length + entity.length;
        }
        if (at === 0) continue;
        frag.append(text.slice(at));
        node.replaceWith(frag);
    }
}

/** A chip or quote card: the event it names, over whatever the modal shows now. */
async function _nostrOpenRef(entity) {
    const r = await _resolveNostrEmbed(entity);
    if (r.state === 'ok') NOSTR_EMBED_HELPERS.push({ embed: r.embed, url: null, entity });
    else showToast(r.error || "This post couldn't be loaded");
}

/** Android back steps out of a quote before it closes the modal. */
function _nostrEmbedBack() {
    if (VectorSvelte.popNostrEmbedModal()) pushBack('nostr-embed', _nostrEmbedBack);
    else VectorSvelte.closeNostrEmbedModal();
}

// Resolved events keyed by reference: { state: 'loading', promise } | { state: 'ok', embed }
// | { state: 'err', error, ts }. A miss expires so a relay blip doesn't blank the card for good.
const _nostrEmbedCache = new Map();

function _resolveNostrEmbed(entity) {
    const key = entity.toLowerCase();
    const c = _nostrEmbedCache.get(key);
    if (c && (c.state !== 'err' || Date.now() - c.ts < NOSTR_EMBED_ERR_TTL_MS)) {
        return c.state === 'loading' ? c.promise : Promise.resolve(c);
    }
    const promise = invoke('fetch_nostr_embed', { reference: entity })
        .then((embed) => ({ state: 'ok', embed }))
        .catch((err) => ({ state: 'err', error: String(err), ts: Date.now() }))
        .then((r) => {
            _nostrEmbedCache.set(key, r);
            return r;
        });
    _nostrEmbedCache.set(key, { state: 'loading', promise });
    if (_nostrEmbedCache.size > NOSTR_EMBED_CACHE_MAX) _nostrEmbedCache.delete(_nostrEmbedCache.keys().next().value);
    return promise;
}

/** A post's text, rendered like a message: markdown, links, mentions, emoji. */
function _nostrEmbedText(text, emoji) {
    const span = document.createElement('span');
    span.className = 'dmsg-text nostr-embed-text';
    span.innerHTML = parseMarkdown(text || '');
    linkifyUrls(span);
    renderMentions(span, false, { allowBare: true, queueSync: true });
    _nostrChipify(span);
    if (emoji?.length) renderCustomEmojiShortcodes(span, emoji);
    twemojify(span);
    return span;
}

/** An article's markdown. Its pictures come through the backend's cache, like every image. */
function _nostrEmbedArticle(markdown, emoji) {
    const div = document.createElement('div');
    div.className = 'nostr-article-body';
    div.innerHTML = renderMarkdown(markdown || '');
    _nostrArticleImages(div);
    linkifyUrls(div);
    renderMentions(div, false, { allowBare: true, queueSync: true });
    _nostrChipify(div);
    if (emoji?.length) renderCustomEmojiShortcodes(div, emoji);
    twemojify(div);
    return div;
}

/** Chat markdown leaves `![alt](url)` as text; an article's become pictures. */
function _nostrArticleImages(root) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
        acceptNode: (n) => (n.parentElement?.closest('code,pre,a') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    const nodes = [];
    while (walker.nextNode()) if (walker.currentNode.nodeValue.includes('![')) nodes.push(walker.currentNode);
    const re = /!\[([^\]]*)\]\((https:\/\/[^\s)]+)\)/g;
    // A cap on downloads one article can start; the rest stay as their source text.
    let budget = NOSTR_ARTICLE_MAX_IMAGES;
    for (const node of nodes) {
        if (budget <= 0) break;
        const text = node.nodeValue;
        const frag = document.createDocumentFragment();
        let at = 0;
        re.lastIndex = 0;
        for (const m of text.matchAll(re)) {
            if (budget-- <= 0) break;
            frag.append(text.slice(at, m.index));
            // Hidden behind a shimmer until it lands: WebKit draws a source-less <img> as broken.
            const skel = document.createElement('span');
            skel.className = 'pack-skel ne-img-skel nostr-article-img';
            const img = document.createElement('img');
            img.className = 'nostr-article-img ne-img-pending';
            img.alt = m[1];
            img.addEventListener('load', () => { skel.remove(); img.classList.remove('ne-img-pending'); }, { once: true });
            img.addEventListener('error', () => { skel.remove(); img.remove(); }, { once: true });
            bindBackendCachedImg(img, m[2]);
            attachImagePreview(img);
            frag.append(skel, img);
            at = m.index + m[0].length;
        }
        if (at === 0) continue;
        frag.append(text.slice(at));
        node.replaceWith(frag);
    }
}

const _nostrVideoFetches = new Map();   // video url -> Promise<path>
const _nostrAuthorsQueued = new Set();

function _fmtMediaDuration(secs) {
    const s = Math.max(0, Math.round(secs || 0));
    const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
    const pad = (n) => String(n).padStart(2, '0');
    return h ? `${h}:${pad(m)}:${pad(r)}` : `${m}:${pad(r)}`;
}

/**
 * NostrEmbedHelpers: the in-chat cards (NostrEmbeds and its leaves) and the expanded modal.
 * @typedef {{
 *   refs: (text: string) => { key: string, entity: string, url: string|null }[],
 *   withoutRefs: (text: string) => string,
 *   settled: (key: string) => boolean,
 *   resolve: (entity: string) => Promise<{ state: 'ok', embed: object }|{ state: 'err', error: string }>,
 *   author: (npub: string) => { name: string, avatarSrc: string|null },
 *   showMiniProfile: (npub: string, el: Element) => void,
 *   twemojify: (el: Element) => void,
 *   when: (secs: number) => string,
 *   buildText: (text: string, emoji: object[]) => HTMLElement,
 *   buildArticle: (markdown: string, emoji: object[]) => HTMLElement,
 *   backendCachedImg: (img: HTMLImageElement, url: string) => void,
 *   attachImagePreview: (img: HTMLImageElement) => void,
 *   onResized: () => void,
 *   inlineVideo: () => boolean,
 *   willAutoDownload: (media: object) => boolean,
 *   fetchVideo: (media: object) => Promise<string>,
 *   cancelVideo: (url: string) => void,
 *   mediaUrl: (path: string) => string,
 *   reveal: (path: string) => void,
 *   duration: (secs: number) => string,
 *   openUrl: (url: string) => void,
 *   copy: (text: string, label: string) => void,
 *   open: (view: { embed: object, url: string|null, entity: string, origin?: object|null }) => void,
 *   push: (view: { embed: object, url: string|null, entity: string, origin?: object|null }) => void,
 *   back: () => void,
 *   close: () => void,
 * }} NostrEmbedHelpers
 */
const NOSTR_EMBED_HELPERS = {
    refs: (text) => nostrEmbedRefs(text),
    withoutRefs: (text) => stripNostrEmbedRefs(text),
    settled: (key) => { const c = _nostrEmbedCache.get(key); return !!c && c.state !== 'loading'; },
    resolve: (entity) => _resolveNostrEmbed(entity),
    author: (npub) => {
        const profile = getProfile(npub);
        if (!profile && !_nostrAuthorsQueued.has(npub)) {
            _nostrAuthorsQueued.add(npub);
            invoke('queue_profile_sync', { npub, priority: 'high', forceRefresh: false }).catch(() => {});
        }
        return { name: getName(npub), avatarSrc: getProfileAvatarSrc(profile) || null };
    },
    showMiniProfile: (npub, el) => showMiniProfile(npub, el),
    twemojify: (el) => twemojify(el),
    when: (secs) => timeAgo(secs * 1000),
    buildText: (text, emoji) => _nostrEmbedText(text, emoji),
    buildArticle: (markdown, emoji) => _nostrEmbedArticle(markdown, emoji),
    backendCachedImg: (img, url) => bindBackendCachedImg(img, url),
    attachImagePreview: (img) => attachImagePreview(img),
    onResized: () => compensateChatScrollForResize(),
    // WebKitGTK's video stack is unreliable, so Linux opens the file instead, as with attachments.
    inlineVideo: () => platformFeatures.os !== 'linux',
    willAutoDownload: (media) => AUTO_DOWNLOAD_ENABLED && media.size > 0 && media.size <= MAX_AUTO_DOWNLOAD_BYTES,
    fetchVideo: (media) => {
        let p = _nostrVideoFetches.get(media.url);
        if (!p) {
            p = invoke('cache_embed_video', { url: media.url, size: media.size ?? null, sha256: media.sha256 ?? null, fallbacks: media.fallbacks || [] });
            _nostrVideoFetches.set(media.url, p);
            // Shared only while in flight: the file can be pruned or cleared later.
            p.finally(() => _nostrVideoFetches.delete(media.url)).catch(() => {});
        }
        return p;
    },
    cancelVideo: (url) => { invoke('cancel_embed_video', { url }).catch(() => {}); },
    mediaUrl: (path) => mediaUrl(path),
    reveal: (path) => { if (platformFeatures.os === 'android') openAndroidAttachment(path); else revealItemInDir(path); },
    duration: (secs) => _fmtMediaDuration(secs),
    openUrl: (url) => _dmsgOpenPreviewUrl(url),
    copy: (text, label) => {
        navigator.clipboard.writeText(text).then(() => showToast(`${label} copied`), () => showToast('Copy failed'));
    },
    open: (view) => {
        VectorSvelte.openNostrEmbedModal(view);
        pushBack('nostr-embed', _nostrEmbedBack);
    },
    push: (view) => {
        VectorSvelte.pushNostrEmbedModal(view);
        pushBack('nostr-embed', _nostrEmbedBack);
    },
    back: () => { if (!VectorSvelte.popNostrEmbedModal()) NOSTR_EMBED_HELPERS.close(); },
    close: () => {
        VectorSvelte.closeNostrEmbedModal();
        popBack('nostr-embed');
    },
};

/** Going to a chat or a profile leaves an expanded post behind. */
function dismissNostrEmbed() {
    NOSTR_EMBED_HELPERS.close();
}

document.addEventListener('DOMContentLoaded', () => {
    VectorSvelte.setScreen('nostrEmbed', { h: NOSTR_EMBED_HELPERS });
    window.__TAURI__.event.listen('embed_video_progress', (e) => {
        VectorSvelte.embedVideoProgressed(e.payload.url, e.payload.progress);
    });
}, { once: true });
