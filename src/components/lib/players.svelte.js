// Embedded video players: an account setting synced across devices, offered where this build
// can host one and only on Clearnet (the player loads outside Tor and I2P). One plays at a time.
import { transportState } from './transport.svelte.js';
import { claimPlayback, releasePlayback } from './audio.svelte.js';

// `playing`: the key (lib/framehost.js) of the one player allowed to exist, or a card's token
// while its player is being fetched. Whoever holds a player stops it once this moves on.
const s = $state({ on: true, supported: false, playing: null });

export function playersState() { return s; }
export function setPlayersOn(on) { s.on = on !== false; }
export function setPlayersSupported(supported) { s.supported = !!supported; }
export function playersAvailable() { return s.on && s.supported && transportState().view?.kind === 'clearnet'; }

/** Reserve the slot for a player still being fetched: the one playing now stops. */
export function reservePlayer(token) { s.playing = token; }
/** Player `key` is the one playing. It shares the videos' lane: one stops the other. */
export function startPlayer(key) {
    s.playing = key;
    claimPlayback(`frame:${key}`, () => stopPlayer(key), 'video');
}
export function stopPlayer(key) {
    if (s.playing === key) s.playing = null;
    releasePlayback(`frame:${key}`, 'video');
}

/** `{ id, start }` for a link to one YouTube video, else null. */
export function youtubeVideo(href) {
    let u;
    try { u = new URL(href); } catch { return null; }
    const host = u.hostname.toLowerCase().replace(/^(?:www|m|music)\./, '');
    let id = null;
    if (host === 'youtu.be') id = u.pathname.split('/')[1];
    else if (host === 'youtube.com' || host === 'youtube-nocookie.com') {
        id = u.pathname === '/watch' ? u.searchParams.get('v') : /^\/(?:shorts|embed|live|v)\/([^/]+)/.exec(u.pathname)?.[1];
    }
    if (!id || !/^[A-Za-z0-9_-]{11}$/.test(id)) return null;
    return { id, start: youtubeSeconds(u.searchParams.get('t') ?? u.searchParams.get('start')) };
}

/** `90`, `90s` or `1h2m3s` in seconds; anything else, or past the host page's six digits, is the start. */
function youtubeSeconds(t) {
    const m = t && /^(?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s?)?$/.exec(t);
    const s = m ? (+m[1] || 0) * 3600 + (+m[2] || 0) * 60 + (+m[3] || 0) : 0;
    return s <= 999999 ? s : 0;
}
