// Runs cleanTrackingFromUrl (src/js/misc.js) over links whose trackers must go and whose
// working parts, referrals included, must stay.
import { readFileSync } from 'node:fs';

const src = readFileSync(new URL('../src/js/misc.js', import.meta.url), 'utf8');
const from = src.indexOf('const REFERRAL_PARAM_RE');
const to = src.indexOf('function cleanTrackingFromUrl');
const end = src.indexOf('\n}\n', to) + 3;
const clean = new Function(`${src.slice(from, end)}\nreturn cleanTrackingFromUrl;`)();

const cases = [
    // X's copied links: the share ID `a` joined `s` and `t`.
    ['https://x.com/nexta_tv/status/2115837312714109277?a=vPNzxc_eNYr_tETQaWo&s=20', 'https://x.com/nexta_tv/status/2115837312714109277'],
    ['https://twitter.com/a/status/1?cxt=abc&t=xyz', 'https://twitter.com/a/status/1'],
    // `a`, `s` and `t` stay everywhere else.
    ['https://example.com/?a=1&s=search&t=30', 'https://example.com/?a=1&s=search&t=30'],
    ['https://www.youtube.com/watch?v=abc&t=42&si=zzz', 'https://www.youtube.com/watch?v=abc&t=42'],
    // Click IDs minted this year, on any host.
    ['https://shop.example/p?tgclid=1&_kx=2&pjclid=3&hootPostID=4&id=7', 'https://shop.example/p?id=7'],
    ['https://news.example/a?_bhlid=x&ym_tracking_id=y&ircid=z&page=2', 'https://news.example/a?page=2'],
    ['https://example.com/?mt_click_id=1&ebisAdID=2&nb_cid=3&cm_mmca4=5&at_type=6&q=x', 'https://example.com/?q=x'],
    ['https://example.com/?utm_ag=1&tw_source=2&tw_adid=3&keep=1', 'https://example.com/?keep=1'],
    // Referral credit survives the patterns and the global list.
    ['https://www.amazon.com/dp/B000000000?tag=friend-20&linkId=abc&ufe=app', 'https://www.amazon.com/dp/B000000000?tag=friend-20'],
    ['https://example.com/?ref=friend&aff_id=9&utm_source=x', 'https://example.com/?ref=friend&aff_id=9'],
    ['https://writer.substack.com/p/post?r=abc&triedRedirect=true&publication_id=1', 'https://writer.substack.com/p/post?r=abc'],
    // Site branches.
    ['https://www.instagram.com/p/xyz/?igsh=a&stkn=b&click_source=c', 'https://www.instagram.com/p/xyz/'],
    ['https://www.threads.com/@user/post/abc?xmt=AQG', 'https://www.threads.com/@user/post/abc'],
    ['https://www.reddit.com/r/x/comments/1/t/?ref=share&ref_source=link', 'https://www.reddit.com/r/x/comments/1/t/'],
    ['https://www.nytimes.com/2026/10/30/a.html?unlocked_article_code=g1&smid=nytcore-ios-share&sgrp=c', 'https://www.nytimes.com/2026/10/30/a.html?unlocked_article_code=g1'],
    ['https://www.theguardian.com/world/2026/oct/30/a?CMP=share_btn_url', 'https://www.theguardian.com/world/2026/oct/30/a'],
    ['https://www.temu.com/goods.html?goods_id=6&_x_from=a&_oak_region=b&refer_share_id=c&_p_rfs=1', 'https://www.temu.com/goods.html?goods_id=6'],
    ['https://www.linkedin.com/jobs/view/1/?trackingId=a&refId=b&eBP=c&alternateChannel=d', 'https://www.linkedin.com/jobs/view/1/'],
    ['https://www.google.com/search?q=vector&sstk=a&spell=1&ved=b', 'https://www.google.com/search?q=vector'],
    // A link with nothing to strip comes back byte for byte.
    ['https://www.google.com/maps/place/x/data=!4m2!3m1!1s0x0:0x1?entry=ttu', 'https://www.google.com/maps/place/x/data=!4m2!3m1!1s0x0:0x1?entry=ttu'],
];

let failed = 0;
for (const [input, want] of cases) {
    const got = clean(input);
    if (got !== want) {
        failed++;
        console.error(`FAIL ${input}\n  got  ${got}\n  want ${want}`);
    }
}
console.log(`${cases.length - failed}/${cases.length} tracking cases pass`);
process.exit(failed ? 1 : 0);
