// Whether this account sees which invite link brought each member, per community:
// holding Create Invites (the owner always does). Unknown until asked.
const s = $state({ seq: 0 });
const sight = new Map();

/** `true`, `false`, or `undefined` while unknown. Reading it subscribes to changes. */
export function inviteSight(communityId) {
    s.seq;
    return sight.get(communityId);
}

export function setInviteSight(communityId, on) {
    if (sight.get(communityId) === on) return;
    sight.set(communityId, on);
    s.seq++;
}
