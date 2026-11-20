// The voice Channel call this device is in, who is in each voice Channel's call, and the voice
// channel the chat pane last opened (its stage shows while that channel is on screen).
// focus: whose shared screen the stage shows; null picks the first sharer, '' shows none.
// joining: the voice channel a join is under way for.
// since: when we joined the call, for its timer.
import { now as clockNow } from './clock.svelte.js';
const live = $state({ session: null, stage: null, focus: null, joining: null, since: 0 });
const rooms = $state({});
let handlers = {};

export function voiceSession() { return live.session; }

/** The backend's voice state; null once out of the call. */
export function setVoiceSession(s) {
    if (!s) live.since = 0;
    else if (!live.session || live.session.channelId !== s.channel_id) live.since = clockNow();
    live.session = s ? {
        communityId: s.community_id, channelId: s.channel_id, peers: s.peers || [],
        muted: !!s.muted, deafened: !!s.deafened, audio: !!s.audio, micLevel: s.mic_level || 0,
        mic: s.mic !== false, serverMuted: !!s.server_muted, serverDeafened: !!s.server_deafened,
        sharing: !!s.sharing, viewers: s.viewers || 0, camera: !!s.camera, canVideo: !!s.can_video,
    } : null;
    if (!s) live.focus = null;
}

/** When we joined the call we are in, in ms; 0 when in none. */
export function voiceSince() { return live.since; }

export function voiceJoining() { return live.joining; }
export function setVoiceJoining(channelId) { live.joining = channelId || null; }

export function voiceFocus() { return live.focus; }
export function setVoiceFocus(npub) { live.focus = npub; }

/** Whether our microphone is off, for any reason: our mute, a moderator's, or a deafen. */
export function voiceMicOff(s) {
    return !!s && (s.muted || s.serverMuted || s.deafened || s.serverDeafened);
}

/** The mic button: deafened, it undeafens and unmutes; under a moderator, it asks (and is told why not). */
export function toggleVoiceMic(s, h) {
    if (s.deafened) {
        h.setDeafened(false);
        if (s.muted) h.setMuted(false);
    } else if (s.serverMuted || s.serverDeafened) {
        h.setMuted(false);
    } else {
        h.setMuted(!s.muted);
    }
}

/** Who is in `channelId`'s call, earliest first: [{ npub, muted, deafened, server_muted, server_deafened, elsewhere }]. */
export function voicePresent(channelId) { return rooms[channelId] || []; }
export function setVoicePresent(channelId, list) { rooms[channelId] = list || []; }

/** The voice channel the chat pane opened: { communityId, channelId, name }. */
export function voiceStage() { return live.stage; }
export function setVoiceStage(stage) { live.stage = stage || null; }

/** The stage's and the bar's helpers: join, leave, setMuted, setDeafened, micAccess, getProfile,
 *  getName, avatarSrc, myNpub, memberMenu, keyHint, canShare, canCamera, share, camera, watch, videoCanvas, selfPreview. */
export function voiceCallHandlers() { return handlers; }
export function setVoiceCallHandlers(h) { handlers = h || {}; }

/** How loud `npub` is in the call we are in, 0 to 1; our own microphone when it's us. */
export function voiceLevel(npub, me) {
    const s = live.session;
    if (!s) return 0;
    if (npub === me) return voiceMicOff(s) || !s.mic ? 0 : s.micLevel;
    return s.peers.find((x) => x.npub === npub)?.level || 0;
}

/** Louder than room tone. */
export const SPEAKING = 0.15;

const GRADES = ['excellent', 'good', 'fair', 'poor'];
const worse = (a, b) => (GRADES.indexOf(a) >= GRADES.indexOf(b) ? a : b);
// A flag outlives its link's recovery by this much, so a link at the edge of a grade doesn't blink.
const FLAG_HOLD_MS = 4000;
const flagsHeld = new Map();

/** Trouble on a peer's link to us, from what actually hurts: loss, or a round trip grown well past
 *  the link's own usual (on a relay, trouble shows as delay, not loss). A steady long path is fine. */
function linkTrouble(p) {
    const loss = p.loss_pct || 0;
    const byLoss = loss < 0.5 ? 'excellent' : loss < 2 ? 'good' : loss < 5 ? 'fair' : 'poor';
    const floor = p.rtt_floor_ms || 0;
    const queued = floor && p.rtt_ms ? p.rtt_ms - floor : 0;
    const byQueue = queued >= 1000 ? 'poor' : queued >= Math.max(300, floor) ? 'fair' : 'excellent';
    return { grade: worse(byLoss, byQueue), byLoss: byLoss !== 'excellent' && worse(byLoss, byQueue) === byLoss };
}

/** A peer's link to us while it's in trouble: { grade, label, title }, the label being the
 *  figure that earned the grade. Null while it's fine or not linked yet. */
export function voiceLinkFlag(p) {
    if (!p?.connected) return null;
    const { grade, byLoss } = linkTrouble(p);
    if (grade !== 'excellent') {
        const flag = { grade, label: byLoss ? `${Math.max(1, Math.round(p.loss_pct))}% loss` : `${p.rtt_ms} ms`, title: voiceLinkTitle(p) };
        flagsHeld.set(p.npub, { flag, at: Date.now() });
        return flag;
    }
    const held = flagsHeld.get(p.npub);
    if (held && Date.now() - held.at < FLAG_HOLD_MS) return held.flag;
    flagsHeld.delete(p.npub);
    return null;
}

/** A linked peer's path in words: round trip against its usual, loss and route. */
export function voiceLinkTitle(p) {
    if (!p?.connected || !p.rtt_ms) return null;
    const usual = p.rtt_floor_ms && p.rtt_ms - p.rtt_floor_ms >= 50 ? ` (usually ${p.rtt_floor_ms} ms)` : '';
    return `${p.rtt_ms} ms${usual}, ${(p.loss_pct || 0).toFixed(1)}% loss, ${p.direct ? 'direct' : 'through a relay'}`;
}

/** Most of our links in trouble at once: the trouble is ours, not theirs. Its grade, or null. */
export function voiceOwnTrouble(s) {
    const linked = (s?.peers || []).filter((p) => p.connected && p.rtt_ms);
    if (linked.length < 2) return null;
    const grades = linked.map((p) => linkTrouble(p).grade);
    const poor = grades.filter((g) => g === 'poor').length;
    const bad = poor + grades.filter((g) => g === 'fair').length;
    if (bad * 2 <= linked.length) return null;
    return poor * 2 > linked.length ? 'poor' : 'fair';
}
