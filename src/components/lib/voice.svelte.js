// The voice Channel call this device is in, and who is in each voice Channel's call.
const live = $state({ session: null });
const rooms = $state({});

export function voiceSession() { return live.session; }

/** The backend's voice state; null once out of the call. */
export function setVoiceSession(s) {
    live.session = s ? {
        communityId: s.community_id, channelId: s.channel_id, peers: s.peers || [],
        muted: !!s.muted, deafened: !!s.deafened, audio: !!s.audio,
    } : null;
}

/** Who is in `channelId`'s call: npubs, earliest first. */
export function voicePresent(channelId) { return rooms[channelId] || []; }
export function setVoicePresent(channelId, npubs) { rooms[channelId] = npubs || []; }
