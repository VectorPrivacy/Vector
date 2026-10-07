// Streamer Mode: a synced account setting that shows people who have not allowed
// streams as dots and a tinted circle. `seed` reshuffles the tints every time it goes
// live; `seq` moves whenever `on` or `seed` does.
const MIRROR = 'vector-streamer';

/** A hidden person's name: fixed length, so it says nothing about the real one. */
export const streamDots = '•••••';

// The device mirror is read before any account loads, so a boot mid-stream starts hidden.
function mirrored() {
    try { return localStorage.getItem(MIRROR) === 'true'; } catch { return false; }
}

const s = $state({ on: mirrored(), seed: '', notif: 'none', hideWallpapers: true, seq: 0 });

export function streamerState() { return s; }
export function streamerSeq() { return s.seq; }

/** Adopt the backend's view: { on, seed, notif, hide_wallpapers }. */
export function setStreamer(view) {
    const on = !!view?.on;
    const seed = typeof view?.seed === 'string' ? view.seed : '';
    if (on !== s.on || seed !== s.seed) s.seq++;
    s.on = on;
    s.seed = seed;
    s.notif = view?.notif || 'none';
    s.hideWallpapers = view?.hide_wallpapers !== false;
    try { localStorage.setItem(MIRROR, on ? 'true' : 'false'); } catch { /* the in-memory value still applies */ }
}

// Keyed (SipHash-2-4) under the stream's 128-bit seed, so the tints and codes on screen
// can't be traced back to the npubs behind them.
const M64 = (1n << 64n) - 1n;
const rotl = (x, b) => ((x << b) | (x >> (64n - b))) & M64;
function sipRound(v) {
    v[0] = (v[0] + v[1]) & M64; v[1] = rotl(v[1], 13n) ^ v[0]; v[0] = rotl(v[0], 32n);
    v[2] = (v[2] + v[3]) & M64; v[3] = rotl(v[3], 16n) ^ v[2];
    v[0] = (v[0] + v[3]) & M64; v[3] = rotl(v[3], 21n) ^ v[0];
    v[2] = (v[2] + v[1]) & M64; v[1] = rotl(v[1], 17n) ^ v[2]; v[2] = rotl(v[2], 32n);
}
function le64(bytes, at) {
    let x = 0n;
    for (let i = 7; i >= 0; i--) x = (x << 8n) | BigInt(bytes[at + i]);
    return x;
}
function siphash24(key, msg) {
    const k0 = le64(key, 0), k1 = le64(key, 8);
    const v = [k0 ^ 0x736f6d6570736575n, k1 ^ 0x646f72616e646f6dn, k0 ^ 0x6c7967656e657261n, k1 ^ 0x7465646279746573n];
    const n = msg.length, end = n - (n % 8);
    for (let i = 0; i < end; i += 8) {
        const m = le64(msg, i);
        v[3] ^= m; sipRound(v); sipRound(v); v[0] ^= m;
    }
    let b = BigInt(n & 0xff) << 56n;
    for (let i = 0; i < n % 8; i++) b |= BigInt(msg[end + i]) << BigInt(8 * i);
    v[3] ^= b; sipRound(v); sipRound(v); v[0] ^= b;
    v[2] ^= 0xffn;
    sipRound(v); sipRound(v); sipRound(v); sipRound(v);
    return v[0] ^ v[1] ^ v[2] ^ v[3];
}

// Until a seed is read (the pre-login picker, a failed read), a key that never leaves memory:
// a fixed one would give every stream the same tints.
const memoryKey = crypto.getRandomValues(new Uint8Array(16));

let keyedSeed = null;
let key = memoryKey;
const tags = new Map();

/** The npub's 64-bit tag under this stream's key: tints take the low half, codes the high. */
function streamTag(npub) {
    if (keyedSeed !== s.seed) {
        keyedSeed = s.seed;
        key = /^[0-9a-f]{32}$/.test(s.seed) ? Uint8Array.from(s.seed.match(/../g), (b) => parseInt(b, 16)) : memoryKey;
        tags.clear();
        tints.clear();
    }
    let tag = tags.get(npub);
    if (tag === undefined) {
        tag = siphash24(key, new TextEncoder().encode(npub));
        tags.set(npub, tag);
    }
    return tag;
}

const tints = new Map();

/** A hidden person's avatar: a solid circle whose hue is all that tells them apart. */
export function streamTint(npub) {
    const hue = Number(streamTag(npub) & 0xffffffffn) % 360;
    let uri = tints.get(npub);
    if (!uri) {
        uri = 'data:image/svg+xml,' + encodeURIComponent(
            `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><circle cx="1" cy="1" r="1" fill="hsl(${hue},48%,46%)"/></svg>`);
        tints.set(npub, uri);
    }
    return uri;
}

/** A short per-stream label for a hidden person: `len` hex digits (at most 8) of the tag's high half. */
export function streamCode(npub, len = 3) {
    return Number(streamTag(npub) >> 32n).toString(16).padStart(8, '0').slice(0, Math.min(len, 8));
}
