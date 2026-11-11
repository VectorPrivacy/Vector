// Deterministic fixture data, shared by the Tauri reference render and vector-gpui.
import { writeFileSync, readFileSync } from 'fs';
import { createHash } from 'crypto';

const NOW = Date.parse('2026-11-11T02:00:00Z');
const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
function polymod(values) {
    const G = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let chk = 1;
    for (const v of values) { const b = chk >> 25; chk = ((chk & 0x1ffffff) << 5) ^ v; for (let i = 0; i < 5; i++) if ((b >> i) & 1) chk ^= G[i]; }
    return chk;
}
function npub(seed) {
    const bytes = createHash('sha256').update(seed).digest();
    const data = []; let acc = 0, bits = 0;
    for (const b of bytes) { acc = (acc << 8) | b; bits += 8; while (bits >= 5) { bits -= 5; data.push((acc >> bits) & 31); } }
    if (bits) data.push((acc << (5 - bits)) & 31);
    const hrp = 'npub';
    const exp = [...[...hrp].map(c => c.charCodeAt(0) >> 5), 0, ...[...hrp].map(c => c.charCodeAt(0) & 31)];
    const mod = polymod([...exp, ...data, 0, 0, 0, 0, 0, 0]) ^ 1;
    const sum = [...Array(6)].map((_, i) => (mod >> (5 * (5 - i))) & 31);
    return hrp + '1' + [...data, ...sum].map(d => CHARSET[d]).join('');
}
const hex = (s) => createHash('sha256').update(s).digest('hex');

const me = npub('me');
const people = [
    ['alice', 'Alice', 'Running late, save me a seat!', 2, 3],
    ['bob', 'Bob Nakamoto', 'The new build is so much faster 🚀', 25, 0],
    ['carol', 'Carol', 'haha yes exactly', 60 * 3, 1],
    ['dave', 'Dave the Builder', 'Sent you the invoice', 60 * 20, 0],
    ['erin', 'Erin', 'Can you check the relay list later?', 60 * 26, 0],
    ['frank', 'Frank', 'gm', 60 * 24 * 3, 0],
    ['grace', 'Grace Hopper', 'It\'s easier to ask forgiveness than permission.', 60 * 24 * 9, 0],
    ['heidi', 'Heidi', 'See you on Saturday', 60 * 24 * 40, 0],
];
const profile = (o) => ({ id: '', name: '', display_name: '', nickname: '', lud06: '', lud16: '', banner: '', avatar: '', about: '', website: '', nip05: '',
    status: { title: '', purpose: '', url: '' }, last_updated: Math.floor(NOW / 1000), mine: false, bot: false, is_blocked: false,
    avatar_cached: '', banner_cached: '', stream_consent: false, ...o });
const profiles = [profile({ id: me, name: 'jskitty', display_name: 'JSKitty', mine: true })];
const chats = [];
for (const [seed, name, last, minsAgo, unread] of people) {
    const id = npub(seed);
    const extra = seed === 'bob'
        ? { about: 'Building on Nostr since 2021.\nCoffee first, relays second ⚡', status: { title: 'Shipping v0.6 🚀', purpose: 'general', url: '' },
            banner: 'https://example.invalid/bob-banner.png', banner_cached: 'media/photo.png' }
        : {};
    profiles.push(profile({ id, name: seed, display_name: name, avatar: `https://example.invalid/${seed}.png`, avatar_cached: `avatars/${seed}.png`, ...extra }));
    const at = NOW - minsAgo * 60_000;
    const convo = [
        [false, `Hey! It's ${name.split(' ')[0]}`, 50],
        [true, 'Hey, how are you doing?', 48],
        [false, 'Pretty good, working on something fun.', 45],
        [true, 'Ooh, tell me more when you can 👀', 44],
        [false, last, 0],
    ];
    const messages = convo.map(([mine, content, back], i) => ({
        id: hex(`${seed}-${i}`), content, replied_to: '', preview_metadata: null, attachments: [], reactions: [],
        at: at - back * 60_000, pending: false, failed: false, mine, npub: mine ? me : id, edited: false,
    }));
    chats.push({
        id, chat_type: 'DirectMessage', participants: [id], messages,
        last_read: unread ? messages[messages.length - 1 - unread].id : messages[messages.length - 1].id,
        unread_from: '', created_at: Math.floor((at - 3600_000) / 1000), metadata: { custom_fields: {} },
        muted: false, notify: 2, everyone_pings: true, community_muted: false,
    });
}

// A conversation exercising the message renderer: streaks, a reply, reactions, edits, send
// states and the markdown Vector supports.
{
    const id = npub('vee');
    profiles.push(profile({ id, name: 'vee', display_name: 'Vee', avatar: 'https://example.invalid/alice.png', avatar_cached: 'avatars/alice.png' }));
    const at = NOW - 60_000;
    const m = (i, mine, content, back, extra = {}) => ({
        id: hex(`vee-${i}`), content, replied_to: '', preview_metadata: null, attachments: [], reactions: [],
        at: at - back * 60_000, pending: false, failed: false, mine, npub: mine ? me : id, edited: false, ...extra,
    });
    const react = (msg, author, emoji) => ({ id: hex(`r-${msg}-${author}-${emoji}`), reference_id: hex(`vee-${msg}`), author_id: author, emoji });
    const messages = [
        m(0, false, 'Have you tried the **new** markdown? It handles *italics*, ~~strikes~~ and `inline code`.', 60 * 26),
        m(1, false, 'Lists too:\n- one\n- two\n- three', 60 * 26 - 1),
        m(2, true, '> quoting you here\nand replying below the quote', 60 * 26 - 3, { reactions: [react(2, id, '❤️'), react(2, me, '❤️'), react(2, id, '😂')] }),
        m(3, true, '```rust\nfn main() {\n    println!("hello");\n}\n```', 60 * 26 - 4),
        m(4, false, 'Nice! That one has a ||secret spoiler|| inside, and a [link](https://vectorapp.io).', 30, { edited: true }),
        m(5, false, '# A heading\n-# and some subtext', 29),
        m(6, true, 'Replying to that heading', 10, { replied_to: hex('vee-5'), replied_to_content: '# A heading\n-# and some subtext', replied_to_npub: id }),
        m(7, true, 'This one is still sending', 2, { pending: true }),
        m(8, true, 'And this one failed', 1, { failed: true }),
        m(9, false, '👍', 0, { reactions: [react(9, me, '🔥')] }),
    ];
    chats.unshift({
        id, chat_type: 'DirectMessage', participants: [id], messages, last_read: messages[messages.length - 1].id,
        unread_from: '', created_at: Math.floor((at - 86400_000 * 2) / 1000), metadata: { custom_fields: {} },
        muted: false, notify: 2, everyone_pings: true, community_muted: false,
    });
}
// Attachments in each state a bubble draws: on this device, a blur waiting on a download
// (too large to auto-download), one auto-downloading, a veiled spoiler, and file boxes.
const thumbs = {};
{
    const tsv = readFileSync(new URL('./media/thumbs.tsv', import.meta.url), 'utf8').trim().split('\n');
    const meta = Object.fromEntries(tsv.map((l) => { const [file, w, h, hash, url] = l.split('\t'); thumbs[hash] = url; return [file, { thumbhash: hash, width: +w, height: +h }]; }));
    const id = npub('mo');
    profiles.push(profile({ id, name: 'mo', display_name: 'Mo', avatar: 'https://example.invalid/grace.png', avatar_cached: 'avatars/grace.png' }));
    const at = NOW - 120_000;
    const att = (seed, o) => ({ id: hex(`att-${seed}`), key: '', nonce: '', extension: 'png', name: '', url: `https://example.invalid/${seed}`,
        path: '', size: 0, img_meta: null, downloading: false, downloaded: false, ...o });
    const m = (i, mine, content, back, attachments) => ({
        id: hex(`mo-${i}`), content, replied_to: '', preview_metadata: null, attachments, reactions: [],
        at: at - back * 60_000, pending: false, failed: false, mine, npub: mine ? me : id, edited: false,
    });
    const messages = [
        m(0, false, 'Look at this sunset', 50, [att('photo', { path: 'media/photo.png', downloaded: true, size: 170552, img_meta: meta['photo.png'] })]),
        m(1, true, '', 40, [att('portrait', { size: 24_117_248, img_meta: meta['portrait.png'] })]),
        m(2, false, '', 30, [att('sticker', { name: 'SPOILER_sticker.png', path: 'media/sticker.png', downloaded: true, size: 15319, img_meta: meta['sticker.png'] })]),
        m(3, false, 'The spec, and the build', 20, [
            att('spec', { extension: 'pdf', name: 'Vector Protocol Spec.pdf', size: 13_002_342 }),
            att('build', { extension: 'zip', name: 'vector-0.5.1-linux.zip', path: 'media/sticker.png', downloaded: true, size: 48_234_496 }),
        ]),
        m(4, true, 'Thanks!', 5, []),
    ];
    chats.unshift({
        id, chat_type: 'DirectMessage', participants: [id], messages, last_read: messages[messages.length - 1].id,
        unread_from: '', created_at: Math.floor((at - 86400_000) / 1000), metadata: { custom_fields: {} },
        muted: false, notify: 2, everyone_pings: true, community_muted: false,
    });
}
// Link preview cards: a page with a picture, one with only text, and a YouTube video.
{
    const id = npub('lin');
    profiles.push(profile({ id, name: 'lin', display_name: 'Lin', avatar: 'https://example.invalid/bob.png', avatar_cached: 'avatars/bob.png' }));
    const at = NOW - 86400_000 * 3;
    const meta = (o) => ({ domain: 'https://example.invalid/', og_title: null, og_description: null, og_image: null, og_url: null, og_type: null,
        title: null, description: null, favicon: 'https://example.invalid/favicon.ico', ...o });
    const m = (i, mine, content, back, preview_metadata = null) => ({
        id: hex(`lin-${i}`), content, replied_to: '', preview_metadata, attachments: [], reactions: [],
        at: at - back * 60_000, pending: false, failed: false, mine, npub: mine ? me : id, edited: false,
    });
    const messages = [
        m(0, false, 'Have a read: https://example.invalid/posts/private-by-default', 30, meta({
            og_title: 'Private by default', og_url: 'https://example.invalid/posts/private-by-default',
            og_description: 'Why every message you send should be sealed before it leaves your device.\nA look at gift wraps, relays and what they can never learn.<br>Part one of three.',
            og_image: 'https://example.invalid/og.png',
        })),
        m(1, true, 'https://example.invalid/docs', 20, meta({ title: 'Docs', description: 'Short and to the point.' })),
        m(2, false, 'Watch this https://www.youtube.com/watch?v=dQw4w9WgXcQ', 10, meta({
            domain: 'https://www.youtube.com/', og_title: 'A video worth watching', og_url: 'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
            og_image: 'https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg', og_description: 'Uploaded by somebody.',
        })),
    ];
    chats.push({
        id, chat_type: 'DirectMessage', participants: [id], messages, last_read: messages[messages.length - 1].id,
        unread_from: '', created_at: Math.floor((at - 86400_000) / 1000), metadata: { custom_fields: {} },
        muted: false, notify: 2, everyone_pings: true, community_muted: false,
    });
}
writeFileSync(new URL('./fixtures.json', import.meta.url), JSON.stringify({ now: NOW, npub: me, profiles, chats, thumbs }, null, 1));
console.log(`${chats.length} chats, ${profiles.length} profiles`);
