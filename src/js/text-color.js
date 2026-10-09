/**
 * Coloured text outside code blocks: the JS half of `vector_core::text_color`.
 *
 * Core takes the markup out on send and carries each span as a `["color", …]` tag;
 * here the composer previews the markup, a received message paints its spans, and
 * an edit puts the markup back. The scanner mirrors the Rust one rule for rule, so
 * the preview can't promise colour the send won't keep.
 */

/**
 * The spelling the UI offers, from the reader's first English locale: `color` in the
 * US and its territories (and with no English at all), `colour` elsewhere. Both are
 * always understood, typed as a command or as a tag.
 */
const TC_COLOR_WORD = (() => {
    const langs = globalThis.navigator?.languages?.length ? navigator.languages : [globalThis.navigator?.language || ''];
    const english = langs.find((l) => /^en(-|$)/i.test(l || ''));
    if (!english || /^en$/i.test(english)) return 'color';
    return /^en-(US|PH|PR|UM|AS|GU|VI|LR)\b/i.test(english) ? 'color' : 'colour';
})();

// Mirrors crates/vector-core/src/text_color_names.json (the test checks it).
const TC_NAMED = {
    aliceblue: '#f0f8ff', amber: '#fbbf24', antiquewhite: '#faebd7', aqua: '#00ffff',
    aquamarine: '#7fffd4', azure: '#f0ffff', beige: '#f5f5dc', bisque: '#ffe4c4',
    black: '#000000', blanchedalmond: '#ffebcd', blue: '#60a5fa', blueviolet: '#8a2be2',
    blush: '#f9a8d4', brick: '#cb4154', brown: '#a52a2a', burlywood: '#deb887',
    cadetblue: '#5f9ea0', chartreuse: '#7fff00', cherry: '#de3163', chocolate: '#d2691e',
    copper: '#d97706', coral: '#ff7f50', cornflowerblue: '#6495ed', cornsilk: '#fff8dc',
    cream: '#fef3c7', crimson: '#dc143c', cyan: '#22d3ee', darkblue: '#00008b',
    darkcyan: '#008b8b', darkgoldenrod: '#b8860b', darkgray: '#a9a9a9', darkgreen: '#006400',
    darkgrey: '#a9a9a9', darkkhaki: '#bdb76b', darkmagenta: '#8b008b', darkolivegreen: '#556b2f',
    darkorange: '#ff8c00', darkorchid: '#9932cc', darkred: '#8b0000', darksalmon: '#e9967a',
    darkseagreen: '#8fbc8f', darkslateblue: '#483d8b', darkslategray: '#2f4f4f', darkslategrey: '#2f4f4f',
    darkturquoise: '#00ced1', darkviolet: '#9400d3', deeppink: '#ff1493', deepskyblue: '#00bfff',
    dimgray: '#696969', dimgrey: '#696969', dodgerblue: '#1e90ff', emerald: '#34d399',
    firebrick: '#b22222', floralwhite: '#fffaf0', forest: '#228b22', forestgreen: '#228b22',
    fuchsia: '#ff00ff', gainsboro: '#dcdcdc', ghostwhite: '#f8f8ff', gold: '#fbbf24',
    goldenrod: '#daa520', gray: '#9ca3af', green: '#4ade80', greenyellow: '#adff2f',
    grey: '#9ca3af', honeydew: '#f0fff0', hotpink: '#ff69b4', indianred: '#cd5c5c',
    indigo: '#818cf8', ivory: '#fffff0', jade: '#00a86b', khaki: '#f0e68c',
    lavender: '#e6e6fa', lavenderblush: '#fff0f5', lawngreen: '#7cfc00', lemon: '#fde047',
    lemonchiffon: '#fffacd', lightblue: '#add8e6', lightcoral: '#f08080', lightcyan: '#e0ffff',
    lightgoldenrodyellow: '#fafad2', lightgray: '#d3d3d3', lightgreen: '#90ee90', lightgrey: '#d3d3d3',
    lightpink: '#ffb6c1', lightsalmon: '#ffa07a', lightseagreen: '#20b2aa', lightskyblue: '#87cefa',
    lightslategray: '#778899', lightslategrey: '#778899', lightsteelblue: '#b0c4de', lightyellow: '#ffffe0',
    lilac: '#d8b4fe', lime: '#a3e635', limegreen: '#32cd32', linen: '#faf0e6',
    magenta: '#e879f9', maroon: '#800000', mediumaquamarine: '#66cdaa', mediumblue: '#0000cd',
    mediumorchid: '#ba55d3', mediumpurple: '#9370db', mediumseagreen: '#3cb371', mediumslateblue: '#7b68ee',
    mediumspringgreen: '#00fa9a', mediumturquoise: '#48d1cc', mediumvioletred: '#c71585', midnightblue: '#191970',
    mint: '#6ee7b7', mintcream: '#f5fffa', mistyrose: '#ffe4e1', moccasin: '#ffe4b5',
    navajowhite: '#ffdead', navy: '#000080', ocean: '#0ea5e9', oldlace: '#fdf5e6',
    olive: '#808000', olivedrab: '#6b8e23', orange: '#f59e42', orangered: '#ff4500',
    orchid: '#da70d6', palegoldenrod: '#eee8aa', palegreen: '#98fb98', paleturquoise: '#afeeee',
    palevioletred: '#db7093', papayawhip: '#ffefd5', peach: '#fdba74', peachpuff: '#ffdab9',
    peru: '#cd853f', pink: '#f472b6', plum: '#dda0dd', powderblue: '#b0e0e6',
    purple: '#a78bfa', rebeccapurple: '#663399', red: '#f04848', rose: '#fb7185',
    rosybrown: '#bc8f8f', royalblue: '#4169e1', ruby: '#e11d48', rust: '#b7410e',
    saddlebrown: '#8b4513', salmon: '#fa8072', sand: '#e5c69b', sandybrown: '#f4a460',
    sapphire: '#2563eb', seagreen: '#2e8b57', seashell: '#fff5ee', sienna: '#a0522d',
    silver: '#c0c0c0', sky: '#38bdf8', skyblue: '#87ceeb', slateblue: '#6a5acd',
    slategray: '#708090', slategrey: '#708090', snow: '#fffafa', springgreen: '#00ff7f',
    steelblue: '#4682b4', tan: '#d2b48c', teal: '#2dd4bf', thistle: '#d8bfd8',
    tomato: '#ff6347', turquoise: '#40e0d0', violet: '#c084fc', wheat: '#f5deb3',
    white: '#ffffff', whitesmoke: '#f5f5f5', yellow: '#f5d442', yellowgreen: '#9acd32',
};
// The named colours a command's hint offers as one-tap swatches.
const TC_CHIP_COLORS = ['red', 'orange', 'yellow', 'green', 'teal', 'blue', 'purple', 'pink', 'brown', 'coral', 'mint', 'white'];
const TC_MAX_STOPS = 6;
const TC_MAX_SPANS = 64;

// ASCII whitespace and case only, as the Rust side reads them.
const tcTrim = (s) => s.replace(/^[ \t\n\r\f]+|[ \t\n\r\f]+$/g, '');
const tcLower = (s) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());

/** A named colour or `#rgb` / `#rrggbb`, as lowercase `#rrggbb`; null otherwise. */
function tcHex(s) {
    s = tcTrim(s || '');
    if (s.startsWith('#')) {
        const h = s.slice(1);
        if (!/^[0-9a-fA-F]+$/.test(h)) return null;
        if (h.length === 6) return '#' + tcLower(h);
        if (h.length === 3) return '#' + tcLower([...h].map((c) => c + c).join(''));
        return null;
    }
    const name = tcLower(s);
    return Object.hasOwn(TC_NAMED, name) ? TC_NAMED[name] : null;
}

function tcAttr(args, name) {
    const lower = tcLower(args);
    let search = 0;
    for (;;) {
        const at = lower.indexOf(name, search);
        if (at < 0) return null;
        search = at + name.length;
        if (at > 0 && !/[ \t\n\r\f]/.test(lower[at - 1])) continue;
        let rest = args.slice(search).replace(/^[ \t\n\r\f]+/, '');
        if (!rest.startsWith('=')) continue;
        rest = rest.slice(1).replace(/^[ \t\n\r\f]+/, '');
        const q = rest[0];
        if (q === '"' || q === "'") {
            const end = rest.indexOf(q, 1);
            return end < 0 ? null : rest.slice(1, end);
        }
        return rest.split(/[ \t\n\r\f]/)[0] || null;
    }
}

function tcSpec(name, args) {
    if (name === 'rainbow') return tcTrim(args) ? null : { effect: 'rainbow', colors: [] };
    if (name === 'gradient') {
        const stops = tcTrim(args).split(/[ \t\n\r\f]+/).filter(Boolean).map(tcHex);
        if (stops.some((s) => !s) || stops.length < 2 || stops.length > TC_MAX_STOPS) return null;
        return { effect: 'gradient', colors: stops };
    }
    if (name === 'color') {
        const arg = tcTrim(args).replace(/^=+/, '').replace(/^["']+|["']+$/g, '');
        const hex = tcHex(arg);
        return hex ? { effect: 'solid', colors: [hex] } : null;
    }
    const value = tcAttr(args, 'data-mx-color') ?? tcAttr(args, 'color');
    const hex = value != null ? tcHex(value) : null;
    return hex ? { effect: 'solid', colors: [hex] } : null;
}

function tcName(s) {
    s = tcLower(s);
    if (s === 'colour') return 'color';
    return ['rainbow', 'gradient', 'color', 'font'].includes(s) ? s : null;
}

/** Index ranges markup must not reach into: fenced blocks and inline code. */
function tcCodeRanges(src) {
    const out = [];
    let fenceStart = -1;
    let lineStart = 0;
    while (lineStart <= src.length) {
        const nl = src.indexOf('\n', lineStart);
        const end = nl < 0 ? src.length : nl + 1;
        const line = src.slice(lineStart, nl < 0 ? src.length : nl).replace(/\r$/, '');
        const isFence = /^ {0,3}```/.test(line);
        if (fenceStart >= 0) {
            if (isFence) { out.push([fenceStart, end]); fenceStart = -1; }
        } else if (isFence) {
            fenceStart = lineStart;
        } else {
            let i = 0;
            while (i < line.length) {
                if (line[i] !== '`') { i++; continue; }
                let run = 0;
                while (line[i + run] === '`') run++;
                let j = i + run;
                let closed = -1;
                while (j < line.length) {
                    if (line[j] === '`') {
                        let r = 0;
                        while (line[j + r] === '`') r++;
                        if (r === run) { closed = j + r; break; }
                        j += r;
                    } else j++;
                }
                if (closed >= 0) { out.push([lineStart + i, lineStart + closed]); i = closed; } else i += run;
            }
        }
        if (nl < 0) break;
        lineStart = end;
    }
    if (fenceStart >= 0) out.push([fenceStart, src.length]);
    return out;
}

/**
 * Every matched pair of colour tags in `src`, as
 * `{ open: [from, to], close: [from, to], effect, colors }` in string indices,
 * ordered by where each opens. A tag without its partner stays text.
 */
function tcScan(src) {
    if (!src || src.indexOf('<') < 0) return [];
    const code = tcCodeRanges(src);
    const inCode = (i) => code.some(([s, e]) => i >= s && i < e);
    const marks = [];
    let i = 0;
    for (;;) {
        const at = src.indexOf('<', i);
        if (at < 0) break;
        i = at + 1;
        if (inCode(at)) continue;
        let close = -1;
        for (let k = at + 1; k < src.length; k++) {
            const c = src[k];
            if (c === '>' || c === '\n' || c === '<') { close = k; break; }
        }
        if (close < 0) break;
        if (src[close] !== '>') continue;
        let body = src.slice(at + 1, close);
        const closing = body.startsWith('/');
        if (closing) body = body.slice(1);
        const nameLen = (body.match(/^[a-zA-Z]*/) || [''])[0].length;
        const name = tcName(body.slice(0, nameLen));
        if (!name) continue;
        const args = body.slice(nameLen);
        if (closing) {
            if (tcTrim(args)) continue;
            marks.push({ at, end: close + 1, name, open: null });
        } else {
            if (args && !/^[ =]/.test(args)) continue;
            const spec = tcSpec(name, args);
            if (!spec) continue;
            marks.push({ at, end: close + 1, name, open: spec });
        }
        i = close + 1;
    }
    const stack = [];
    const pairs = [];
    marks.forEach((m, idx) => {
        if (m.open) { stack.push(idx); return; }
        let pos = -1;
        for (let s = stack.length - 1; s >= 0; s--) if (marks[stack[s]].name === m.name) { pos = s; break; }
        if (pos >= 0 && pos === stack.length - 1) pairs.push([stack.pop(), idx]);
    });
    return pairs
        .sort((a, b) => marks[a[0]].at - marks[b[0]].at)
        .map(([o, c]) => ({
            open: [marks[o].at, marks[o].end],
            close: [marks[c].at, marks[c].end],
            effect: marks[o].open.effect,
            colors: marks[o].open.colors,
        }));
}

/** `src` without its colour markup, and the spans in code points of what's left;
 *  markup around nothing but whitespace stays, so a send is never emptied by it. */
function tcExtractColor(src) {
    const pairs = tcScan(src);
    if (!pairs.length) return { plain: src, spans: [] };
    const cuts = pairs.flatMap((p) => [p.open, p.close]).sort((a, b) => a[0] - b[0]);
    let plain = '';
    let pos = 0;
    let points = 0;
    const pointAt = new Map();
    for (const [s, e] of cuts) {
        const piece = src.slice(pos, s);
        plain += piece;
        points += [...piece].length;
        pointAt.set(s, points);
        pointAt.set(e, points);
        pos = e;
    }
    plain += src.slice(pos);
    if (!tcTrim(plain)) return { plain: src, spans: [] };
    const spans = pairs
        .map((p) => ({ kind: 'color', from: pointAt.get(p.open[1]), to: pointAt.get(p.close[0]), effect: p.effect, colors: p.colors }))
        .filter((s) => s.from < s.to)
        .slice(0, TC_MAX_SPANS);
    return { plain, spans };
}

/**
 * All the markup out of `src`, as core takes it at send: colour tags, then `<t:…>` times,
 * each replaced by its fixed UTC text. Spans count code points of the result.
 */
function tcExtract(src) {
    const { plain, spans: colors } = tcExtractColor(src);
    // A code split by colour markup isn't one: its edges would fall inside the time.
    let seen = 0;
    let seenPoints = 0;
    const tokens = ttTokens(plain).filter((t) => {
        seenPoints += [...plain.slice(seen, t.at)].length;
        seen = t.at;
        const s = seenPoints;
        const e = s + [...plain.slice(t.at, t.end)].length;
        return !colors.some((c) => (c.from > s && c.from < e) || (c.to > s && c.to < e));
    });
    if (!tokens.length) return { plain, spans: colors };
    let out = '';
    let pos = 0;
    let points = 0;
    let delta = 0;
    const shifts = [];
    const times = [];
    for (const t of tokens) {
        const before = plain.slice(pos, t.at);
        out += before;
        points += [...before].length;
        const tokenPoints = [...plain.slice(t.at, t.end)].length;
        const text = ttFallback(t.unix);
        const textPoints = [...text].length;
        times.push({ kind: 'time', from: points, to: points + textPoints, unix: t.unix, style: t.style });
        out += text;
        const originalEnd = points - delta + tokenPoints;
        delta += textPoints - tokenPoints;
        shifts.push([originalEnd, delta]);
        points += textPoints;
        pos = t.end;
    }
    out += plain.slice(pos);
    const shift = (p) => p + ([...shifts].reverse().find(([at]) => p >= at)?.[1] ?? 0);
    return { plain: out, spans: [...colors.map((c) => ({ ...c, from: shift(c.from), to: shift(c.to) })), ...times] };
}

/** `content` with its spans written back as markup, for the editor: colour tags and `<t:…>` codes. */
function tcRestore(content, spans) {
    if (!spans || !spans.length) return content;
    const times = new Map(spans.filter((s) => s.kind === 'time').map((s) => [s.from, s]));
    // A colour edge inside a time moves to its border, where the code leaves room for a tag.
    const inside = (p) => [...times.values()].find((t) => p > t.from && p < t.to);
    spans = spans.filter((s) => s.kind !== 'time').map((s) => ({ ...s, from: inside(s.from)?.from ?? s.from, to: inside(s.to)?.to ?? s.to }));
    const points = [...content];
    // The shortest name for a colour reads best back in the editor (by length, then a-z).
    const named = (hex) => Object.keys(TC_NAMED).filter((k) => TC_NAMED[k] === hex)
        .sort((a, b) => a.length - b.length || (a < b ? -1 : 1))[0] || hex;
    const openTag = (s) => s.effect === 'rainbow' ? '<rainbow>'
        : s.effect === 'gradient' ? `<gradient ${s.colors.map(named).join(' ')}>`
        : `<${TC_COLOR_WORD} ${named(s.colors[0])}>`;
    const closeTag = (s) => s.effect === 'solid' ? `</${TC_COLOR_WORD}>` : `</${s.effect}>`;
    // Outer spans open first and close last, so nested markup re-reads as it was written.
    const order = spans.map((s, i) => ({ ...s, i })).sort((a, b) => a.from - b.from || b.to - a.to || a.i - b.i);
    const opens = new Map();
    const closes = new Map();
    for (const s of order) {
        if (!opens.has(s.from)) opens.set(s.from, []);
        opens.get(s.from).push(openTag(s));
        if (!closes.has(s.to)) closes.set(s.to, []);
        closes.get(s.to).unshift(closeTag(s));
    }
    let out = '';
    for (let i = 0; i <= points.length; i++) {
        if (closes.has(i)) out += closes.get(i).join('');
        if (opens.has(i)) out += opens.get(i).join('');
        const time = times.get(i);
        if (time && points.slice(time.from, time.to).join('') === ttFallback(time.unix)) {
            out += `<t:${time.unix}:${time.style}>`;
            i = time.to - 1;
            continue;
        }
        if (i < points.length) out += points[i];
    }
    return out;
}

// ---- colour maths ----------------------------------------------------------------

function tcRgb(hex) {
    return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
}

function tcLuminance(rgb) {
    const f = (c) => { c /= 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
    return 0.2126 * f(rgb[0]) + 0.7152 * f(rgb[1]) + 0.0722 * f(rgb[2]);
}

// The chat's near-black: every colour is lifted toward white until it reads against it.
const TC_BG_LUMINANCE = tcLuminance([16, 16, 16]);
const tcReadableMemo = new Map();

function tcReadable(rgb) {
    const key = (rgb[0] << 16) | (rgb[1] << 8) | rgb[2];
    let out = tcReadableMemo.get(key);
    if (out) return out;
    let c = rgb;
    for (let t = 0; t <= 1.0001; t += 0.05) {
        c = rgb.map((v) => Math.round(v + (255 - v) * t));
        if ((tcLuminance(c) + 0.05) / (TC_BG_LUMINANCE + 0.05) >= 4.5) break;
    }
    out = `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
    if (tcReadableMemo.size < 4096) tcReadableMemo.set(key, out);
    return out;
}

function tcHsl(h, s, l) {
    const a = s * Math.min(l, 1 - l);
    const f = (n) => { const k = (n + h / 30) % 12; return l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1)); };
    return [f(0), f(8), f(4)].map((v) => Math.round(v * 255));
}

// A long run shares each colour across neighbouring glyphs, so it paints as at most
// this many runs instead of one element per glyph.
const TC_STEPS = 128;

/** The colour of glyph `i` of `n` in a span, already floored for readability. */
function tcColorAt(span, i, n) {
    if (span.effect === 'solid') return tcReadable(tcRgb(span.colors[0]));
    if (span.effect === 'rainbow') return tcReadable(tcHsl((360 * i) / Math.max(n, 1), 1, 0.6));
    const stops = span.colors.map(tcRgb);
    const t = n > 1 ? i / (n - 1) : 0;
    const seg = Math.min(stops.length - 2, Math.floor(t * (stops.length - 1)));
    const local = t * (stops.length - 1) - seg;
    const [a, b] = [stops[seg], stops[seg + 1]];
    return tcReadable(a.map((v, k) => Math.round(v + (b[k] - v) * local)));
}

/** Glyph `i` of `n` → its colour; a long span shares each of TC_STEPS colours across neighbours. */
function tcPalette(span, n) {
    if (span.effect === 'solid') {
        const c = tcColorAt(span, 0, 1);
        return () => c;
    }
    const steps = Math.min(n, TC_STEPS);
    const colors = [];
    for (let b = 0; b < steps; b++) colors.push(tcColorAt(span, n > steps ? Math.floor((b * n) / steps) : b, n));
    return (i) => colors[Math.min(steps - 1, n > steps ? Math.floor((i * steps) / n) : i)];
}

/** Per-glyph colours for a run of `n` glyphs. */
function tcColors(span, n) {
    const at = tcPalette(span, n);
    return Array.from({ length: n }, (_, i) => at(i));
}

// ---- painting a received message ---------------------------------------------------

// Private-use sentinels, one pair per span, carried through markdown as text.
const TC_OPEN = 0xF0000;
const TC_SHUT = 0xF0100;
const TC_SENTINEL = /[\u{F0000}-\u{F01FF}]/u;
const TC_SENTINELS = /[\u{F0000}-\u{F01FF}]/gu;
const TC_UNPAINTED = 'a, code, pre, .mention, .spoiler, .spoiler-wrapper, .golink, .custom-emoji-inline, .vt-time';
// Runs a span edge must never split: links in any form, nostr references, emoji
// shortcodes, HTML entities, inline code and escape codes. A sentinel inside one would
// show the link guards and the linkifier different text from the reader's.
// Each run's class excludes its own opener and is bounded, so no input backtracks
// quadratically.
const TC_ATOMIC = /!?\[[^[\]\n]{0,512}\]\([^()\n]{0,2048}\)|<[^<>\s]{1,2048}>|(?:https?:\/\/|www\.)[^\s<]+|(?:nostr:|@)?(?:npub1|nprofile1|naddr1|nevent1|note1)[02-9ac-hj-np-z]+|:[a-zA-Z0-9_~-]+:|&#?[a-zA-Z0-9]+;|`+[^`\n]*`+|\x1b\[[\x20-\x3f]*[\x40-\x7e]?/g;
const TC_RULE = /^[ \t]{0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;

/**
 * `text` with a sentinel at each span edge, or null when there is nothing to mark or
 * the text already holds sentinel code points (it then renders uncoloured). Edges
 * move off anything a sentinel would break: a start passes a line's block prefix, an
 * end before one steps back a line, neither lands in a fenced block, a rule or a
 * table row, and neither touches an atomic run.
 */
function tcMarkSpans(text, spans) {
    const times = (spans || []).filter((s) => s.kind === 'time');
    spans = (spans || []).filter((s) => s.kind !== 'time');
    if (!spans.length || TC_SENTINEL.test(text)) return null;
    const points = [...text];
    const n = points.length;
    // Code-point offset → string index, and back.
    const index = new Array(n + 1);
    for (let i = 0, at = 0; i <= n; i++) { index[i] = at; if (i < n) at += points[i].length; }
    const pointOf = new Map(index.map((at, i) => [at, i]));
    const fences = tcCodeRanges(text).filter(([s]) => /^ {0,3}```/.test(text.slice(s, s + 8)));
    // A time's text is replaced by its chip later, so it must reach the DOM in one piece.
    const atomic = [...text.matchAll(TC_ATOMIC)].map((m) => [m.index, m.index + m[0].length])
        .concat(times.filter((t) => t.to <= n).map((t) => [index[t.from], index[t.to]]));
    const lineStart = (at) => at === 0 || text[at - 1] === '\n';
    const lineOf = (at) => {
        const s = text.lastIndexOf('\n', at - 1) + 1;
        const e = text.indexOf('\n', at);
        return [s, e < 0 ? text.length : e];
    };
    // A sentinel touching a URL joins it for the linkifier, so an edge in or against
    // an atomic run moves past the whole word around it, and the blanks beside it.
    const inAtom = (at) => atomic.some(([s, e]) => at >= s && at <= e);
    const blank = (c) => c === ' ' || c === '\t';
    // Beside a delimiter a sentinel counts as a letter, which changes whether the
    // delimiter opens or closes; it is harmless only with a word character on its
    // other side (`**|bold`, `it|_`). After a backslash it breaks the escape.
    const SIGNIFICANT = /[*_~|\\`\[\]()<>!#]/;
    const word = (c) => c !== undefined && !/\s/.test(c) && !SIGNIFICANT.test(c);
    const wedged = (at) => text[at - 1] === '\\'
        || (SIGNIFICANT.test(text[at - 1] ?? '') && !word(text[at]))
        || (SIGNIFICANT.test(text[at] ?? '') && !word(text[at - 1]));
    // Where on its line a sentinel can't go: a block prefix (`#`, `>`, list marker),
    // or the whole line for a rule or a table row, whose shape a stray code point breaks.
    const zoneOf = (at) => {
        const [s, e] = lineOf(at);
        const line = text.slice(s, e);
        if (TC_RULE.test(line) || /^[ \t]{0,3}\|(?!\|)/.test(line)) return { s, e, whole: true };
        const m = /^[ \t]{0,3}(?:(?:#{1,6}|-#|>|[-*]|\d{1,9}\.)[ \t]+|>[ \t]?)*/.exec(line.slice(0, 64));
        return { s, e: s + (m ? m[0].length : 0), whole: false };
    };
    const startRules = (at) => {
        for (const [s, e] of fences) if (at >= s && at < e) at = e;
        // Past every blank, rule or table line in a row: alone on a blank line a
        // sentinel would be its only content, and the others can't hold one.
        for (;;) {
            if (at >= text.length) return text.length;
            const [ls, le] = lineOf(at);
            if (text.slice(ls, le).trim() && !zoneOf(at).whole) break;
            at = Math.min(le + 1, text.length);
        }
        const z = zoneOf(at);
        if (at >= z.s && at < z.e) at = z.e;
        if (inAtom(at)) {
            while (at < text.length && !/\s/.test(text[at])) at++;
            while (blank(text[at])) at++;
        }
        while (at < text.length && wedged(at)) at++;
        return at;
    };
    const endRules = (at) => {
        for (const [s, e] of fences) if (at > s && at <= e) at = s;
        // Back over every line it can't sit on, in one pass.
        for (;;) {
            const z = zoneOf(at);
            if (at === 0 || !(z.whole || at === z.s || at < z.e)) break;
            at = Math.max(z.s - 1, 0);
        }
        if (inAtom(at)) {
            while (at > 0 && !/\s/.test(text[at - 1])) at--;
            while (at > 0 && blank(text[at - 1])) at--;
        }
        while (at > 0 && wedged(at)) at--;
        return at;
    };
    // Each rule can expose another (a word moved past ends at a line start), so run
    // them to a fixed point. Starts only move forward and ends only back, so it settles
    // within the text's length.
    const settle = (at, rules) => {
        for (let i = 0; i <= text.length + 1; i++) {
            const next = rules(at);
            if (next === at) break;
            at = next;
        }
        return at;
    };
    const placeStart = (p) => settle(index[p], startRules);
    const placeEnd = (p) => settle(index[p], endRules);
    const inserts = [];
    spans.slice(0, TC_MAX_SPANS).forEach((span, i) => {
        if (!(span.from < span.to) || span.to > n) return;
        const from = pointOf.get(placeStart(span.from));
        const to = pointOf.get(placeEnd(span.to));
        if (from === undefined || to === undefined || from >= to) return;
        inserts.push([from, 1, String.fromCodePoint(TC_OPEN + i)]);
        inserts.push([to, 0, String.fromCodePoint(TC_SHUT + i)]);
    });
    if (!inserts.length) return null;
    // Ends before starts at one offset, so adjacent spans never overlap by a glyph.
    inserts.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    let out = '';
    let k = 0;
    for (let i = 0; i <= n; i++) {
        while (k < inserts.length && inserts[k][0] === i) out += inserts[k++][2];
        if (i < n) out += points[i];
    }
    return out;
}

/**
 * Turn the sentinels markdown carried through into empty marker elements. Any
 * that markdown moved into an attribute are scrubbed there; that span edge simply
 * goes unmarked.
 */
function tcSentinelsToMarkers(root) {
    for (const el of root.querySelectorAll('*')) {
        for (const attr of [...el.attributes]) {
            if (TC_SENTINEL.test(attr.value)) el.setAttribute(attr.name, attr.value.replace(TC_SENTINELS, ''));
        }
    }
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) if (TC_SENTINEL.test(walker.currentNode.nodeValue)) nodes.push(walker.currentNode);
    for (const node of nodes) {
        const frag = document.createDocumentFragment();
        let last = 0;
        const value = node.nodeValue;
        for (const m of value.matchAll(TC_SENTINELS)) {
            if (m.index > last) frag.appendChild(document.createTextNode(value.slice(last, m.index)));
            const cp = m[0].codePointAt(0);
            const marker = document.createElement('i');
            marker.className = 'tc-marker';
            marker.dataset.tc = cp >= TC_SHUT ? 'e' + (cp - TC_SHUT) : 's' + (cp - TC_OPEN);
            frag.appendChild(marker);
            last = m.index + m[0].length;
        }
        if (last < value.length) frag.appendChild(document.createTextNode(value.slice(last)));
        node.replaceWith(frag);
    }
}

/**
 * Colour the text between each span's markers, glyph by glyph, then drop the
 * markers. Links, code, mentions, spoilers and custom emoji keep their own look.
 * Where spans overlap, the later one paints.
 */
function tcPaint(root, spans) {
    spans = (spans || []).filter((s) => s.kind !== 'time');
    if (!spans.length) return;
    const runs = [];
    const active = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    while (walker.nextNode()) {
        const node = walker.currentNode;
        if (node.nodeType === Node.ELEMENT_NODE) {
            if (!node.classList.contains('tc-marker')) continue;
            const tag = node.dataset.tc;
            const i = Number(tag.slice(1));
            if (tag[0] === 's') active.push(i);
            else { const at = active.lastIndexOf(i); if (at >= 0) active.splice(at, 1); }
            continue;
        }
        if (!active.length || !node.nodeValue || node.parentElement.closest(TC_UNPAINTED)) continue;
        runs.push({ node, active: [...active] });
    }
    for (const m of root.querySelectorAll('.tc-marker')) m.remove();
    if (!runs.length) return;

    const seg = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
    const glyphs = runs.map((r) => [...seg.segment(r.node.nodeValue)].map((g) => g.segment));
    // A span is active over one unbroken stretch of these runs, so each glyph's place in
    // it is its place overall less where the span began: an outer gradient flows on
    // under an inner span. Only the spans that paint somewhere build a palette.
    const first = new Map();
    const last = new Map();
    const painting = new Set();
    let seen = 0;
    runs.forEach((r, k) => {
        for (const i of r.active) if (!first.has(i)) first.set(i, seen);
        seen += glyphs[k].filter((g) => /\S/.test(g)).length;
        for (const i of r.active) last.set(i, seen);
        painting.add(Math.max(...r.active));
    });
    const palettes = new Map();
    for (const i of painting) if (spans[i]) palettes.set(i, tcPalette(spans[i], last.get(i) - first.get(i)));
    let glyph = 0;

    runs.forEach((r, k) => {
        const top = Math.max(...r.active);
        const palette = palettes.get(top);
        if (!palette) {
            glyph += glyphs[k].filter((g) => /\S/.test(g)).length;
            return;
        }
        const frag = document.createDocumentFragment();
        let run = null;
        let runColor = null;
        for (const g of glyphs[k]) {
            if (!/\S/.test(g)) {
                if (run) run.textContent += g; else frag.appendChild(document.createTextNode(g));
                continue;
            }
            const color = palette(glyph - first.get(top));
            glyph++;
            if (run && color === runColor) { run.textContent += g; continue; }
            run = document.createElement('span');
            run.style.color = color;
            run.textContent = g;
            runColor = color;
            frag.appendChild(run);
        }
        r.node.replaceWith(frag);
    });
}

// ---- system commands -----------------------------------------------------------------

/**
 * A leading colour command: `/rainbow text`, `/gradient a b text`, `/color c text`
 * (or `/colour`). Returns `{ name, typed, prefixEnd, effect, colors }`, `{ name, typed, error }`
 * when the command is right but its arguments aren't, or null when `src` isn't one.
 */
function tcCommand(src) {
    const m = /^\/(rainbow|gradient|colou?r)(?=\s|$)/.exec(src || '');
    if (!m) return null;
    const typed = m[1];
    const name = typed.startsWith('colo') ? 'color' : typed;
    const want = name === 'gradient' ? 2 : name === 'color' ? 1 : 0;
    let at = m[0].length;
    const colors = [];
    for (let k = 0; k < want; k++) {
        const arg = /^\s+(\S+)/.exec(src.slice(at));
        const hex = arg && tcHex(arg[1]);
        if (!hex) {
            const error = name === 'gradient'
                ? `needs two ${TC_COLOR_WORD}s, like /gradient pink #60a5fa text`
                : `needs a ${TC_COLOR_WORD}, like /${TC_COLOR_WORD} pink text`;
            return { name, typed, error };
        }
        colors.push(hex);
        at += arg[0].length;
    }
    const gap = /^\s+/.exec(src.slice(at));
    if (!gap || !src.slice(at + gap[0].length).trim()) return { name, typed, error: `needs some text to ${TC_COLOR_WORD}` };
    at += gap[0].length;
    return { name, typed, prefixEnd: at, effect: name === 'color' ? 'solid' : name, colors };
}

/** A colour as it will actually render, for a swatch. */
function tcSwatch(hex) {
    return tcReadable(tcRgb(hex));
}

/**
 * Colour suggestions for a command argument: a spread of examples while it's empty,
 * then the names that start with (then contain) what's typed, or the typed `#hex`.
 */
function tcColorChoices(query, limit = 12) {
    const q = tcLower(tcTrim(query || ''));
    if (!q) return TC_CHIP_COLORS.map((name) => ({ value: name, color: tcSwatch(TC_NAMED[name]) }));
    if (q.startsWith('#')) {
        const hex = tcHex(q);
        return hex ? [{ value: q, color: tcSwatch(hex) }] : [];
    }
    const names = Object.keys(TC_NAMED);
    // The everyday names first (`br` → brown before brick), then shortest, then a-z.
    const common = (n) => (TC_CHIP_COLORS.includes(n) ? 0 : 1);
    const byLength = (a, b) => common(a) - common(b) || a.length - b.length || (a < b ? -1 : 1);
    const starts = names.filter((n) => n.startsWith(q)).sort(byLength);
    const within = names.filter((n) => !n.startsWith(q) && n.includes(q)).sort(byLength);
    return [...starts, ...within].slice(0, limit).map((name) => ({ value: name, color: tcSwatch(TC_NAMED[name]) }));
}

/**
 * A CSS gradient showing what a colour command will do, from the colour words typed
 * so far; a missing colour shows as a neutral grey. Built only from our own numbers.
 */
function tcEffectPreview(name, words) {
    const pending = 'rgb(70, 70, 70)';
    const at = (w) => (w && tcHex(w) ? tcReadable(tcRgb(tcHex(w))) : pending);
    if (name === 'rainbow') {
        const stops = Array.from({ length: 7 }, (_, i) => tcReadable(tcHsl((360 * i) / 7, 1, 0.6)));
        return `linear-gradient(90deg, ${stops.join(', ')})`;
    }
    if (name === 'gradient') return `linear-gradient(90deg, ${at(words[0])}, ${at(words[1])})`;
    if (name === 'color') return `linear-gradient(90deg, ${at(words[0])}, ${at(words[0])})`;
    return null;
}

/** A colour command rewritten as the markup core reads, or null. */
function tcCommandToMarkup(src) {
    const c = tcCommand(src);
    if (!c || c.error) return null;
    const args = c.colors.length ? ' ' + c.colors.join(' ') : '';
    const body = src.slice(c.prefixEnd);
    // A tag on a fence line would break the fence; give it its own line.
    const lead = /^ {0,3}```/.test(body) ? '\n' : '';
    const gap = /(^|\n) {0,3}```[^\n]*$/.test(body) ? '\n' : '';
    return `<${c.name}${args}>${lead}${body}${gap}</${c.name}>`;
}
