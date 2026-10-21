// Every assignable CORD-04 §3 bit, grouped as a person thinks of them. Retired and
// reserved bits are absent; any a role already carries ride through its edits.
export const BIT = {
    MANAGE_ROLES: 1n << 0n, MANAGE_CHANNELS: 1n << 1n, MANAGE_METADATA: 1n << 2n, KICK: 1n << 3n,
    BAN: 1n << 4n, MANAGE_MESSAGES: 1n << 5n, CREATE_INVITE: 1n << 6n, VIEW_AUDIT_LOG: 1n << 8n,
    MENTION_EVERYONE: 1n << 9n, PIN_MESSAGES: 1n << 11n,
    VIEW_CHANNEL: 1n << 13n, SEND_MESSAGES: 1n << 14n, SEND_IN_THREADS: 1n << 15n, ATTACH_FILES: 1n << 16n,
    ADD_REACTIONS: 1n << 17n, READ_MESSAGE_HISTORY: 1n << 18n, CONNECT: 1n << 19n, SPEAK: 1n << 20n,
    VIDEO: 1n << 21n, PRIORITY_SPEAKER: 1n << 22n, MUTE_MEMBERS: 1n << 23n, DEAFEN_MEMBERS: 1n << 24n,
    MOVE_MEMBERS: 1n << 25n, MODERATE_MEMBERS: 1n << 26n, MANAGE_NICKNAMES: 1n << 28n, CHANGE_NICKNAME: 1n << 29n,
};
// Channel and voice bits exist on Roles v2 only; a legacy roster has no use for them.
export const V2_GROUPS = [
    {
        title: 'Channels',
        perms: [
            { bit: BIT.VIEW_CHANNEL, name: 'View Channels', desc: 'See channels and read what is posted in them.' },
            { bit: BIT.SEND_MESSAGES, name: 'Send Messages', desc: 'Post in channels.' },
            { bit: BIT.SEND_IN_THREADS, name: 'Send in Threads', desc: 'Reply in threads.' },
            { bit: BIT.ATTACH_FILES, name: 'Attach Files', desc: 'Share images, videos and files.' },
            { bit: BIT.ADD_REACTIONS, name: 'Add Reactions', desc: 'React to messages.' },
            { bit: BIT.READ_MESSAGE_HISTORY, name: 'Read Message History', desc: 'Read what was posted before they could see a channel.' },
        ],
    },
    {
        title: 'Voice',
        perms: [
            { bit: BIT.CONNECT, name: 'Connect', desc: 'Join calls.' },
            { bit: BIT.SPEAK, name: 'Speak', desc: 'Talk in calls.' },
            { bit: BIT.VIDEO, name: 'Video', desc: 'Share their camera or screen in calls.' },
            { bit: BIT.PRIORITY_SPEAKER, name: 'Priority Speaker', desc: 'Be heard over others while they talk.' },
            { bit: BIT.MUTE_MEMBERS, name: 'Mute Members', desc: 'Mute members beneath them in calls.' },
            { bit: BIT.DEAFEN_MEMBERS, name: 'Deafen Members', desc: 'Deafen members beneath them in calls.' },
            { bit: BIT.MOVE_MEMBERS, name: 'Move Members', desc: 'Move members beneath them between calls.' },
        ],
    },
];
export const GROUPS = [
    {
        title: 'Community',
        perms: [
            { bit: BIT.MANAGE_METADATA, name: 'Manage Community', desc: 'Change the community\'s name, icon and description.' },
            { bit: BIT.MANAGE_CHANNELS, name: 'Manage Channels', desc: 'Create, rename and delete channels.' },
            { bit: BIT.MANAGE_ROLES, name: 'Manage Roles', desc: 'Create and edit roles beneath their own, and give them to members.' },
            { bit: BIT.CREATE_INVITE, name: 'Create Invites', desc: 'Make invite links and invite people directly.' },
            { bit: BIT.VIEW_AUDIT_LOG, name: 'View Audit Log', desc: 'See the record of moderation and management actions.' },
        ],
    },
    {
        title: 'Moderation',
        perms: [
            { bit: BIT.KICK, name: 'Kick Members', desc: 'Remove members beneath them. A kicked member can rejoin.' },
            { bit: BIT.BAN, name: 'Ban Members', desc: 'Ban members beneath them, silencing them and cutting them off for good.' },
            { bit: BIT.MANAGE_MESSAGES, name: 'Manage Messages', desc: 'Hide messages from members beneath them.' },
            { bit: BIT.PIN_MESSAGES, name: 'Pin Messages', desc: 'Pin messages to a channel for everyone to see.' },
            { bit: BIT.MODERATE_MEMBERS, name: 'Timeout Members', desc: 'Silence members beneath them for a while.', v2: true },
            { bit: BIT.MANAGE_NICKNAMES, name: 'Manage Nicknames', desc: 'Change the nicknames of members beneath them.' },
        ],
    },
    {
        title: 'Chat',
        perms: [
            { bit: BIT.MENTION_EVERYONE, name: 'Mention @everyone', desc: 'Notify everyone at once, and mention any role.' },
            { bit: BIT.CHANGE_NICKNAME, name: 'Change Nickname', desc: 'Choose what they go by in this community.', v2: true },
        ],
    },
];

const NAMES = new Map([...GROUPS, ...V2_GROUPS].flatMap((g) => g.perms.map((p) => [p.bit, p.name])));

/** A permission's name by its bit position; a bit this client doesn't know reads by number. */
export function permissionName(index) {
    return NAMES.get(1n << BigInt(index)) || `Permission ${index}`;
}
