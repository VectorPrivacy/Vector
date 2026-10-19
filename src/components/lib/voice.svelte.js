// The voice Channel call this device is in, who is in each voice Channel's call, and the voice
// channel the chat pane last opened (its stage shows while that channel is on screen).
// focus: whose shared screen the stage shows; null picks the first sharer, '' shows none.
// joining: the voice channel a join is under way for.
const live = $state({ session: null, stage: null, focus: null, joining: null });
const rooms = $state({});
let handlers = {};

export function voiceSession() { return live.session; }

/** The backend's voice state; null once out of the call. */
export function setVoiceSession(s) {
    live.session = s ? {
        communityId: s.community_id, channelId: s.channel_id, peers: s.peers || [],
        muted: !!s.muted, deafened: !!s.deafened, audio: !!s.audio, micLevel: s.mic_level || 0,
        mic: s.mic !== false, serverMuted: !!s.server_muted, serverDeafened: !!s.server_deafened,
        sharing: !!s.sharing, viewers: s.viewers || 0, camera: !!s.camera,
    } : null;
    if (!s) live.focus = null;
}

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
