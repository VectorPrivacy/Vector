// Renders the Tauri frontend with the fixtures and saves a screenshot plus element boxes.
//   node capture.mjs <out-prefix> [width] [height] [script-after-boot.js]
import { createServer } from 'http';
import { readFileSync, existsSync, statSync, writeFileSync } from 'fs';
import { join, extname, dirname } from 'path';
import { fileURLToPath } from 'url';
import { createRequire } from 'module';
// require() honours NODE_PATH, so a global Playwright works too.
const { chromium } = createRequire(import.meta.url)('playwright');

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = join(HERE, '..', '..', '..', 'src');
const [out = 'ref', W = '385', H = '650', after] = process.argv.slice(2);
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.json': 'application/json', '.gif': 'image/gif', '.webp': 'image/webp' };
const server = createServer((req, res) => {
    let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    let file = p.startsWith('/__ref/') ? join(HERE, p.slice(7)) : join(SRC, p === '/' ? 'index.html' : p);
    if (!existsSync(file) || statSync(file).isDirectory()) { res.writeHead(404); return res.end(); }
    res.writeHead(200, { 'content-type': TYPES[extname(file)] || 'application/octet-stream' });
    res.end(readFileSync(file));
}).listen(0);
const port = server.address().port;
const fixtures = JSON.parse(readFileSync(process.env.FIXTURES || join(HERE, 'fixtures.json'), 'utf8'));
// REQUESTS=<n>: the last n chats lose our side, so they wait under Requests.
const requests = +process.env.REQUESTS || 0;
// CHATLIST_DEMO=1: pins and archived chats from the fixtures' demo block.
if (process.env.CHATLIST_DEMO) Object.assign(fixtures, fixtures.chatlist_demo);
// VOICE_DEMO=present|joined: who is in the Lounge's call, and (joined) our own session in it.
if (process.env.VOICE_DEMO) fixtures.voice = {
    present: fixtures.voice_demo[process.env.VOICE_DEMO === 'joined' ? 'joined' : 'present'],
    session: process.env.VOICE_DEMO === 'joined' ? fixtures.voice_demo.session : null,
};
// PINS_DEMO=1: the general channel's pins.
if (process.env.PINS_DEMO) fixtures.pins = fixtures.pins_demo;
if (requests) for (const chat of fixtures.chats.slice(-requests)) chat.messages = chat.messages.filter((m) => !m.mine);

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: +W, height: +H }, deviceScaleFactor: 1 });
await page.clock.install({ time: fixtures.now });
await page.clock.resume();
const logs = [];
page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') logs.push(`[${m.type()}] ${m.text()}`); });
page.on('pageerror', (e) => logs.push(`[pageerror] ${e.message}`));
await page.addInitScript(`window.__VECTOR_FIXTURES__ = ${JSON.stringify(fixtures)};`);
await page.addInitScript(`window.__OPEN_NAME__ = ${JSON.stringify(process.env.OPEN || '')};`);
await page.addInitScript(`window.__CREATOR__ = ${JSON.stringify(process.env.CREATOR || "")};`);
await page.addInitScript(`window.__TYPE__ = ${JSON.stringify(process.env.TYPE || "")};`);
await page.addInitScript(`window.__NET__ = ${JSON.stringify(process.env.NET_DEMO || "")};`);
await page.addInitScript(`window.__REF_TRANSCRIPT__ = ${JSON.stringify(!!process.env.TRANSCRIPT)};`);
await page.addInitScript(`window.__MOCK_MODE__ = ${JSON.stringify(process.env.MOCK_MODE || 'app')};`);
await page.addInitScript({ path: join(HERE, 'mock-tauri.js') });
await page.goto(`http://localhost:${port}/`);
await page.waitForTimeout(2500);
if (after) { await page.evaluate(readFileSync(after, 'utf8')); await page.waitForTimeout(+(process.env.WAIT || 1500)); }
// MOUSE=x,y[;x,y…]: pointer moves after the script, each given a moment.
for (const p of (process.env.MOUSE || '').split(';').filter(Boolean)) { const [x, y] = p.split(',').map(Number); await page.mouse.move(x, y, { steps: 4 }); await page.waitForTimeout(400); }
await page.screenshot({ path: `${out}.png` });
const boxes = await page.evaluate(() => {
    const pick = [...document.querySelectorAll('body *')].filter((e) => { const r = e.getBoundingClientRect(); const s = getComputedStyle(e); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none' && r.bottom > 0 && r.top < innerHeight; });
    return pick.slice(0, 1500).map((e) => { const r = e.getBoundingClientRect(); const s = getComputedStyle(e); return { tag: e.tagName.toLowerCase(), id: e.id || undefined, cls: (typeof e.className === 'string' ? e.className : '') || undefined, x: +r.x.toFixed(1), y: +r.y.toFixed(1), w: +r.width.toFixed(1), h: +r.height.toFixed(1), font: `${s.fontWeight} ${s.fontSize}/${s.lineHeight} ${s.fontFamily.split(',')[0]}`, color: s.color, bg: s.backgroundColor !== 'rgba(0, 0, 0, 0)' ? s.backgroundColor : undefined, radius: s.borderRadius !== '0px' ? s.borderRadius : undefined, border: ['Top','Right','Bottom','Left'].map((d) => s['border'+d+'Width'] !== '0px' ? `${d[0]}:${s['border'+d+'Width']} ${s['border'+d+'Color']}` : '').filter(Boolean).join(' ') || undefined, opacity: s.opacity !== '1' ? s.opacity : undefined, shadow: s.boxShadow !== 'none' ? s.boxShadow : undefined, text: e.children.length === 0 ? (e.textContent || '').trim().slice(0, 40) || undefined : undefined }; });
});
writeFileSync(`${out}.boxes.json`, JSON.stringify(boxes, null, 0).replace(/\},\{/g, '},\n{'));
writeFileSync(`${out}.log`, [...logs, 'missed: ' + (await page.evaluate(() => window.__mockMissed.join(' ')))].join('\n'));
await browser.close(); server.close();
console.log(`${out}.png  (${boxes.length} boxes, ${logs.length} console problems)`);
