// Runs the shared colour-markup cases against src/js/text-color.js; the Rust half
// runs them in vector-core (`shared_cases_match_the_js_parser`).
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const root = new URL('..', import.meta.url);
const ctx = {};
vm.createContext(ctx);
vm.runInContext(readFileSync(new URL('src/js/text-color.js', root), 'utf8') + ';this.tcExtract = tcExtract; this.tcRestore = tcRestore; this.TC_NAMED = TC_NAMED;', ctx);
const cases = JSON.parse(readFileSync(new URL('crates/vector-core/src/text_color_cases.json', root), 'utf8'));
const names = JSON.parse(readFileSync(new URL('crates/vector-core/src/text_color_names.json', root), 'utf8'));

let failed = 0;
// The names a sender types resolve the same on both sides.
if (JSON.stringify(Object.entries(ctx.TC_NAMED).sort()) !== JSON.stringify(Object.entries(names).sort())) {
    failed++;
    console.error('FAIL TC_NAMED differs from text_color_names.json');
}
for (const c of cases) {
    const { plain, spans } = ctx.tcExtract(c.input);
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
console.log(`[text-color] ${cases.length - failed}/${cases.length} cases`);
process.exit(failed ? 1 : 0);
