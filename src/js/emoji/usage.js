// Emoji frecency, persisted per account in vector-core, and the searches that rank by it.
// One global scope: loads before picker.js and shares its globals.

// ----- Emoji usage (frecency) -------------------------------------------------
// Persisted per-account in the backend (vector-core `emoji_usage`). Replaces the
// old per-origin localStorage counters, which didn't survive production restarts
// (WKWebView localStorage on the app scheme is ephemeral) and leaked across
// accounts. Ranking is decayed frecency, computed backend-side; the picker just
// hydrates the in-memory signals it reads synchronously — stock `arrEmojis[].used`
// and a custom shortcode→score map — on each panel open, and bumps on selection.

/** Mirror of the backend SCORE_CAP so optimistic local bumps don't overshoot. */
const EMOJI_USAGE_CAP = 25;

/** Custom shortcode → frecency score, hydrated from the backend on open. */
let _customUsageScores = {};

/** Lazy emoji-char → `arrEmojis` entry map, so usage hydration/bumps are O(1)
 *  instead of a linear scan of ~1.9k entries on every panel open. */
let _emojiByChar = null;
function _emojiEntryByChar(char) {
    if (!_emojiByChar) {
        _emojiByChar = new Map();
        for (const e of arrEmojis) _emojiByChar.set(e.emoji, e);
    }
    return _emojiByChar.get(char);
}

/** Pull the active account's ranked usage and hydrate the synchronous signals
 *  the picker reads (stock `.used` + the custom score map). Cheap (≤256 rows),
 *  run on each open so it reflects the current account + latest persisted use. */
async function loadEmojiUsage() {
    let entries = [];
    try {
        entries = await invoke('get_emoji_usage', { limit: 256 }) || [];
    } catch (_) { entries = []; }
    for (const e of arrEmojis) e.used = 0;
    _customUsageScores = {};
    for (const u of entries) {
        if (u.kind === 'unicode') {
            const e = _emojiEntryByChar(u.id);
            if (e) e.used = u.score;
        } else if (u.kind === 'custom') {
            _customUsageScores[u.id] = u.score;
        }
    }
}

/** Record one emoji use: persist to the per-account backend store (fire-and-
 *  forget) and optimistically bump the in-memory signal so the recents row and
 *  search reflect it before the next reload. `kind` is 'unicode' | 'custom'. */
function bumpEmojiUsage(kind, id, url) {
    if (!id) return;
    invoke('bump_emoji_usage', { kind, id, url: url || null }).catch(() => {});
    if (kind === 'unicode') {
        const e = _emojiEntryByChar(id);
        if (e) e.used = Math.min(EMOJI_USAGE_CAP, (e.used || 0) + 1);
    } else {
        _customUsageScores[id] = Math.min(EMOJI_USAGE_CAP, (_customUsageScores[id] || 0) + 1);
    }
}

function _customEmojiUsage(shortcode) {
    return _customUsageScores[shortcode] || 0;
}

/** Batched usage record for a sent message: persist all its distinct emojis in
 *  ONE IPC (one backend load+save) and optimistically bump each locally. */
function bumpEmojiUsageBatch(entries) {
    if (!Array.isArray(entries) || entries.length === 0) return;
    invoke('bump_emoji_usage_batch', { entries }).catch(() => {});
    for (const e of entries) {
        if (e.kind === 'unicode') {
            const m = _emojiEntryByChar(e.id);
            if (m) m.used = Math.min(EMOJI_USAGE_CAP, (m.used || 0) + 1);
        } else if (e.kind === 'custom') {
            _customUsageScores[e.id] = Math.min(EMOJI_USAGE_CAP, (_customUsageScores[e.id] || 0) + 1);
        }
    }
}

/** Extract the DISTINCT emojis from a sent message for frecency tracking: stock
 *  unicode (grapheme-scanned against our dataset) + custom `:shortcode:` literals
 *  resolving to a subscribed pack. Each unique emoji counts once per message —
 *  repetition ACROSS messages over time is the meaningful signal, not within one. */
function extractMessageEmojis(text) {
    if (!text) return [];
    const out = [];
    const seen = new Set();
    try {
        const seg = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
        for (const part of seg.segment(text)) {
            const ch = part.segment;
            if (emojiDataSet.has(ch) && !seen.has('u:' + ch)) {
                seen.add('u:' + ch);
                out.push({ kind: 'unicode', id: ch, url: null });
            }
        }
    } catch (_) { /* Intl.Segmenter unavailable → skip stock extraction */ }
    const re = /:([a-zA-Z0-9_~+-]+):/g;
    let m;
    while ((m = re.exec(text)) !== null) {
        const code = m[1];
        if (seen.has('c:' + code)) continue;
        let url = null;
        for (const pack of (arrEmojiPacks || [])) {
            const hit = pack.emojis && pack.emojis.find(x => (x.dispCode || x.shortcode) === code);
            if (hit) { url = hit.url; break; }
        }
        if (url) { seen.add('c:' + code); out.push({ kind: 'custom', id: code, url }); }
    }
    return out;
}

/** Custom-emoji equivalent of `getMostUsedEmojis()`. Hydrates each
 *  shortcode in the usage map against `arrEmojiPacks` so the caller
 *  gets `{ isCustom, shortcode, url, used }` rows ready to merge with
 *  stock emoji recents. Customs not present in any subscribed pack are
 *  dropped silently (subscription was removed; the recents shouldn't
 *  point at packs that no longer exist). */
function getMostUsedCustomEmojis(limit) {
    const map = _customUsageScores;
    if (!arrEmojiPacks || !arrEmojiPacks.length) return [];
    const out = [];
    const seen = new Set();
    for (const shortcode of Object.keys(map)) {
        const used = map[shortcode];
        if (!used || used <= 0) continue;
        if (seen.has(shortcode)) continue;
        // Usage is keyed by the disambiguated code, so match on `dispCode`
        // (falling back to the bare shortcode for non-colliding emojis).
        for (const pack of arrEmojiPacks) {
            if (packIsDead(pack)) continue;
            if (!pack.emojis) continue;
            const match = pack.emojis.find(e => (e.dispCode || e.shortcode) === shortcode);
            if (match) {
                seen.add(shortcode);
                out.push({ isCustom: true, shortcode, url: match.url, used });
                break;
            }
        }
    }
    out.sort((a, b) => b.used - a.used);
    return typeof limit === 'number' ? out.slice(0, limit) : out;
}

/**
 * Match every subscribed pack's emoji shortcodes against `query` and tag
 * each result with a `matchTier`:
 *   0 = exact shortcode match (top of every list)
 *   1 = prefix match (mid)
 *   2 = inner substring (bottom)
 * The picker + shortcode autocomplete use this tier to interleave custom
 * results with stock results properly — a substring custom match must
 * NOT outrank an exact stock unicode match like "kiss".
 */
function searchCustomEmojis(query) {
    if (!query || !Array.isArray(arrEmojiPacks) || !arrEmojiPacks.length) return [];
    const q = String(query).toLowerCase().replace(/^:|:$/g, '');
    if (!q) return [];
    // Base scores chosen to slot into searchEmojis' score scale so a unified
    // sort puts custom + stock on equal footing:
    //   -2.0 exact shortcode  (matches stock shortcode-exact)
    //   -1.5 prefix           (matches stock shortcode-prefix)
    //    0.3 substring        (between stock word-starts-with 0.1 and fuzzy 0.5)
    // Personal usage subtracts via the same USAGE_SCORE_WEIGHT.
    const out = [];
    // De-dup by the disambiguated code (`love~1` / `love~2`) so same-name
    // emojis from different packs each surface as their own result instead of
    // one clobbering the other. Matching is still against the bare shortcode.
    const seen = new Set();
    for (const pack of arrEmojiPacks) {
        if (packIsDead(pack)) continue;
        if (!pack.emojis) continue;
        for (const e of pack.emojis) {
            const sc = e.shortcode.toLowerCase();
            let baseScore;
            if (sc === q) baseScore = -2.0;
            else if (sc.startsWith(q)) baseScore = -1.5;
            else if (sc.includes(q)) baseScore = 0.3;
            else continue;
            const code = e.dispCode || e.shortcode;
            if (seen.has(code)) continue;
            seen.add(code);
            const used = _customEmojiUsage(code);
            const weight = (typeof USAGE_SCORE_WEIGHT === 'number') ? USAGE_SCORE_WEIGHT : 0.2;
            out.push({
                isCustom: true,
                shortcode: code,
                url: e.url,
                name: code,
                packTitle: pack.title || pack.identifier,
                used,
                score: baseScore - used * weight,
            });
        }
    }
    out.sort((a, b) => a.score - b.score || a.shortcode.length - b.shortcode.length);
    return out;
}
