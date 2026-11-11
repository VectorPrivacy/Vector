// Deterministic fixture data, shared by the Tauri reference render and vector-gpui.
import { writeFileSync } from 'fs';
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
    profiles.push(profile({ id, name: seed, display_name: name, avatar: `https://example.invalid/${seed}.png`, avatar_cached: `avatars/${seed}.png` }));
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
writeFileSync(new URL('./fixtures.json', import.meta.url), JSON.stringify({ now: NOW, npub: me, profiles, chats }, null, 1));
console.log(`${chats.length} chats, ${profiles.length} profiles`);
