// Runs the shared colour-markup cases against src/js/text-color.js; the Rust half
// runs them in vector-core (`shared_cases_match_the_js_parser`).
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const root = new URL('..', import.meta.url);
const ctx = {};
vm.createContext(ctx);
vm.runInContext(readFileSync(new URL('src/js/text-color.js', root), 'utf8') + readFileSync(new URL('src/js/text-time.js', root), 'utf8')
    + ';this.tcExtract = tcExtract; this.tcExtractColor = tcExtractColor; this.tcRestore = tcRestore; this.TC_NAMED = TC_NAMED; this.ttParse = ttParse;', ctx);
const cases = JSON.parse(readFileSync(new URL('crates/vector-core/src/text_color_cases.json', root), 'utf8'));
const names = JSON.parse(readFileSync(new URL('crates/vector-core/src/text_color_names.json', root), 'utf8'));

let failed = 0;
// The names a sender types resolve the same on both sides.
if (JSON.stringify(Object.entries(ctx.TC_NAMED).sort()) !== JSON.stringify(Object.entries(names).sort())) {
    failed++;
    console.error('FAIL TC_NAMED differs from text_color_names.json');
}
for (const c of cases) {
    const { plain, spans } = ctx.tcExtractColor(c.input);
    const got = JSON.stringify([plain, spans.map((s) => [s.from, s.to, s.effect, ...s.colors])]);
    const want = JSON.stringify([c.plain, c.spans]);
    if (got !== want) {
        failed++;
        console.error(`FAIL ${JSON.stringify(c.input)}\n  got  ${got}\n  want ${want}`);
        continue;
    }
    // An edit puts the markup back; reading it again must give the same message.
    const again = ctx.tcExtract(ctx.tcRestore(plain, spans));
    if (JSON.stringify([again.plain, again.spans]) !== JSON.stringify([plain, spans])) {
        failed++;
        console.error(`FAIL restore ${JSON.stringify(c.input)}`);
    }
}
// Colour and times together, against the cases core runs (`shared_cases_match_the_js_extractor`).
const canon = (v) => JSON.stringify(v, (k, x) => (x && typeof x === 'object' && !Array.isArray(x)
    ? Object.fromEntries(Object.entries(x).filter(([key, val]) => !(key === 'colors' && Array.isArray(val) && !val.length)).sort())
    : x));
const spanCases = JSON.parse(readFileSync(new URL('crates/vector-core/src/text_spans_cases.json', root), 'utf8'));
for (const c of spanCases) {
    const { plain, spans } = ctx.tcExtract(c.input);
    if (plain !== c.plain || canon(spans) !== canon(c.spans)) {
        failed++;
        console.error(`FAIL spans ${JSON.stringify(c.input)}\n  got  ${JSON.stringify([plain, spans])}\n  want ${JSON.stringify([c.plain, c.spans])}`);
        continue;
    }
    const again = ctx.tcExtract(ctx.tcRestore(plain, spans));
    if (again.plain !== plain || canon(again.spans) !== canon(spans)) {
        failed++;
        console.error(`FAIL spans restore ${JSON.stringify(c.input)} → ${JSON.stringify(ctx.tcRestore(plain, spans))}`);
    }
}

// Typed times, read against a fixed "now": Wednesday 2026-10-07 15:42:10 local.
const now = new Date(2026, 9, 7, 15, 42, 10);
const at = (y, mo, d, h = 15, mi = 42, s = 0) => Math.floor(new Date(y, mo - 1, d, h, mi, s).getTime() / 1000);
const parse = [
    ['', at(2026, 10, 7)], ['now', at(2026, 10, 7)],
    ['5pm', at(2026, 10, 7, 17, 0)], ['5:30 pm', at(2026, 10, 7, 17, 30)], ['17:30', at(2026, 10, 7, 17, 30)], ['at 9', at(2026, 10, 7, 9, 0)],
    ['noon', at(2026, 10, 7, 12, 0)], ['midnight', at(2026, 10, 7, 0, 0)],
    ['tomorrow', at(2026, 10, 8)], ['tomorrow 5pm', at(2026, 10, 8, 17, 0)], ['5pm tomorrow', at(2026, 10, 8, 17, 0)], ['tonight', at(2026, 10, 7, 20, 0)],
    ['friday', at(2026, 10, 9)], ['friday 9:30am', at(2026, 10, 9, 9, 30)], ['next wednesday', at(2026, 10, 14)], ['wednesday', at(2026, 10, 7)], ['on mon at 8pm', at(2026, 10, 12, 20, 0)],
    ['2026-12-25', at(2026, 12, 25)], ['2026-12-25 18:00', at(2026, 12, 25, 18, 0)], ['dec 25', at(2026, 12, 25)], ['25th of december 2027 at 9am', at(2027, 12, 25, 9, 0)], ['december 25th', at(2026, 12, 25)],
    ['in 2 hours', Math.round(now.getTime() / 1000) + 7200], ['in 1h30m', Math.round(now.getTime() / 1000) + 5400], ['in an hour', Math.round(now.getTime() / 1000) + 3600],
    ['3 days ago', Math.round(now.getTime() / 1000) - 259200], ['in half an hour', Math.round(now.getTime() / 1000) + 1800],
    ['1700000000', 1700000000], ['1700000000000', 1700000000],
    ['banana', null], ['tomorrow banana', null], ['13pm', null], ['in 2 bananas', null], ['25:00', null],
    ['2026-02-30', null], ['feb 30', null], ['13/13', null], ['999999999999', null], ['in 9999 years', null], ['1000000 days ago', null],
    ['tonight at 9', at(2026, 10, 7, 21, 0)], ['tonight at 9:30', at(2026, 10, 7, 21, 30)], ['tonight at 11pm', at(2026, 10, 7, 23, 0)],
    ['5 p.m.', at(2026, 10, 7, 17, 0)], ['9:30 a.m. tomorrow', at(2026, 10, 8, 9, 30)],
];
for (const [text, want] of parse) {
    const got = ctx.ttParse(text, now);
    if (got !== want) { failed++; console.error(`FAIL parse ${JSON.stringify(text)}: got ${got}, want ${want}`); }
}

console.log(`[text-color] ${cases.length} colour cases, ${spanCases.length} span cases, ${parse.length} typed times${failed ? ` — ${failed} failed` : ''}`);
process.exit(failed ? 1 : 0);
