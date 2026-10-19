// The voice Channel call this device is in, who is in each voice Channel's call, and the voice
// channel the chat pane last opened (its stage shows while that channel is on screen).
const live = $state({ session: null, stage: null });
const rooms = $state({});
let handlers = {};

export function voiceSession() { return live.session; }

/** The backend's voice state; null once out of the call. */
export function setVoiceSession(s) {
    live.session = s ? {
        communityId: s.community_id, channelId: s.channel_id, peers: s.peers || [],
        muted: !!s.muted, deafened: !!s.deafened, audio: !!s.audio, micLevel: s.mic_level || 0,
        mic: s.mic !== false,
    } : null;
}

/** Who is in `channelId`'s call, earliest first: [{ npub, muted, deafened, elsewhere }]. */
export function voicePresent(channelId) { return rooms[channelId] || []; }
export function setVoicePresent(channelId, list) { rooms[channelId] = list || []; }

/** The voice channel the chat pane opened: { communityId, channelId, name }. */
export function voiceStage() { return live.stage; }
export function setVoiceStage(stage) { live.stage = stage || null; }

/** The stage's and the bar's helpers: join, leave, setMuted, setDeafened, micAccess, getProfile,
 *  getName, avatarSrc, myNpub, memberMenu. */
export function voiceCallHandlers() { return handlers; }
export function setVoiceCallHandlers(h) { handlers = h || {}; }

/** How loud `npub` is in the call we are in, 0 to 1; our own microphone when it's us. */
export function voiceLevel(npub, me) {
    const s = live.session;
    if (!s) return 0;
    if (npub === me) return s.muted || !s.mic ? 0 : s.micLevel;
    return s.peers.find((x) => x.npub === npub)?.level || 0;
}

/** Louder than room tone. */
export const SPEAKING = 0.15;
