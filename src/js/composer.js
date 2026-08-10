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
    const safe = (fn, arg) => {
        if (!fn) return null;
        try { return fn(arg); } catch (_) { return null; }
    };
    const out = [];
    // Order matters: code first (its content is literal), emoji before mention
    // so `:a:` inside a name can't be swallowed.
    // Emoji goes LAST: `*️⃣` and `*italic*` both start with `*`, and the engine only
    // falls through to a later alternative once the earlier one fails to match.
    const re = new RegExp(
        '(`[^`\\n]+`)|(\\*\\*[^*\\n]+\\*\\*)|(~~[^~\\n]+~~)|(\\|\\|[^|\\n]+\\|\\|)'
        // The npub form comes before the name form: a pasted `@npub1…` is all
        // name-shaped characters, so the greedy name rule would swallow it and
        // leave 63 characters of key on screen.
        // Display names contain spaces ("Walter White"), so that run is captured
        // greedily and the resolver decides how much of it is actually a name.
        + '|(\\*[^*\\n]+\\*)|(:[a-zA-Z0-9_~-]+:)'
        + '|(@npub1[023456789acdefghjklmnpqrstuvwxyz]{58})'
        + '|(@[\\p{L}\\p{N}_.\\- ]{1,64})'
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
        else if (m[2]) out.push({ kind: 'bold', from, to, mark: 2 });
        else if (m[3]) out.push({ kind: 'strike', from, to, mark: 2 });
        else if (m[4]) out.push({ kind: 'spoiler', from, to, mark: 2 });
        else if (m[5]) out.push({ kind: 'italic', from, to, mark: 1 });
        else if (m[6]) {
            // Only an emoji the app can actually resolve becomes a widget; an
            // unknown `:word:` stays literal text so it can still be typed through.
            const url = safe(opts.resolveEmoji, raw.slice(1, -1));
            out.push(url ? { kind: 'emoji', from, to, url } : { kind: 'text', from, to });
        } else if (m[7]) {
            // A mention pasted from a message carries the raw key, which is what
            // gets sent. Show the person's name over it; `data-src` keeps the npub.
            const label = safe(opts.resolveNpub, raw.slice(1));
            out.push(label ? { kind: 'npubmention', from, to, label } : { kind: 'text', from, to });
        } else if (m[8]) {
            // The resolver returns the tracked name this run STARTS with, which is
            // usually shorter than the greedy capture — the pill has to end at the
            // name, not swallow the words after it.
            const known = safe(opts.resolveMention, raw.slice(1));
            if (known && known.length <= raw.length - 1) {
                const end = from + 1 + known.length;
                out.push({ kind: 'mention', from, to: end });
                last = end;
                re.lastIndex = end;
                continue;
            }
            out.push({ kind: 'text', from, to });
        } else if (m[9]) {
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
    return out;
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
        if (t.kind === 'emoji' || t.kind === 'twemoji') s += '(' + src.slice(t.from, t.to) + ')';
        s += '|';
    }
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
                case 'italic': el.appendChild(decorated('cmp-italic', raw, 1)); break;
                case 'strike': el.appendChild(decorated('cmp-strike', raw, 2)); break;
                case 'spoiler': el.appendChild(decorated('cmp-spoiler', raw, 2)); break;
                case 'code': el.appendChild(decorated('cmp-code', raw, 1)); break;
                case 'listmark':
                    // Recede it like any other marker. The bullet itself is the
                    // renderer's job; here the point is that the line WILL format.
                    el.appendChild(span('cmp-mark', raw));
                    break;
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
                case 'twemoji': {
                    const w = document.createElement('span');
                    w.className = 'cmp-twemoji';
                    w.contentEditable = 'false';
                    w.dataset.src = raw;
                    const img = document.createElement('img');
                    // Always a BUNDLED asset path from `cmpTwemojiUrl` (/twemoji/svg/…),
                    // never a network URL — unlike pack art, which must go through the host.
                    img.src = t.url;
                    img.alt = raw;
                    // Second safety net: artwork the manifest claims but the build
                    // doesn't ship degrades to the character rather than a broken icon.
                    img.addEventListener('error', () => {
                        w.replaceWith(document.createTextNode(raw));
                    }, { once: true });
                    w.appendChild(img);
                    el.appendChild(document.createTextNode(CMP_ZWSP));
                    el.appendChild(w);
                    el.appendChild(document.createTextNode(CMP_ZWSP));
                    break;
                }
                case 'emoji': {
                    // The one real widget: an image's metrics can't match `:code:`.
                    const w = document.createElement('span');
                    w.className = 'cmp-emoji';
                    w.contentEditable = 'false';
                    w.dataset.src = raw;
                    const img = document.createElement('img');
                    img.alt = raw;
                    w.appendChild(img);
                    el.appendChild(document.createTextNode(CMP_ZWSP));
                    el.appendChild(w);
                    el.appendChild(document.createTextNode(CMP_ZWSP));
                    // Appended first: the fallback replaces the widget, which needs a parent.
                    const fail = () => { if (w.parentNode) w.replaceWith(document.createTextNode(raw)); };
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
        if (!el.firstChild) el.appendChild(document.createTextNode(''));
        // `:empty` can't drive the placeholder — the root always holds a text node.
        el.dataset.empty = src === '' ? '1' : '0';
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
     * Both ends of the selection as model offsets. `selectionStart` used to
     * report the END too, so anything that replaces a selection — paste, the
     * picker's insertAtCursor — inserted beside the highlighted text instead of
     * over it.
     */
    function selectionRange() {
        const sel = window.getSelection();
        if (!sel || !sel.rangeCount) return null;
        const r = sel.getRangeAt(0);
        const start = offsetOfPoint(r.startContainer, r.startOffset);
        const end = offsetOfPoint(r.endContainer, r.endOffset);
        if (start === null || end === null) return null;
        return start <= end ? { start, end } : { start: end, end: start };
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
    function syncFromDom() {
        const next = readDom();
        src = next;
        const tokens = cmpTokenize(src, opts);
        const sig = cmpSignature(tokens, src);
        if (sig === signature) return;
        signature = sig;
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

    el.addEventListener('compositionstart', () => { composing = true; });
    el.addEventListener('compositionend', () => {
        composing = false;
        // One reconcile after the IME is finished, never during.
        syncFromDom();
    });
    el.addEventListener('input', () => {
        if (composing) return;   // NEVER touch the DOM mid-composition
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
    el.addEventListener('copy', (e) => {
        const sel = window.getSelection();
        if (!sel || sel.isCollapsed) return;
        e.preventDefault();
        e.clipboardData.setData('text/plain', sel.toString().split(CMP_ZWSP).join(''));
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

if (typeof window !== 'undefined') window.createRichComposer = createRichComposer;
