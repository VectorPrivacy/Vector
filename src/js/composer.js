/**
 * Rich composer: a contenteditable that renders markdown, mentions and custom
 * emoji inline while the source stays a plain string.
 *
 * DECORATED SOURCE, not WYSIWYG. `**bold**` keeps its asterisks (dimmed) and
 * bolds what's between, the way Discord does. That is what keeps the model a
 * flat string: caret positions are integer offsets, `value` is literally what
 * gets sent, and there is no round-trip back to markdown to get wrong.
 *
 * Duck-types the textarea members the app actually uses (value, selectionStart,
 * selectionEnd, setSelectionRange, focus, add/removeEventListener,
 * dispatchEvent), so existing call sites need no changes.
 */

// Zero-width space. Sentinels around an atomic widget give the caret somewhere
// to land — without them WebKit cannot place it after a trailing widget.
const CMP_ZWSP = '​';

/**
 * Typographic variants of one character. macOS substitution rewrites these as
 * you type — including inside a name the autofill just inserted — which turns a
 * real mention into plain text both on screen and on send. Every mapping is
 * 1:1, so folding never shifts an offset.
 */
const CMP_VARIANTS = [
    ["'", '‘’'],
    ['"', '“”'],
    ['-', '–—'],
];
const CMP_FOLD_TO = new Map();
for (const [canon, variants] of CMP_VARIANTS) for (const v of variants) CMP_FOLD_TO.set(v, canon);
const CMP_FOLD_RE = new RegExp('[' + CMP_VARIANTS.map(([, v]) => v).join('') + ']', 'g');

/** Canonical form, for comparing a display name against what was typed. */
function cmpFold(s) {
    return s.replace(CMP_FOLD_RE, (c) => CMP_FOLD_TO.get(c));
}

/** Regex source matching `name` under any variant of its characters. */
function cmpNamePattern(name) {
    return [...cmpFold(name)].map((ch) => {
        const hit = CMP_VARIANTS.find(([canon]) => canon === ch);
        // `-` leads its class, where it is literal.
        if (hit) return '[' + ch + hit[1] + ']';
        return ch.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    }).join('');
}

/**
 * Emoji sequences: flags (two regional indicators), keycaps, and pictographs
 * with their optional tone/variation selector plus any ZWJ continuation. Matched
 * as ONE unit so a multi-codepoint emoji is a single atom rather than its parts.
 */
const CMP_EMOJI = '(?:\\p{Regional_Indicator}\\p{Regional_Indicator}'
    + '|[0-9#*]\\uFE0F?\\u20E3'
    + '|\\p{Extended_Pictographic}(?:[\\u{1F3FB}-\\u{1F3FF}]|\\uFE0F)?'
    + '(?:\\u200D\\p{Extended_Pictographic}(?:[\\u{1F3FB}-\\u{1F3FF}]|\\uFE0F)?)*)';

const cmpTwemojiCache = Object.create(null);

/**
 * Twemoji URL for `emoji`, or null when Twemoji has no artwork for it — a
 * skin-toned or very new emoji, which then stays plain text and renders with
 * the system font. Delegates to twemoji's own parser so the composer and the
 * sent message resolve identically.
 */
function cmpTwemojiUrl(emoji) {
    if (emoji in cmpTwemojiCache) return cmpTwemojiCache[emoji];
    let url = null;
    if (window.twemoji) {
        const span = document.createElement('span');
        span.textContent = emoji;
        window.twemoji.parse(span, { callback: (icon) => '/twemoji/svg/' + icon + '.svg' });
        const img = span.querySelector('img');
        url = img ? img.getAttribute('src') : null;
    }
    cmpTwemojiCache[emoji] = url;
    return url;
}

/**
 * Split `src` into non-overlapping tokens, left to right. Every token carries
 * its source bounds, so the rendered DOM can always be read back to the exact
 * input string.
 */
function cmpTokenize(src, opts) {
    // A resolver is host code that can be wired up later than the composer. If one
    // throws, tokenising must still finish: the caller renders from these tokens,
    // so an escaping error leaves the DOM frozen and swallows every keystroke.
    // Degrading a decoration to plain text is a cosmetic loss; losing input is not.
    const safe = (fn, ...args) => {
        if (!fn) return null;
        try { return fn(...args); } catch (_) { return null; }
    };
    const out = [];
    // Emoji-only face (`opts.emojiOnly`): the Status mini-composer wants inline
    // emoji but no markdown, mentions or lists — same widgets, smaller grammar.
    if (opts.emojiOnly) {
        const re = new RegExp('(:[a-zA-Z0-9_~-]+:)|' + CMP_EMOJI, 'gmu');
        let last = 0;
        let m;
        while ((m = re.exec(src)) !== null) {
            if (m.index > last) out.push({ kind: 'text', from: last, to: m.index });
            const raw = m[0];
            const from = m.index;
            const to = from + raw.length;
            if (m[1]) {
                const url = safe(opts.resolveEmoji, raw.slice(1, -1));
                out.push(url ? { kind: 'emoji', from, to, url } : { kind: 'text', from, to });
            } else {
                const url = cmpTwemojiUrl(raw);
                out.push(url ? { kind: 'twemoji', from, to, url } : { kind: 'text', from, to });
            }
            last = to;
        }
        if (last < src.length) out.push({ kind: 'text', from: last, to: src.length });
        return out;
    }
    // Order matters: code first (its content is literal), emoji before mention
    // so `:a:` inside a name can't be swallowed.
    // Emoji goes LAST: `*️⃣` and `*italic*` both start with `*`, and the engine only
    // falls through to a later alternative once the earlier one fails to match.
    const re = new RegExp(
        '(`[^`\\n]+`)|(\\*\\*\\*[^*\\n]+\\*\\*\\*)|(\\*\\*[^*\\n]+\\*\\*)|(~~[^~\\n]+~~)|(\\|\\|[^|\\n]+\\|\\|)'
        + '|(\\*[^*\\n]+\\*)'
        // Underscore emphasis is boundary-gated in the handler, mirroring
        // marked: `_this_` italicises, `snake_case_name` stays literal.
        + '|(___[^_\\n]+___)|(__[^_\\n]+__)|(_[^_\\n]+_)'
        // The npub form comes before the name form: a pasted `@npub1…` is all
        // name-shaped characters, so the greedy name rule would swallow it and
        // leave 63 characters of key on screen.
        + '|(:[a-zA-Z0-9_~-]+:)'
        + '|(@npub1[023456789acdefghjklmnpqrstuvwxyz]{58})'
        // Just the marker. What follows is measured against the tracked names,
        // not a character class — see the handler.
        + '|(@(?=\\S))'
        // Line-level formats: ATX headers and `-# ` subtext take their whole
        // line — the marker recedes, the body carries the weight.
        + '|(^#{1,6} [^\\n]*)|(^-# [^\\n]*)'
        // List marker, anchored to the line start (hence the `m` flag) and mirroring
        // marked's own rule: up to three spaces, then `-`, `*` or `1.`, then a space.
        // `+` is deliberately absent — the message renderer refuses it too.
        + '|(^ {0,3}(?:[-*]|\\d{1,9}\\.) )'
        + '|' + CMP_EMOJI,
        'gmu');
    let last = 0;
    let m;
    while ((m = re.exec(src)) !== null) {
        if (m.index > last) out.push({ kind: 'text', from: last, to: m.index });
        const raw = m[0];
        const from = m.index;
        const to = from + raw.length;
        if (m[1]) out.push({ kind: 'code', from, to, mark: 1 });
        else if (m[2]) out.push({ kind: 'bolditalic', from, to, mark: 3 });
        else if (m[3]) out.push({ kind: 'bold', from, to, mark: 2 });
        else if (m[4]) out.push({ kind: 'strike', from, to, mark: 2 });
        else if (m[5]) out.push({ kind: 'spoiler', from, to, mark: 2 });
        else if (m[6]) out.push({ kind: 'italic', from, to, mark: 1 });
        else if (m[7] || m[8] || m[9]) {
            // Underscores only delimit at word edges (marked's rule). A rejected
            // match must not swallow tokens it covered — emit one literal `_`
            // and rescan from the next character.
            const word = /[\p{L}\p{N}_]/u;
            const opens = from === 0 || !word.test(src[from - 1]);
            const closes = to >= src.length || !word.test(src[to]);
            if (opens && closes) {
                out.push(m[7] ? { kind: 'bolditalic', from, to, mark: 3 }
                    : m[8] ? { kind: 'bold', from, to, mark: 2 }
                    : { kind: 'italic', from, to, mark: 1 });
            } else {
                out.push({ kind: 'text', from, to: from + 1 });
                last = from + 1;
                re.lastIndex = from + 1;
                continue;
            }
        }
        else if (m[10]) {
            // Only an emoji the app can actually resolve becomes a widget; an
            // unknown `:word:` stays literal text so it can still be typed through.
            const url = safe(opts.resolveEmoji, raw.slice(1, -1));
            out.push(url ? { kind: 'emoji', from, to, url } : { kind: 'text', from, to });
        } else if (m[11]) {
            // A mention pasted from a message carries the raw key, which is what
            // gets sent. Show the person's name over it; `data-src` keeps the npub.
            const label = safe(opts.resolveNpub, raw.slice(1));
            out.push(label ? { kind: 'npubmention', from, to, label } : { kind: 'text', from, to });
        } else if (m[12]) {
            // Measured against the tracked names rather than a character class: a
            // display name is arbitrary text, so any class is a guess that needs
            // widening the first time someone picks a character it forgot.
            const known = safe(opts.resolveMention, src, from + 1);
            const end = known ? from + 1 + known.length : -1;
            // Same boundaries the send-time conversion uses, so the preview can't
            // promise a tag that won't fire.
            const opens = from === 0 || /\s/.test(src[from - 1]);
            const closes = known && (end >= src.length || /[\s.,!?;:]/.test(src[end]));
            if (known && opens && closes) {
                out.push({ kind: 'mention', from, to: end });
                last = end;
                re.lastIndex = end;
                continue;
            }
            out.push({ kind: 'text', from, to });
        } else if (m[13]) {
            // Whole-line format. `mark` = hashes + space, so it doubles as the
            // level; inline tokens within the line stay literal in the preview.
            out.push({ kind: 'header', from, to, mark: raw.indexOf(' ') + 1 });
        } else if (m[14]) {
            out.push({ kind: 'subtext', from, to, mark: 3 });
        } else if (m[15]) {
            out.push({ kind: 'listmark', from, to });
        } else {
            // Unicode emoji. No Twemoji artwork (skin tones, very new emoji) falls
            // back to plain text, which the system font renders.
            const url = cmpTwemojiUrl(raw);
            out.push(url ? { kind: 'twemoji', from, to, url } : { kind: 'text', from, to });
        }
        last = to;
    }
    if (last < src.length) out.push({ kind: 'text', from: last, to: src.length });
    return cmpApplyColor(src, cmpApplyAnsi(src, out), opts);
}

// Tokens whose text keeps its own look, as a sent message's colour skips them.
const CMP_UNPAINTED = new Set(['code', 'mention', 'npubmention', 'emoji', 'twemoji', 'spoiler', 'ansitext', 'ansicode', 'fence', 'colormark']);

/**
 * Colour markup and a leading colour command, previewed as they'll send: the tags
 * and the command recede like any marker, and the text they cover takes its
 * colours glyph by glyph. The run's `paint` maps each glyph's offset to its colour;
 * `colorKey` changes whenever a glyph would change colour, which repaints.
 */
function cmpApplyColor(src, tokens, opts) {
    const pairs = tcScan(src);
    const cmd = tcCommand(src);
    // A bot in the chat that claims the name runs instead, and gets the text as typed.
    let owned = true;
    if (cmd && opts.ownsCommand) {
        try { owned = opts.ownsCommand(cmd.typed); } catch (_) { /* host not wired yet: preview */ }
    }
    const command = cmd && !cmd.error && owned ? cmd : null;
    if (!pairs.length && !command) return tokens;
    const marks = [];
    const spans = [];
    if (command) {
        marks.push([0, command.prefixEnd]);
        spans.push({ from: command.prefixEnd, to: src.length, effect: command.effect, colors: command.colors });
    }
    for (const p of pairs) {
        marks.push(p.open, p.close);
        spans.push({ from: p.open[1], to: p.close[0], effect: p.effect, colors: p.colors });
    }
    const out = [];
    for (const t of tokens) {
        let pieces = [t];
        for (const [from, to] of marks) {
            pieces = pieces.flatMap((p) => {
                if (to <= p.from || from >= p.to) return [p];
                const keep = [];
                if (p.from < from) keep.push({ kind: 'text', from: p.from, to: from });
                if (p.to > to) keep.push({ kind: 'text', from: to, to: p.to });
                return keep;
            });
        }
        out.push(...pieces);
    }
    for (const [from, to] of marks) out.push({ kind: 'colormark', from, to });
    out.sort((a, b) => a.from - b.from);

    const skip = out.filter((t) => CMP_UNPAINTED.has(t.kind));
    const skipped = (i) => skip.some((t) => i >= t.from && i < t.to);
    const seg = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
    const paint = new Map();
    let key = '';
    spans.forEach((span) => {
        const glyphs = [];
        for (const g of seg.segment(src.slice(span.from, span.to))) {
            const at = span.from + g.index;
            if (/\S/.test(g.segment) && !skipped(at)) glyphs.push(at);
        }
        const colors = tcColors(span, glyphs.length);
        glyphs.forEach((at, i) => paint.set(at, colors[i]));
        // A solid span looks the same at any length, so typing in one repaints nothing;
        // the others recolour as glyphs come and go.
        key += span.effect === 'solid'
            ? `${span.from}:solid${span.colors[0]};`
            : `${span.from}:${span.to}:${glyphs.length}:${span.effect}${span.colors.join('')};`;
    });
    out.paint = paint;
    out.colorKey = key;
    return out;
}

const CMP_ANSI_OPEN = /^ {0,3}```ansi(?:[ \t][^\n]*)?$/gim;
const CMP_FENCE_CLOSE = /^ {0,3}```[ \t]*$/gm;

/**
 * ```ansi blocks, open or still being typed: fences, coloured text runs, and each
 * escape sequence as its own token. Colours fold through the same SGR reader the
 * message renderer uses, so the preview can't disagree with what gets sent.
 */
function cmpAnsiRegions(src) {
    const regions = [];
    CMP_ANSI_OPEN.lastIndex = 0;
    let m;
    while ((m = CMP_ANSI_OPEN.exec(src)) !== null) {
        const openTo = m.index + m[0].length;
        CMP_FENCE_CLOSE.lastIndex = Math.min(openTo + 1, src.length);
        const close = openTo < src.length ? CMP_FENCE_CLOSE.exec(src) : null;
        const bodyTo = close ? close.index : src.length;
        const end = close ? close.index + close[0].length : src.length;
        const tokens = [{ kind: 'fence', from: m.index, to: openTo }];
        const st = { b: false, i: false, u: false, s: false, fg: null, bg: null };
        const body = src.slice(openTo, bodyTo);
        const text = (from, to) => {
            if (to > from) tokens.push({ kind: 'ansitext', from, to, style: { ...st } });
        };
        let last = 0;
        for (const e of body.matchAll(ANSI_ESCAPE)) {
            text(openTo + last, openTo + e.index);
            last = e.index + e[0].length;
            tokens.push({ kind: 'ansicode', from: openTo + e.index, to: openTo + last });
            if (e[2] === 'm' && /^[0-9;]*$/.test(e[1])) applyAnsiSgr(st, e[1]);
        }
        text(openTo + last, bodyTo);
        if (close) tokens.push({ kind: 'fence', from: close.index, to: end });
        regions.push({ from: m.index, to: end, tokens });
        if (end >= src.length) break;
        CMP_ANSI_OPEN.lastIndex = Math.max(end, m.index + 1);
    }
    return regions;
}

/** Fold ```ansi blocks into the token run, and hide stray escape sequences anywhere else. */
function cmpApplyAnsi(src, tokens) {
    if (!src.includes('\x1b') && !/```ansi/i.test(src)) return tokens;
    const regions = cmpAnsiRegions(src);
    const out = [];
    for (const t of tokens) {
        let pieces = [t];
        for (const g of regions) {
            if (g.to <= t.from || g.from >= t.to) continue;
            pieces = pieces.flatMap((p) => {
                if (g.to <= p.from || g.from >= p.to) return [p];
                const keep = [];
                if (p.from < g.from) keep.push({ kind: 'text', from: p.from, to: g.from });
                if (p.to > g.to) keep.push({ kind: 'text', from: g.to, to: p.to });
                return keep;
            });
        }
        for (const p of pieces) {
            const raw = src.slice(p.from, p.to);
            if (p.kind !== 'text' || !raw.includes('\x1b')) { out.push(p); continue; }
            let last = 0;
            for (const e of raw.matchAll(ANSI_ESCAPE)) {
                if (e.index > last) out.push({ kind: 'text', from: p.from + last, to: p.from + e.index });
                last = e.index + e[0].length;
                out.push({ kind: 'ansicode', from: p.from + e.index, to: p.from + last });
            }
            if (last < raw.length) out.push({ kind: 'text', from: p.from + last, to: p.to });
        }
    }
    if (!regions.length) return out;
    for (const g of regions) out.push(...g.tokens);
    return out.sort((a, b) => a.from - b.from);
}

function cmpAnsiKey(st) {
    return `${+st.b}${+st.i}${+st.u}${+st.s}/${st.fg || ''}/${st.bg || ''}`;
}

/**
 * Structural fingerprint: the SHAPE of the token run, deliberately without
 * offsets. Editing inside a run shifts every offset after it, so including them
 * made a fingerprint that changed on every keystroke — re-rendering constantly
 * and, worse, overwriting the caret the browser had just placed correctly with
 * our own restored guess. Plain typing and deleting must touch no DOM at all.
 *
 * Atomic widgets carry their source, since swapping `:cat:` for `:dog:` keeps
 * the shape identical but must still repaint the image.
 */
function cmpSignature(tokens, src) {
    let s = '';
    for (const t of tokens) {
        s += t.kind;
        if (t.kind === 'emoji' || t.kind === 'twemoji' || t.kind === 'ansicode') s += '(' + src.slice(t.from, t.to) + ')';
        if (t.kind === 'ansitext') s += cmpAnsiKey(t.style);
        // Levels share a kind, but `#` -> `##` must still repaint the mark.
        if (t.kind === 'header') s += t.mark;
        s += '|';
    }
    if (tokens.colorKey) s += tokens.colorKey;
    return s;
}

function createRichComposer(host, opts = {}) {
    const el = document.createElement('div');
    el.className = 'rich-composer';
    el.contentEditable = 'true';
    el.setAttribute('role', 'textbox');
    el.setAttribute('aria-multiline', 'true');
    el.spellcheck = true;
    if (opts.placeholder) el.dataset.placeholder = opts.placeholder;
    host.appendChild(el);

    let src = '';
    let signature = '';
    let composing = false;

    // ---- source <-> DOM -----------------------------------------------------

    /** Source text of everything inside `node`, by the same rules everywhere. */
    function serializeInto(node) {
        let s = '';
        for (const child of node.childNodes) {
            if (child.nodeType === Node.TEXT_NODE) {
                s += child.nodeValue.split(CMP_ZWSP).join('');
            } else if (child.nodeType === Node.ELEMENT_NODE) {
                if (child.dataset && child.dataset.src !== undefined) {
                    s += child.dataset.src;              // widget stands for its source run
                } else if (child.tagName === 'BR') {
                    s += '\n';
                } else {
                    s += serializeInto(child);
                }
            }
        }
        return s;
    }

    /** Serialize the DOM back to source. Also the copy handler. */
    function readDom() {
        let s = serializeInto(el);
        // Browsers park a filler <br> at the end of an editable — deleting the last
        // character leaves one behind. Counting it would make an emptied composer
        // read as "\n": never empty, so the placeholder stays gone and `value` is
        // a newline nobody typed. A deliberate trailing newline keeps its own <br>,
        // because the filler is always the one AFTER it.
        // Find the last node that actually renders. WebKit parks empty text nodes
        // after the filler, and they're invisible in innerHTML, so walking to
        // `lastChild` alone finds a text node and misses the <br> behind it.
        const lastRendered = (node) => {
            for (let i = node.childNodes.length - 1; i >= 0; i--) {
                const c = node.childNodes[i];
                if (c.nodeType === Node.TEXT_NODE) {
                    if (c.nodeValue.split(CMP_ZWSP).join('') === '') continue;
                    return c;
                }
                if (c.nodeType !== Node.ELEMENT_NODE) continue;
                if (c.tagName === 'BR' || (c.dataset && c.dataset.src !== undefined)) return c;
                const inner = lastRendered(c);
                if (inner) return inner;
            }
            return null;
        };
        const tail = lastRendered(el);
        if (tail && tail.nodeName === 'BR' && s.endsWith('\n')) {
            s = s.slice(0, -1);
        }
        return s;
    }

    function span(cls, text) {
        const n = document.createElement('span');
        n.className = cls;
        n.textContent = text;
        return n;
    }

    /** A decoration keeps its markers visible but dimmed, so source == display. */
    function decorated(cls, raw, markLen) {
        const wrap = document.createElement('span');
        wrap.className = cls;
        wrap.appendChild(span('cmp-mark', raw.slice(0, markLen)));
        wrap.appendChild(span('cmp-body', raw.slice(markLen, raw.length - markLen)));
        wrap.appendChild(span('cmp-mark', raw.slice(raw.length - markLen)));
        return wrap;
    }

    function render(tokens) {
        el.textContent = '';
        for (const t of tokens) {
            const raw = src.slice(t.from, t.to);
            switch (t.kind) {
                case 'bold': el.appendChild(decorated('cmp-bold', raw, 2)); break;
                case 'bolditalic': el.appendChild(decorated('cmp-bolditalic', raw, 3)); break;
                case 'italic': el.appendChild(decorated('cmp-italic', raw, 1)); break;
                case 'strike': el.appendChild(decorated('cmp-strike', raw, 2)); break;
                case 'spoiler': el.appendChild(decorated('cmp-spoiler', raw, 2)); break;
                case 'code': el.appendChild(decorated('cmp-code', raw, 1)); break;
                case 'header':
                case 'subtext': {
                    // Leading mark only — decorated() is symmetric.
                    const wrap = document.createElement('span');
                    wrap.className = t.kind === 'header' ? 'cmp-h' + (t.mark - 1) : 'cmp-subtext';
                    wrap.appendChild(span('cmp-mark', raw.slice(0, t.mark)));
                    wrap.appendChild(span('cmp-body', raw.slice(t.mark)));
                    el.appendChild(wrap);
                    break;
                }
                case 'listmark':
                    // Recede it like any other marker. The bullet itself is the
                    // renderer's job; here the point is that the line WILL format.
                    el.appendChild(span('cmp-mark', raw));
                    break;
                case 'fence':
                case 'colormark':
                    el.appendChild(span('cmp-mark', raw));
                    break;
                case 'ansitext': {
                    // Styles take only what applyAnsiSgr built from parsed numbers.
                    const run = document.createElement('span');
                    run.className = 'cmp-ansi';
                    const st = t.style;
                    if (st.b) run.style.fontWeight = '700';
                    if (st.i) run.style.fontStyle = 'italic';
                    if (st.u || st.s) run.style.textDecoration = [st.u && 'underline', st.s && 'line-through'].filter(Boolean).join(' ');
                    if (st.fg) run.style.color = st.fg;
                    if (st.bg) run.style.backgroundColor = st.bg;
                    raw.split('\n').forEach((p, i) => {
                        if (i) run.appendChild(document.createElement('br'));
                        if (p) run.appendChild(document.createTextNode(p));
                    });
                    el.appendChild(run);
                    break;
                }
                case 'ansicode': {
                    // An escape sequence is styling, not text: an invisible atom, a bare
                    // <img> like the emoji widgets so IMEs read it as one object.
                    const img = document.createElement('img');
                    img.className = 'cmp-ansi-code';
                    img.dataset.src = raw;
                    img.alt = '';
                    img.draggable = false;
                    el.appendChild(img);
                    break;
                }
                case 'mention':
                    // Editable text, NOT an atomic widget: the caret walks through it
                    // normally and a broken pill degrades to plain text instead of
                    // trapping the caret. The source already carries the short name.
                    el.appendChild(span('cmp-mention', raw));
                    break;
                case 'npubmention': {
                    // Atomic, unlike the name form: the text shown ("@Alice") is not
                    // the source ("@npub1…"), so the caret must not enter it. `data-src`
                    // carries the key, keeping `value` byte-identical to what is sent.
                    const w = span('cmp-mention', '@' + t.label);
                    w.contentEditable = 'false';
                    w.dataset.src = raw;
                    el.appendChild(document.createTextNode(CMP_ZWSP));
                    el.appendChild(w);
                    el.appendChild(document.createTextNode(CMP_ZWSP));
                    break;
                }
                // Emoji widgets are BARE <img> elements, deliberately: an image is
                // inherently atomic (nothing to type inside) and reads to an IME as
                // an ordinary object character. The previous shape — a
                // contenteditable=false span between ZWSP sentinels — made old
                // Android WebViews reset the input connection (keyboard close)
                // whenever a native edit landed the caret against the island.
                case 'twemoji': {
                    const img = document.createElement('img');
                    img.className = 'cmp-twemoji';
                    img.dataset.src = raw;
                    // Always a BUNDLED asset path from `cmpTwemojiUrl` (/twemoji/svg/…),
                    // never a network URL — unlike pack art, which must go through the host.
                    img.src = t.url;
                    img.alt = raw;
                    img.draggable = false;
                    // Second safety net: artwork the manifest claims but the build
                    // doesn't ship degrades to the character rather than a broken icon.
                    img.addEventListener('error', () => {
                        img.replaceWith(document.createTextNode(raw));
                    }, { once: true });
                    el.appendChild(img);
                    break;
                }
                case 'emoji': {
                    const img = document.createElement('img');
                    img.className = 'cmp-emoji';
                    img.dataset.src = raw;
                    img.alt = raw;
                    img.draggable = false;
                    el.appendChild(img);
                    // Appended first: the fallback replaces the widget, which needs a parent.
                    const fail = () => { if (img.parentNode) img.replaceWith(document.createTextNode(raw)); };
                    // Pack art is REMOTE. This module never assigns such a src itself: the
                    // backend proxy is the only thing allowed to reach the network, and it
                    // is what carries Tor routing. A host without a binder gets the literal
                    // `:shortcode:` — never a direct fetch.
                    if (opts.bindEmojiImg) {
                        try { opts.bindEmojiImg(img, t.url, fail); } catch (_) { fail(); }
                    } else {
                        fail();
                    }
                    break;
                }
                default: {
                    // Newlines need real <br>; a bare "\n" in a div collapses.
                    const parts = raw.split('\n');
                    parts.forEach((p, i) => {
                        if (i) el.appendChild(document.createElement('br'));
                        if (p) el.appendChild(document.createTextNode(p));
                    });
                }
            }
        }
        // Emit the trailing filler ourselves. readDom always discards one trailing
        // <br> (the browser's own filler, which it re-adds after every edit), so a
        // source ending in a newline must render TWO: one for the line, one to be
        // discarded. Without it each read eats a newline and every second Shift+Enter
        // appears to do nothing.
        if (src.endsWith('\n')) el.appendChild(document.createElement('br'));
        if (tokens.paint && tokens.paint.size) paintColors(tokens.paint);
        if (!el.firstChild) el.appendChild(document.createTextNode(''));
        // `:empty` can't drive the placeholder — the root always holds a text node.
        el.dataset.empty = src === '' ? '1' : '0';
    }

    /**
     * Wrap each coloured glyph run of the rendered text in its colour. Runs are
     * plain spans, so they read back as their text like any other decoration.
     */
    function paintColors(paint) {
        const seg = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
        let at = 0;
        const walk = (node) => {
            for (const child of [...node.childNodes]) {
                if (child.nodeType === Node.TEXT_NODE) {
                    const value = child.nodeValue;
                    const base = at;
                    at += value.split(CMP_ZWSP).join('').length;
                    if (value.includes(CMP_ZWSP) || child.parentElement.closest('.cmp-mark, .cmp-code, .cmp-mention, .cmp-ansi, .cmp-spoiler')) continue;
                    let colored = false;
                    const frag = document.createDocumentFragment();
                    let run = null;
                    let runColor = null;
                    for (const g of seg.segment(value)) {
                        const color = paint.get(base + g.index) || (run && !/\S/.test(g.segment) ? runColor : null);
                        if (!color) {
                            run = null;
                            frag.appendChild(document.createTextNode(g.segment));
                            continue;
                        }
                        colored = true;
                        if (run && color === runColor) { run.textContent += g.segment; continue; }
                        run = span('cmp-color', g.segment);
                        run.style.color = color;
                        runColor = color;
                        frag.appendChild(run);
                    }
                    if (colored) child.replaceWith(frag);
                } else if (child.nodeType === Node.ELEMENT_NODE) {
                    if (child.dataset && child.dataset.src !== undefined) at += child.dataset.src.length;
                    else if (child.tagName === 'BR') at += 1;
                    else walk(child);
                }
            }
        };
        walk(el);
    }

    // ---- selection mapping --------------------------------------------------

    /**
     * Model offset of the current caret, or null when the selection isn't ours.
     *
     * Measures the span from the start of the editable to the caret and
     * serializes it, rather than hunting for the caret's container while walking.
     * The container is often the EDITABLE ITSELF with a child index — which is
     * what a browser leaves behind after deleting a line — and a hunt that only
     * recognises text nodes misses it, runs to the end, and reports the full
     * length. That is the caret jumping to the bottom on every such edit.
     */
    function offsetOfPoint(node, nodeOffset) {
        if (!el.contains(node) && node !== el) return null;
        const pre = document.createRange();
        pre.selectNodeContents(el);
        try {
            pre.setEnd(node, nodeOffset);
        } catch (_) {
            return null;
        }
        return serializeInto(pre.cloneContents()).length;
    }

    function caretOffset() {
        const sel = window.getSelection();
        if (!sel || !sel.rangeCount) return null;
        const range = sel.getRangeAt(0);
        return offsetOfPoint(range.endContainer, range.endOffset);
    }

    /**
     * Both ends of the LIVE selection as model offsets, or null when the
     * selection isn't inside us. `selectionStart` used to report the END too, so
     * anything replacing a selection inserted beside the highlighted text.
     */
    function liveSelectionRange() {
        const sel = window.getSelection();
        if (!sel || !sel.rangeCount) return null;
        const r = sel.getRangeAt(0);
        const start = offsetOfPoint(r.startContainer, r.startOffset);
        const end = offsetOfPoint(r.endContainer, r.endOffset);
        if (start === null || end === null) return null;
        return start <= end ? { start, end } : { start: end, end: start };
    }

    /**
     * Where the caret was the last time it was ours. Clicking the emoji picker
     * moves focus out of the composer, so by the time it calls back there is no
     * live selection to read and an insert would land at the end of the message
     * instead of where you left off.
     */
    let lastSelection = null;
    document.addEventListener('selectionchange', () => {
        const r = liveSelectionRange();
        if (r) lastSelection = r;
    });

    function selectionRange() {
        return liveSelectionRange() || lastSelection;
    }

    /** Put the caret at model offset `target`. */
    function setCaret(target) {
        const sel = window.getSelection();
        if (!sel) return;
        let seen = 0;
        let placed = false;
        const place = (node, off) => {
            const r = document.createRange();
            r.setStart(node, off);
            r.collapse(true);
            sel.removeAllRanges();
            sel.addRange(r);
            placed = true;
        };
        const walk = (node) => {
            for (const child of node.childNodes) {
                if (placed) return;
                if (child.nodeType === Node.TEXT_NODE) {
                    const clean = child.nodeValue.split(CMP_ZWSP).join('');
                    if (seen + clean.length >= target) {
                        // Map the clean offset back through any ZWSPs in this node.
                        let want = target - seen;
                        let idx = 0;
                        let cnt = 0;
                        while (idx < child.nodeValue.length && cnt < want) {
                            if (child.nodeValue[idx] !== CMP_ZWSP) cnt++;
                            idx++;
                        }
                        place(child, idx);
                        return;
                    }
                    seen += clean.length;
                } else if (child.nodeType === Node.ELEMENT_NODE) {
                    if (child.dataset && child.dataset.src !== undefined) {
                        seen += child.dataset.src.length;
                    } else if (child.tagName === 'BR') {
                        seen += 1;
                    } else {
                        walk(child);
                    }
                }
            }
        };
        walk(el);
        if (!placed) {
            const r = document.createRange();
            r.selectNodeContents(el);
            r.collapse(false);
            sel.removeAllRanges();
            sel.addRange(r);
        }
    }

    // ---- the reconcile loop -------------------------------------------------

    /**
     * Adopt whatever the browser just did, then re-render ONLY if the token
     * structure changed. Typing a plain character inside a plain run is the
     * common case and touches no DOM at all — which is what keeps this at 60fps
     * and, more importantly, keeps the IME's composition intact.
     */
    /**
     * True when a `.cmp-mark` span's text no longer equals its token's marker.
     * Native typing at a mark boundary inserts INTO the marker span — a
     * header's body starts empty, so the caret parks in the mark and the whole
     * line would keep typing in marker grey without a repaint.
     */
    function marksDrifted(tokens) {
        const expect = [];
        for (const t of tokens) {
            const raw = src.slice(t.from, t.to);
            switch (t.kind) {
                case 'bold': case 'bolditalic': case 'italic': case 'strike':
                case 'spoiler': case 'code':
                    expect.push(raw.slice(0, t.mark), raw.slice(raw.length - t.mark));
                    break;
                case 'header': case 'subtext':
                    expect.push(raw.slice(0, t.mark));
                    break;
                case 'listmark': case 'fence': case 'colormark':
                    expect.push(raw);
                    break;
            }
        }
        const spans = el.querySelectorAll('.cmp-mark');
        if (spans.length !== expect.length) return true;
        for (let i = 0; i < spans.length; i++) {
            if (spans[i].textContent !== expect[i]) return true;
        }
        return false;
    }

    /**
     * True when typed text sits outside its ```ansi run. A caret at the edge of a
     * run after an escape code inserts beside the span, not in it, and the
     * letter would show uncoloured until something else repainted.
     */
    function ansiDrifted(tokens) {
        let expect = '';
        for (const t of tokens) if (t.kind === 'ansitext') expect += src.slice(t.from, t.to).split('\n').join('');
        let shown = '';
        for (const run of el.querySelectorAll('.cmp-ansi')) shown += run.textContent;
        return shown !== expect;
    }

    function syncFromDom() {
        const next = readDom();
        src = next;
        const tokens = cmpTokenize(src, opts);
        const sig = cmpSignature(tokens, src);
        if (sig === signature) {
            if (composing || (!marksDrifted(tokens) && !ansiDrifted(tokens))) return;
        } else {
            signature = sig;
        }
        const caret = caretOffset();
        render(tokens);
        if (caret !== null) setCaret(caret);
    }

    /** Rebuild from `src` unconditionally (programmatic writes). */
    function rerender(caret) {
        const tokens = cmpTokenize(src, opts);
        signature = cmpSignature(tokens, src);
        render(tokens);
        if (caret !== null && caret !== undefined) setCaret(caret);
    }

    // Own the line break rather than reading it back out of the DOM. WebKit adds
    // its filler <br> LAZILY, so "how many trailing <br>s mean a newline" has no
    // stable answer — the same keypress lands differently depending on whether the
    // filler has materialised yet, which drops every other break. Applying it to
    // the model and re-rendering makes the DOM exactly what we wrote.
    el.addEventListener('beforeinput', (e) => {
        if (composing) return;
        if (e.inputType !== 'insertLineBreak' && e.inputType !== 'insertParagraph') return;
        e.preventDefault();
        const at = caretOffset() ?? src.length;
        src = src.slice(0, at) + '\n' + src.slice(at);
        rerender(at + 1);
        el.dispatchEvent(new Event('input', { bubbles: true }));
    });

    // Deletions AT or NEXT TO an atomic widget are done in the MODEL, never
    // by the browser: older Android WebViews (11-era) reset the IME's input
    // connection — the keyboard just closes — both when a native delete
    // consumes the non-editable island AND when one merely lands the caret
    // against it (deleting the space after an emoji). Splice the source
    // ourselves; every platform takes this path so behaviour can't diverge.
    // Range deletions and deletes in plain text keep the native path.
    // Returns true when handled (the caller cancels the event).
    function deleteAdjacentWidget(backward) {
        const sel = selectionRange();
        if (!sel || sel.start !== sel.end) return false;
        const caret = sel.start;
        const atomic = (t) => t.kind === 'emoji' || t.kind === 'twemoji' || t.kind === 'ansicode';
        const tokens = cmpTokenize(src, opts);
        const splice = (from, to) => {
            src = src.slice(0, from) + src.slice(to);
            rerender(from);
            el.dispatchEvent(new Event('input', { bubbles: true }));
            return true;
        };
        // Escape codes are invisible, so a delete passes over them to the nearest
        // character you can see, and the colour it belongs to stays.
        const codeAt = (pos) => tokens.find(t => t.kind === 'ansicode' && (backward ? t.to === pos : t.from === pos));
        if (codeAt(caret)) {
            let p = caret;
            for (let c = codeAt(p); c; c = codeAt(p)) p = backward ? c.from : c.to;
            const widget = tokens.find(t => atomic(t) && (backward ? t.to === p : t.from === p));
            if (widget) return splice(widget.from, widget.to);
            const cp = backward ? src.codePointAt(p - 2) : src.codePointAt(p);
            const w = cp > 0xFFFF && (backward ? p >= 2 : true) ? 2 : 1;
            if (backward ? p < 1 : p >= src.length) return true;
            return backward ? splice(p - w, p) : splice(p, p + w);
        }
        if (backward) {
            const hit = tokens.find(t => atomic(t) && t.to === caret);
            if (hit) return splice(hit.from, hit.to);
            // One character, whole code point: half a surrogate pair is worse
            // than the bug being fixed.
            let w = 1;
            const lo = src.charCodeAt(caret - 1);
            if (lo >= 0xDC00 && lo <= 0xDFFF && caret >= 2) {
                const hi = src.charCodeAt(caret - 2);
                if (hi >= 0xD800 && hi <= 0xDBFF) w = 2;
            }
            if (caret >= w && tokens.some(t => atomic(t) && t.to === caret - w)) {
                return splice(caret - w, caret);
            }
            return false;
        }
        const hit = tokens.find(t => atomic(t) && t.from === caret);
        if (hit) return splice(hit.from, hit.to);
        let w = 1;
        const hi = src.charCodeAt(caret);
        if (hi >= 0xD800 && hi <= 0xDBFF && caret + 1 < src.length) {
            const lo = src.charCodeAt(caret + 1);
            if (lo >= 0xDC00 && lo <= 0xDFFF) w = 2;
        }
        if (tokens.some(t => atomic(t) && t.from === caret + w)) {
            return splice(caret, caret + w);
        }
        return false;
    }

    // Two interception points, whichever fires first wins and cancels the
    // rest. keydown catches the REAL key event (KEYCODE_DEL) that Android
    // IMEs send AHEAD of their input-connection machinery — on old WebViews
    // that machinery resets (keyboard closes) before any beforeinput can
    // reach us, so beforeinput alone only worked on the second press.
    el.addEventListener('keydown', (e) => {
        if (composing) return;
        if (e.key !== 'Backspace' && e.key !== 'Delete') return;
        if (deleteAdjacentWidget(e.key === 'Backspace')) e.preventDefault();
    });
    el.addEventListener('beforeinput', (e) => {
        if (composing) return;
        if (e.inputType !== 'deleteContentBackward' && e.inputType !== 'deleteContentForward') return;
        if (deleteAdjacentWidget(e.inputType === 'deleteContentBackward')) e.preventDefault();
    });

    // The sentinels around a widget are real caret stops, so crossing one costs an
    // extra tap. A ZWSP hop is exactly a move that leaves the MODEL offset where it
    // was, which is the cheap test for "keep going".
    el.addEventListener('keydown', (e) => {
        if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
        if (e.shiftKey || e.metaKey || e.altKey || e.ctrlKey || composing) return;
        const sel = window.getSelection();
        if (!sel || !sel.isCollapsed || !sel.modify || !sel.rangeCount) return;
        if (!el.contains(sel.getRangeAt(0).startContainer)) return;
        const before = caretOffset();
        if (before === null) return;
        e.preventDefault();
        const dir = e.key === 'ArrowRight' ? 'right' : 'left';
        // Bounded: a widget contributes at most two sentinels, and standing still
        // means the caret is against an edge. Invisible escape codes are crossed
        // too, so a step always lands past one visible character.
        let from = before;
        for (let i = 0; i < 64; i++) {
            const r = sel.getRangeAt(0);
            const node = r.startContainer;
            const off = r.startOffset;
            sel.modify('move', dir, 'character');
            const moved = sel.getRangeAt(0);
            if (moved.startContainer === node && moved.startOffset === off) break;
            const now = caretOffset();
            if (now === from) continue;
            const crossed = src.slice(Math.min(now, from), Math.max(now, from));
            if (!crossed.includes('\x1b') || stripAnsiCodes(crossed) !== '') break;
            from = now;
        }
    });

    el.addEventListener('compositionstart', () => { composing = true; });
    el.addEventListener('compositionend', () => {
        composing = false;
        // One reconcile after the IME is finished, never during.
        syncFromDom();
    });
    el.addEventListener('input', () => {
        if (composing) {
            // READ-only sync while the IME composes: the model and the empty
            // flag track every keystroke (the placeholder hides on the first
            // char, `value` readers like live previews stay letter-accurate)
            // — but NO render: a DOM write mid-composition resets the IME.
            src = readDom();
            el.dataset.empty = src === '' ? '1' : '0';
            return;
        }
        syncFromDom();
    });
    // Paste as plain text, applied to the MODEL. `execCommand('insertText')` drops
    // the newlines out of multi-line clipboard text, collapsing a pasted list into
    // one long line — and pasted HTML would inject nodes the serializer has no rule
    // for. Splicing the string keeps both problems out.
    el.addEventListener('paste', (e) => {
        e.preventDefault();
        const raw = (e.clipboardData || window.clipboardData).getData('text/plain');
        if (!raw) return;
        const text = raw.replace(/\r\n?/g, '\n');   // Windows clipboards carry CRLF
        const sel = selectionRange() || { start: src.length, end: src.length };
        src = src.slice(0, sel.start) + text + src.slice(sel.end);
        rerender(sel.start + text.length);
        el.dispatchEvent(new Event('input', { bubbles: true }));
    });
    // Copy/cut serialize through the MODEL, not Selection.toString() — that
    // API skips <img> nodes entirely, so emoji widgets would leave the
    // clipboard as bare whitespace. Model offsets already count them.
    el.addEventListener('copy', (e) => {
        const r = liveSelectionRange();
        if (!r || r.start === r.end) return;
        e.preventDefault();
        e.clipboardData.setData('text/plain', src.slice(r.start, r.end));
    });
    el.addEventListener('cut', (e) => {
        const r = liveSelectionRange();
        if (!r || r.start === r.end) return;
        e.preventDefault();
        e.clipboardData.setData('text/plain', src.slice(r.start, r.end));
        src = src.slice(0, r.start) + src.slice(r.end);
        rerender(r.start);
        el.dispatchEvent(new Event('input', { bubbles: true }));
    });

    // ---- the textarea-shaped face -------------------------------------------

    const api = {
        el,
        get value() { return src; },
        set value(v) {
            src = String(v == null ? '' : v);
            rerender(src.length);
        },
        get selectionStart() { const r = selectionRange(); return r ? r.start : src.length; },
        set selectionStart(v) { setCaret(v); },
        get selectionEnd() { const r = selectionRange(); return r ? r.end : src.length; },
        set selectionEnd(v) { setCaret(v); },
        setSelectionRange(a, _b) { setCaret(a); },
        focus() { el.focus(); },
        blur() { el.blur(); },
        addEventListener: (...a) => el.addEventListener(...a),
        removeEventListener: (...a) => el.removeEventListener(...a),
        dispatchEvent: (...a) => el.dispatchEvent(...a),
        get placeholder() { return el.dataset.placeholder || ''; },
        set placeholder(v) { el.dataset.placeholder = v; },
        // A div has no `disabled`; only switching off editing stops keystrokes landing.
        get disabled() { return el.contentEditable === 'false'; },
        set disabled(v) {
            el.contentEditable = v ? 'false' : 'true';
            el.toggleAttribute('data-locked', !!v);
            if (v) el.blur();
        },
        /** Re-run tokenisation when the emoji/mention resolvers learn something new. */
        refresh() { rerender(caretOffset()); },
    };

    // Paint the empty state once up front. Without this nothing renders until the
    // first edit or `value` write, so `data-empty` is unset and the placeholder has
    // no selector to match on a fresh boot.
    rerender();

    // Anything not part of the composer's own face falls through to the element,
    // so incidental DOM use at existing call sites (style, classList, closest,
    // getBoundingClientRect, scrollHeight…) keeps working untouched.
    return new Proxy(api, {
        get(target, key) {
            if (key in target) return target[key];
            const v = el[key];
            return typeof v === 'function' ? v.bind(el) : v;
        },
        set(target, key, value) {
            if (key in target) target[key] = value;
            else el[key] = value;
            return true;
        },
        has(target, key) { return key in target || key in el; },
    });
}

if (typeof window !== 'undefined') {
    window.createRichComposer = createRichComposer;
    // The send-time conversion has to fold names the same way the preview does,
    // or one of them tags where the other doesn't.
    window.cmpFold = cmpFold;
    window.cmpNamePattern = cmpNamePattern;
}
