/**
 * Times that read the same everywhere: the JS half of `vector_core::text_time`.
 *
 * A message carries each time as a fixed UTC rendering plus a `time` span; here it is
 * shown in the reader's zone and locale, Discord's `<t:UNIX:STYLE>` codes are read in
 * the composer and in bridged text, and `@time` turns a typed phrase into one.
 */

const TT_STYLES = 'tTdDfFsSR';
const TT_MIN = -62167219200;
const TT_MAX = 253402300799;
// One code: `<t:UNIX>` or `<t:UNIX:STYLE>`, seconds since the epoch.
const TT_TOKEN = /<t:(-?\d{1,12})(?::([tTdDfFsSR]))?>/g;

/** The fixed UTC text a time travels as, matching core: `YYYY-MM-DD HH:MM UTC`, `:SS` when set. */
function ttFallback(unix) {
    if (!Number.isInteger(unix) || unix < TT_MIN || unix > TT_MAX) return null;
    const d = new Date(unix * 1000);
    const pad = (n, w = 2) => String(n).padStart(w, '0');
    const time = `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}` + (d.getUTCSeconds() ? `:${pad(d.getUTCSeconds())}` : '');
    return `${pad(d.getUTCFullYear(), 4)}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${time} UTC`;
}

/** Every valid code in `src` outside code: `{ at, end, unix, style }` in string indices. */
function ttTokens(src) {
    if (!src || src.indexOf('<t:') < 0) return [];
    const code = tcCodeRanges(src);
    const out = [];
    for (const m of src.matchAll(TT_TOKEN)) {
        if (code.some(([s, e]) => m.index >= s && m.index < e)) continue;
        const unix = Number(m[1]);
        if (ttFallback(unix) === null) continue;
        out.push({ at: m.index, end: m.index + m[0].length, unix, style: m[2] || 'f' });
        if (out.length === 64) break;
    }
    return out;
}

// ---- showing a time -----------------------------------------------------------------

const ttFormats = new Map();
function ttFormatter(key, options) {
    let f = ttFormats.get(key);
    if (!f) {
        f = new Intl.DateTimeFormat(undefined, options);
        ttFormats.set(key, f);
    }
    return f;
}

const TT_OPTIONS = {
    t: { hour: 'numeric', minute: '2-digit' },
    T: { hour: 'numeric', minute: '2-digit', second: '2-digit' },
    d: { year: 'numeric', month: '2-digit', day: '2-digit' },
    D: { year: 'numeric', month: 'long', day: 'numeric' },
    f: { dateStyle: 'long', timeStyle: 'short' },
    F: { dateStyle: 'full', timeStyle: 'short' },
    s: { year: 'numeric', month: '2-digit', day: '2-digit', hour: 'numeric', minute: '2-digit' },
    S: { year: 'numeric', month: '2-digit', day: '2-digit', hour: 'numeric', minute: '2-digit', second: '2-digit' },
};

let ttRelative = null;
/** "in 2 hours", "3 days ago": the unit grows with the distance, as Discord's does. Always
 *  a count, never "tomorrow", so it reads as a countdown beside the date styles. */
function ttRelativeText(unix, now = Date.now() / 1000) {
    ttRelative ??= new Intl.RelativeTimeFormat(undefined, { numeric: 'always' });
    const diff = unix - now;
    const abs = Math.abs(diff);
    // The last minute counts down by the second; "now" is the moment itself.
    if (abs < 0.5) return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(0, 'second');
    const [n, unit] = abs < 59.5 ? [diff, 'second']
        : abs < 2700 ? [diff / 60, 'minute']
        : abs < 79200 ? [diff / 3600, 'hour']
        : abs < 2246400 ? [diff / 86400, 'day']
        : abs < 28512000 ? [diff / 2592000, 'month']
        : [diff / 31536000, 'year'];
    return ttRelative.format(Math.round(n), unit);
}

/** A time in the reader's zone and locale, in one of Discord's styles. */
function ttFormat(unix, style) {
    if (style === 'R') return ttRelativeText(unix);
    return ttFormatter(style, TT_OPTIONS[style] || TT_OPTIONS.f).format(new Date(unix * 1000));
}

/** The hover text: the full date and time, with the reader's zone; just the date for a day. */
function ttFull(unix, dateOnly = false) {
    return dateOnly
        ? ttFormatter('fullDate', { dateStyle: 'full' }).format(new Date(unix * 1000))
        : ttFormatter('full', { dateStyle: 'full', timeStyle: 'long' }).format(new Date(unix * 1000));
}

function ttChip(unix, style) {
    const chip = document.createElement('span');
    chip.className = 'vt-time';
    chip.dataset.unix = String(unix);
    chip.dataset.style = style;
    chip.textContent = ttFormat(unix, style);
    if (style === 'R') ttStartTicking();
    return chip;
}

let ttTicker = 0;
/** Relative chips stay current: one shared tick for whatever is on screen, every half
 *  minute, or every second, on the second, while one is within its last minute and a half
 *  either side of now. It stops when none is left. */
function ttStartTicking() {
    if (ttTicker) return;
    const tick = () => {
        const chips = document.querySelectorAll('.vt-time[data-style="R"]');
        if (!chips.length) { ttTicker = 0; return; }
        const now = Date.now() / 1000;
        let near = false;
        for (const chip of chips) {
            const unix = Number(chip.dataset.unix);
            const text = ttRelativeText(unix, now);
            if (chip.textContent !== text) chip.textContent = text;
            if (Math.abs(unix - now) < 90) near = true;
        }
        ttTicker = setTimeout(tick, near ? 1000 - (Date.now() % 1000) + 5 : 30000);
    };
    ttTicker = setTimeout(tick, 1000 - (Date.now() % 1000) + 5);
}

const TT_SKIP = 'code, pre, a, .vt-time';

/**
 * Replace each time span's fallback text in a rendered message with its chip. The
 * fallback is found as the same occurrence it is in the content, so a matching string
 * typed elsewhere in the message is left alone; one inside code stays text.
 */
function ttRenderSpans(root, content, spans) {
    // Last first: a chip takes its text out, which would renumber the copies after it.
    const times = (spans || []).filter((s) => s.kind === 'time').sort((a, b) => b.from - a.from);
    if (!times.length) return;
    const points = [...content];
    const textNodes = () => {
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
        const out = [];
        while (walker.nextNode()) out.push(walker.currentNode);
        return out;
    };
    for (const span of times) {
        const fallback = points.slice(span.from, span.to).join('');
        if (fallback !== ttFallback(span.unix)) continue;
        const before = points.slice(0, span.from).join('');
        let occurrence = 0;
        for (let i = before.indexOf(fallback); i >= 0; i = before.indexOf(fallback, i + fallback.length)) occurrence++;
        let seen = 0;
        for (const node of textNodes()) {
            let at = node.nodeValue.indexOf(fallback);
            while (at >= 0 && seen < occurrence) {
                seen++;
                at = node.nodeValue.indexOf(fallback, at + fallback.length);
            }
            if (at < 0) continue;
            if (!node.parentElement.closest(TT_SKIP)) {
                const tail = node.splitText(at);
                tail.nodeValue = tail.nodeValue.slice(fallback.length);
                tail.parentNode.insertBefore(ttChip(span.unix, span.style), tail);
            }
            break;
        }
    }
}

/**
 * What a chip adds when hovered (or tapped on a phone), at its shortest: a countdown's
 * moment (its time within a day, else its date) or how far off a written time is.
 */
function ttHint(unix, style) {
    if (style !== 'R') return ttRelativeText(unix);
    if (Math.abs(unix - Date.now() / 1000) < 86400) return ttFormat(unix, 't');
    const at = new Date(unix * 1000);
    const thisYear = at.getFullYear() === new Date().getFullYear();
    return ttFormatter(thisYear ? 'hint' : 'hintYear', { weekday: 'short', day: 'numeric', month: 'short', ...(thisYear ? {} : { year: 'numeric' }) }).format(at);
}

(() => {
    const chipOf = (e) => e.target.closest?.('.vt-time');
    function show(chip) {
        // A spoiler's chip keeps its secret until it's revealed.
        if (chip.closest('.spoiler:not(.revealed)')) return;
        showGlobalTooltip(ttHint(Number(chip.dataset.unix), chip.dataset.style), chip);
    }
    if (matchMedia('(hover: hover) and (pointer: fine)').matches) {
        document.addEventListener('mouseover', (e) => {
            const chip = chipOf(e);
            if (chip && !chip.contains(e.relatedTarget)) show(chip);
        });
        document.addEventListener('mouseout', (e) => {
            const chip = chipOf(e);
            if (chip && !chip.contains(e.relatedTarget)) hideGlobalTooltip();
        });
        return;
    }
    // A phone taps: the bubble shows once this tap is done closing tooltips, and goes by
    // itself, or with the next touch or scroll.
    let timer = 0;
    document.addEventListener('click', (e) => {
        const chip = chipOf(e);
        if (!chip) return;
        setTimeout(() => show(chip));
        clearTimeout(timer);
        timer = setTimeout(hideGlobalTooltip, 2500);
    });
    document.addEventListener('scroll', () => hideGlobalTooltip(), { capture: true, passive: true });
})();

/** Discord codes in text that arrived without spans (a bridge, an older client): chips too. */
function ttRenderCodes(root) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) {
        const n = walker.currentNode;
        if (n.nodeValue.includes('<t:') && !n.parentElement.closest(TT_SKIP)) nodes.push(n);
    }
    let budget = 64;   // as many as a message's own spans may carry
    for (const node of nodes) {
        const value = node.nodeValue;
        const frag = document.createDocumentFragment();
        let last = 0;
        for (const m of value.matchAll(TT_TOKEN)) {
            if (!budget) break;
            const unix = Number(m[1]);
            if (ttFallback(unix) === null) continue;
            if (m.index > last) frag.appendChild(document.createTextNode(value.slice(last, m.index)));
            frag.appendChild(ttChip(unix, m[2] || 'f'));
            last = m.index + m[0].length;
            budget--;
        }
        if (!last) continue;
        if (last < value.length) frag.appendChild(document.createTextNode(value.slice(last)));
        node.replaceWith(frag);
    }
}

/** `content` with each time span shown as the reader would see it, for one-line previews. */
function ttLocalize(content, spans) {
    const times = (spans || []).filter((s) => s.kind === 'time').sort((a, b) => b.from - a.from);
    if (!times.length) return content;
    const points = [...content];
    for (const s of times) {
        if (points.slice(s.from, s.to).join('') !== ttFallback(s.unix)) continue;
        points.splice(s.from, s.to - s.from, ttFormat(s.unix, s.style));
    }
    return points.join('');
}

// ---- reading a typed time -----------------------------------------------------------

const TT_UNITS = [
    [/^(s|secs?|seconds?)$/, 1], [/^(m|mins?|minutes?)$/, 60], [/^(h|hrs?|hours?)$/, 3600],
    [/^(d|days?)$/, 86400], [/^(w|wks?|weeks?)$/, 604800], [/^(mo|mos|months?)$/, 2592000], [/^(y|yrs?|years?)$/, 31536000],
];
const TT_DAYS = ['sunday', 'monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday'];
const TT_MONTHS = ['january', 'february', 'march', 'april', 'may', 'june', 'july', 'august', 'september', 'october', 'november', 'december'];

/** "2h 30m", "an hour", "3 days and 4 hours": seconds, or null when any word isn't one. */
function ttDuration(text) {
    const clean = text.replace(/\band\b/g, ' ').replace(/(\d)([a-z])/g, '$1 $2').replace(/([a-z])(\d)/g, '$1 $2').replace(/\s+/g, ' ').trim();
    const re = /(\d+(?:\.\d+)?|half an?|an?|one|half) ([a-z]+)/g;
    let total = 0;
    for (const m of clean.matchAll(re)) {
        const amount = /^\d/.test(m[1]) ? Number(m[1]) : m[1].startsWith('half') ? 0.5 : 1;
        const unit = TT_UNITS.find(([u]) => u.test(m[2]));
        if (!unit) return null;
        total += amount * unit[1];
    }
    return total && !clean.replace(re, '').trim() ? total : null;
}

/** Day/month order for a bare `4/5`, from the reader's locale. */
function ttDayFirst() {
    const parts = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'numeric' }).formatToParts(new Date(2000, 10, 22));
    return parts.findIndex((p) => p.type === 'day') < parts.findIndex((p) => p.type === 'month');
}

/**
 * A typed time as epoch seconds, or null when it isn't one. Reads "now", epoch numbers,
 * "in 2 hours" / "3 days ago", and a date and a time in either order: today, tonight,
 * tomorrow, yesterday, weekdays (this, next or last monday), next week / month / year,
 * the weekend, 2026-12-25, 25/12, dec 25 or 25th of december with an optional year, and
 * 5pm, 5:30 pm, 17:30, noon, midnight or morning / afternoon / evening / night. A word
 * it can't place makes the whole phrase null rather than a guess.
 */
function ttParse(input, now = new Date()) {
    return ttParseFull(input, now)?.unix ?? null;
}

/** As ttParse, plus `dateOnly` when no time of day was given (it then reads as noon). */
function ttParseFull(input, now = new Date()) {
    const read = ttRead(input, now);
    if (read === null) return null;
    const out = typeof read === 'number' ? { unix: read, dateOnly: false } : read;
    return ttFallback(out.unix) === null ? null : out;
}

/** The day a week starts on in the reader's locale, 0 for Sunday. */
function ttWeekStart() {
    try {
        const locale = new Intl.Locale(new Intl.DateTimeFormat().resolvedOptions().locale);
        const info = locale.getWeekInfo?.() ?? locale.weekInfo;
        return (info?.firstDay ?? 1) % 7;
    } catch (_) {
        return 1;
    }
}

const TT_PARTS = { morning: 9, afternoon: 15, evening: 18, night: 20 };

function ttRead(input, now) {
    let text = (input || '').toLowerCase().trim().replace(/[,]+/g, ' ').replace(/\s+/g, ' ');
    if (!text || text === 'now') return Math.floor(now.getTime() / 60000) * 60;
    if (/^-?\d{9,13}$/.test(text)) {
        const n = Number(text);
        return text.replace('-', '').length === 13 ? Math.trunc(n / 1000) : n;
    }
    let m = /^in (.+)$/.exec(text);
    if (m) { const d = ttDuration(m[1]); return d === null ? null : Math.round(now.getTime() / 1000 + d); }
    m = /^(.+) (ago|from now)$/.exec(text);
    if (m) { const d = ttDuration(m[1]); return d === null ? null : Math.round(now.getTime() / 1000 + (m[2] === 'ago' ? -d : d)); }

    const date = new Date(now);
    let haveDate = false;
    let yearless = false;   // "dec 25" is the next one, so a passed date means next year
    let time = null;
    const take = (re) => {
        const found = re.exec(text);
        if (!found) return null;
        text = (text.slice(0, found.index) + ' ' + text.slice(found.index + found[0].length)).trim();
        return found;
    };

    // Time of day first: its forms are the most specific.
    let t = take(/\b(?:at )?(\d{1,2})(?::(\d{2}))?(?::(\d{2}))? ?(am|pm|a\.m\.|p\.m\.|a|p)(?![a-z.])/);
    let clock24 = false;   // read without am/pm, so "tonight" can move it to the evening
    if (t) {
        let h = Number(t[1]) % 12;
        if (t[4].startsWith('p')) h += 12;
        if (Number(t[1]) > 12 || Number(t[2] || 0) > 59) return null;
        time = [h, Number(t[2] || 0), Number(t[3] || 0)];
    } else if ((t = take(/\b(?:at )?([01]?\d|2[0-3]):([0-5]\d)(?::([0-5]\d))?\b/))) {
        time = [Number(t[1]), Number(t[2]), Number(t[3] || 0)];
        clock24 = true;
    } else if ((t = take(/\b(?:at )?(noon|midday|midnight)\b/))) {
        time = t[1] === 'midnight' ? [0, 0, 0] : [12, 0, 0];
    } else if ((t = take(/\bat (\d{1,2})\b/))) {
        if (Number(t[1]) > 23) return null;
        time = [Number(t[1]), 0, 0];
        clock24 = true;
    }
    // A part of the day sets the hour, or moves a bare "at 8" past noon.
    const part = take(/\b(?:in the |this )?(morning|afternoon|evening|night)\b/);
    if (part && !time) time = [TT_PARTS[part[1]], 0, 0];
    else if (part && clock24 && part[1] !== 'morning' && time[0] < 12) time[0] += 12;

    // A date that rolls over (Feb 30, month 13) was mistyped or misread, not meant.
    const setDate = (y, month, day) => {
        date.setFullYear(y, month - 1, day);
        return date.getMonth() === month - 1 && date.getDate() === day;
    };
    let d = take(/\b(\d{4})-(\d{1,2})-(\d{1,2})\b/);
    if (d) {
        if (!setDate(Number(d[1]), Number(d[2]), Number(d[3]))) return null;
        haveDate = true;
    } else if ((d = take(/\b(\d{1,2})[/.](\d{1,2})(?:[/.](\d{2,4}))?\b/))) {
        const [a, b] = [Number(d[1]), Number(d[2])];
        const [day, month] = ttDayFirst() ? [a, b] : [b, a];
        const year = d[3] ? (d[3].length === 2 ? 2000 + Number(d[3]) : Number(d[3])) : date.getFullYear();
        if (!setDate(year, month, day)) return null;
        yearless = !d[3];
        haveDate = true;
    } else {
        const monthRe = '(' + TT_MONTHS.map((n) => n.slice(0, 3) + '(?:' + n.slice(3) + ')?').join('|') + ')\\.?';
        const ord = '(\\d{1,2})(?:st|nd|rd|th)?';
        const year = '(?: (\\d{4}))?';
        let found = take(new RegExp('\\b' + monthRe + ' ' + ord + year + '\\b'));
        let month, day, y;
        if (found) [month, day, y] = [found[1], found[2], found[3]];
        else if ((found = take(new RegExp('\\b' + ord + ' (?:of )?' + monthRe + year + '\\b')))) [day, month, y] = [found[1], found[2], found[3]];
        if (found) {
            const mi = TT_MONTHS.findIndex((n) => n.startsWith(month.slice(0, 3)));
            if (!setDate(y ? Number(y) : date.getFullYear(), mi + 1, Number(day))) return null;
            yearless = !y;
            haveDate = true;
        }
    }
    if (!haveDate) {
        let w = take(/\b(today|tonight|tomorrow|tmrw|tmr|yesterday)\b/);
        if (w) {
            const shift = { today: 0, tonight: 0, tomorrow: 1, tmrw: 1, tmr: 1, yesterday: -1 }[w[1]];
            date.setDate(date.getDate() + shift);
            if (w[1] === 'tonight' && !time) time = [20, 0, 0];
            else if (w[1] === 'tonight' && clock24 && time[0] < 12) time[0] += 12;
            haveDate = true;
        } else if ((w = take(/\b(next|last|this) (week|month|year)\b/))) {
            const step = { next: 1, last: -1, this: 0 }[w[1]];
            if (w[2] === 'week') date.setDate(date.getDate() + 7 * step);
            else {
                // The same day of the month, or its last day when that month is shorter.
                const day = date.getDate();
                date.setDate(1);
                if (w[2] === 'month') date.setMonth(date.getMonth() + step);
                else date.setFullYear(date.getFullYear() + step);
                date.setDate(Math.min(day, new Date(date.getFullYear(), date.getMonth() + 1, 0).getDate()));
            }
            haveDate = true;
        } else if ((w = take(/\b(?:(next|this|last|on|coming) )?(?:the )?(weekend|sun(?:day)?|mon(?:day)?|tue(?:s|sday)?|wed(?:nesday)?|thu(?:r|rs|rsday)?|fri(?:day)?|sat(?:urday)?)\b/))) {
            const weekend = w[2] === 'weekend';
            const target = weekend ? 6 : TT_DAYS.findIndex((n) => n.startsWith(w[2].slice(0, 3)));
            const today = date.getDay();
            let ahead;
            if (w[1] === 'last') ahead = -(((today - target + 7) % 7) || 7);
            else if (weekend && today === 0 && w[1] !== 'next') ahead = 0;
            else {
                ahead = (target - today + 7) % 7;
                // "next friday" is the one in next week, not merely the next one to come.
                if (w[1] === 'next') {
                    if (ahead === 0) ahead = 7;
                    if (ahead < (((ttWeekStart() - today + 7) % 7) || 7)) ahead += 7;
                }
            }
            date.setDate(date.getDate() + ahead);
            haveDate = true;
        }
    }
    text = text.replace(/\b(at|on)\b/g, '').trim();
    if (text) return null;
    if (!haveDate && !time) return null;
    if (yearless && date < new Date(now.getFullYear(), now.getMonth(), now.getDate())) {
        const [month, day] = [date.getMonth(), date.getDate()];
        date.setFullYear(date.getFullYear() + 1, month, day);
        if (date.getDate() !== day) return null;
    }
    if (time) return Math.floor(date.setHours(time[0], time[1], time[2], 0) / 1000);
    // A day alone reads as noon: the same calendar date in nearly every zone.
    return { unix: Math.floor(date.setHours(12, 0, 0, 0) / 1000), dateOnly: true };
}

// ---- suggesting a time as it's typed ------------------------------------------------

// Only phrases that can't be ordinary words: "in 6 hours" / "6 hours from now", and a
// calendar date with its day. A bare "tomorrow" or "monday" is left alone, and so is
// anything past: there's nothing to count down to.
// Any word in a unit's place; ttSuggestUnit decides which it is, so one still being typed
// ("in 10 minu") holds the suggestion rather than dropping it between keystrokes.
const TT_SUGGEST_UNIT = '[a-z]+';
const TT_SUGGEST_UNITS = { minutes: 2, hours: 1, days: 1, weeks: 1, months: 3, years: 1 };   // shortest unambiguous start
const TT_SUGGEST_ALIASES = { min: 'minutes', mins: 'minutes', m: 'minutes', hr: 'hours', hrs: 'hours', wk: 'weeks', wks: 'weeks', yr: 'years', yrs: 'years' };

/** The unit a typed word is, or will be once finished; null when it can't be one. After
 *  "a" or "an" a single letter is too little ("in a d…" is as likely "different"). */
function ttSuggestUnit(word, first, spelled) {
    word = word.toLowerCase();
    if (spelled && word.length < 2) return null;
    // A lone "m" leads a phrase as minutes or months alike; after "1h" it's minutes.
    if (word === 'm') return first ? null : 'minutes';
    if (TT_SUGGEST_ALIASES[word]) return TT_SUGGEST_ALIASES[word];
    for (const [unit, least] of Object.entries(TT_SUGGEST_UNITS)) {
        if (word.length >= least && unit.startsWith(word)) return unit;
        if (word === unit.slice(0, -1)) return unit;
    }
    return null;
}
const TT_SUGGEST_AMOUNT = '(?:\\d+(?:\\.\\d+)?|an?|half an?)';
// A second part may be the short "30m" ("in 1h 30m"); a lone "m" could be anything.
const TT_SUGGEST_SPAN = `${TT_SUGGEST_AMOUNT} ?${TT_SUGGEST_UNIT}(?:,?(?: and)? \\d+ ?${TT_SUGGEST_UNIT})?`;
// A phrase may close a sentence; the mark stays outside what's replaced.
const TT_SUGGEST_END = '[.,!?;:)]? ?$';
const TT_SUGGEST_MONTH = '(jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)';
const TT_SUGGEST_DAY = '\\d{1,2}(st|nd|rd|th)?';
const TT_SUGGEST_CLOCK = '(,? (?:at )?(?:\\d{1,2}(?::\\d{2})? ?(?:am|pm)|\\d{1,2}:\\d{2}))?';
const TT_SUGGEST_YEAR = '(,? \\d{4})?';
const TT_SUGGEST_FORMS = [
    { re: new RegExp(`(?:^|[\\s(])(in ${TT_SUGGEST_SPAN}|${TT_SUGGEST_SPAN} from now)${TT_SUGGEST_END}`, 'i'), relative: true },
    { re: new RegExp(`(?:^|[\\s(])(${TT_SUGGEST_MONTH}\\.? ${TT_SUGGEST_DAY}${TT_SUGGEST_YEAR}${TT_SUGGEST_CLOCK})${TT_SUGGEST_END}`, 'i') },
    { re: new RegExp(`(?:^|[\\s(])(?:the )?(${TT_SUGGEST_DAY} (?:of )?${TT_SUGGEST_MONTH}${TT_SUGGEST_YEAR}${TT_SUGGEST_CLOCK})${TT_SUGGEST_END}`, 'i') },
    { re: new RegExp(`(?:^|[\\s(])(\\d{4}-\\d{2}-\\d{2}(?:[ T]\\d{1,2}:\\d{2})?)${TT_SUGGEST_END}`) },
];

/**
 * A time phrase the draft ends with, worth offering as a time everyone reads in their
 * own zone: `{ start, end, unix, style }` (string indices of `before`), or null.
 * "in 6 hours" becomes a countdown, a date a date, a date with a time both.
 */
function ttSuggest(before, now = new Date()) {
    if (!before || before.length < 6) return null;
    const tail = before.slice(-80);
    const offset = before.length - tail.length;
    for (const form of TT_SUGGEST_FORMS) {
        const m = form.re.exec(tail);
        if (!m) continue;
        const phrase = m[1];
        // "may 5" is as likely a verb as a date: only with its year, time or "th".
        if (!form.relative && /^(\d+\w* (of )?)?may\b/i.test(phrase)) {
            const parts = phrase.match(new RegExp(`${TT_SUGGEST_YEAR}${TT_SUGGEST_CLOCK}$`, 'i'));
            if (!/\d(st|nd|rd|th)\b/i.test(phrase) && !(parts && (parts[1] || parts[2]))) return null;
        }
        const start = offset + m.index + m[0].indexOf(phrase);
        const end = start + phrase.length;
        if (tcCodeRanges(before).some(([s, e]) => start >= s && start < e)) return null;
        // Each unit read as the one it is (or is becoming); a word that is none isn't a time.
        let read = phrase;
        if (form.relative) {
            let first = true;
            let bad = false;
            // A spelled amount is a whole word: "and" is not "an" + "d".
            read = phrase.replace(/\b(?:(\d+(?:\.\d+)?) ?|(half an?|an?) )([a-z]+)/gi, (_, number, spelled, word) => {
                const amount = number || spelled;
                const unit = ttSuggestUnit(word, first, !number);
                first = false;
                if (!unit) bad = true;
                return `${amount} ${unit}`;
            });
            if (bad) return null;
        }
        const parsed = ttParseFull(read, now);
        if (!parsed) return null;
        const ahead = parsed.unix - now.getTime() / 1000;
        // Ten minutes and up, with slack for the moments between reading and checking.
        if (ahead < (form.relative ? 590 : 60)) return null;
        const style = form.relative ? 'R' : parsed.dateOnly ? 'D' : 'f';
        return { start, end, unix: parsed.unix, style };
    }
    return null;
}
