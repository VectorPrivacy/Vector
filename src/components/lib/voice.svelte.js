// The voice Channel call this device is in, and who is in each voice Channel's call.
const live = $state({ session: null });
const rooms = $state({});

export function voiceSession() { return live.session; }

/** The backend's voice state; null once out of the call. */
export function setVoiceSession(s) {
    live.session = s ? {
        communityId: s.community_id, channelId: s.channel_id, peers: s.peers || [],
        muted: !!s.muted, deafened: !!s.deafened, audio: !!s.audio, micLevel: s.mic_level || 0,
    } : null;
}

/** Who is in `channelId`'s call: npubs, earliest first. */
export function voicePresent(channelId) { return rooms[channelId] || []; }
export function setVoicePresent(channelId, npubs) { rooms[channelId] = npubs || []; }

/** Whether `npub` is speaking in the call we are in: louder than room tone. */
export function voiceSpeaking(npub) {
    const p = live.session?.peers.find((x) => x.npub === npub);
    return !!p && p.level > 0.15;
}
