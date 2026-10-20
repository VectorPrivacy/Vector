// The Audit Log in words: each entry as a sentence of plain and bold parts, the details it
// expands to, and the family the filter groups it under.
import { permissionName } from './permissions.js';

export const FAMILIES = [
    { value: 'all', label: 'All Actions' },
    { value: 'community', label: 'Community' },
    { value: 'roles', label: 'Roles' },
    { value: 'members', label: 'Members' },
    { value: 'channels', label: 'Channels' },
    { value: 'messages', label: 'Messages' },
    { value: 'voice', label: 'Voice' },
];

export function familyOf(action) {
    if (action.startsWith('voice_')) return 'voice';
    if (action.startsWith('message_') || action === 'pins_update') return 'messages';
    if (action.startsWith('member_') || action === 'banlist_update') return 'members';
    if (action.startsWith('role') || action === 'everyone_update') return 'roles';
    if (action.startsWith('channel_') || action.startsWith('category_') || action.startsWith('overwrite_')) return 'channels';
    return 'community';
}

/** How the row's badge reads the act: something made, something taken away, or a change. */
export function toneOf(action) {
    if (/_(create|pin)$|^invite_create$|^member_unban$|^roles_activate$/.test(action)) return 'add';
    if (/_(delete|unpin)$|^member_(ban|kick|timeout)$|^voice_(disconnect|mute|deafen)$/.test(action)) return 'remove';
    return 'change';
}

const plural = (n, one, many) => (n === 1 ? one : `${n} ${many}`);

function targetName(t, ctx) {
    if (!t) return '';
    switch (t.kind) {
        case 'everyone': return '@everyone';
        case 'role': return t.name ? `@${t.name}` : 'a deleted role';
        case 'member': return ctx.member(t.npub);
        case 'channel': return t.name ? (t.view === 'voice' ? t.name : `#${t.name}`) : 'a private channel';
        case 'category': return t.name || 'a category';
        default: return '';
    }
}

function perms(list) {
    return (list || []).map(permissionName).join(', ');
}

const field = (entry, name) => (entry.changes || []).find((c) => c.field === name);

/**
 * The sentence for `entry`: `[{ t, b }]` parts, `b` marking a name.
 * @param {{ member: (npub: string) => string }} ctx
 */
export function sentence(entry, ctx) {
    const actor = entry.actor ? ctx.member(entry.actor) : 'Someone';
    const t = entry.target;
    const name = (x) => ({ t: targetName(x, ctx), b: true });
    const say = (...parts) => [{ t: actor, b: true }, ...parts.map((p) => (typeof p === 'string' ? { t: p } : p))];
    switch (entry.action) {
        case 'community_create': return say(' created the community');
        case 'community_update': return say(' updated the community');
        case 'roles_activate': return say(' switched the community to its new roles');
        case 'everyone_update': return say(' changed what ', name(t), ' can do');
        case 'role_create': return say(' created the role ', name(t));
        case 'role_update': return say(' updated the role ', name(t));
        case 'role_delete': return say(' deleted the role ', name(t));
        case 'member_roles_update': {
            const c = field(entry, 'roles') || {};
            const added = c.added || [];
            const removed = c.removed || [];
            if (added.length === 1 && !removed.length) return say(' gave ', name(t), ' the role ', name(added[0]));
            if (removed.length === 1 && !added.length) return say(' took ', name(removed[0]), ' from ', name(t));
            return say(' changed the roles of ', name(t));
        }
        case 'member_ban': return say(' banned ', name(t));
        case 'member_unban': return say(' unbanned ', name(t));
        case 'banlist_update': return say(' updated the ban list');
        case 'member_kick': return say(' kicked ', name(t));
        case 'member_timeout': {
            const until = field(entry, 'until')?.new;
            return say(' timed out ', name(t), until ? ` until ${whenFull(until)}` : '');
        }
        case 'member_timeout_remove': return say(' lifted the timeout on ', name(t));
        case 'channel_create': {
            const voice = field(entry, 'view')?.new === 'voice';
            const priv = field(entry, 'private')?.new;
            return say(` created the ${priv ? 'private ' : ''}${voice ? 'voice ' : ''}channel `, name(t));
        }
        case 'channel_update': return say(' updated ', name(t));
        case 'channel_delete': return say(' deleted the channel ', name(t));
        case 'category_create': return say(' created the category ', name(t));
        case 'category_update': return say(' updated the category ', name(t));
        case 'category_delete': return say(' deleted the category ', name(t));
        case 'overwrite_create': return say(' set permissions for ', name(t?.for), ' in ', name(t?.in));
        case 'overwrite_update': return say(' changed permissions for ', name(t?.for), ' in ', name(t?.in));
        case 'overwrite_delete': return say(' cleared the permissions for ', name(t?.for), ' in ', name(t?.in));
        case 'channel_sync': return say(' synced ', name(t), ' to ', name(field(entry, 'category')?.new));
        case 'channel_unsync': return say(' unsynced ', name(t), ' from its category');
        case 'message_pin': return say(' pinned a message by ', { t: ctx.member(t?.author), b: true }, ' in ', name(t?.channel));
        case 'message_unpin': return say(' unpinned a message by ', { t: ctx.member(t?.author), b: true }, ' in ', name(t?.channel));
        case 'pins_update': return say(' changed the pins in ', name(t));
        case 'message_delete': return say(' deleted a message by ', { t: ctx.member(t?.author), b: true }, ' in ', name(t?.channel));
        case 'invite_create': return say(` created ${plural(field(entry, 'count')?.new || 1, 'an invite link', 'invite links')}`);
        case 'invite_delete': return say(` revoked ${plural(field(entry, 'count')?.new || 1, 'an invite link', 'invite links')}`);
        case 'keys_rotate': return field(entry, 'severed')
            ? say(' locked the community down and rotated its keys')
            : say(' rotated the community\'s keys');
        case 'voice_mute': return say(' muted ', name(t), ' in ', name(field(entry, 'channel')?.new));
        case 'voice_unmute': return say(' unmuted ', name(t), ' in ', name(field(entry, 'channel')?.new));
        case 'voice_deafen': return say(' deafened ', name(t), ' in ', name(field(entry, 'channel')?.new));
        case 'voice_undeafen': return say(' undeafened ', name(t), ' in ', name(field(entry, 'channel')?.new));
        case 'voice_move': return say(' moved ', name(t), ' to ', name(field(entry, 'to')?.new));
        case 'voice_disconnect': return say(' disconnected ', name(t), ' from ', name(field(entry, 'channel')?.new));
        default: return say(' made a change');
    }
}

const VIEWS = { chat: 'Text', voice: 'Voice', forum: 'Forum' };
const hexColor = (c) => (c ? `#${Number(c).toString(16).padStart(6, '0')}` : 'none');

/** What `entry` changed, one line each; empty when the sentence says it all. */
export function details(entry, ctx) {
    const out = [];
    const creating = /_create$/.test(entry.action);
    for (const c of entry.changes || []) {
        switch (c.field) {
            case 'name':
                if (!creating || entry.action === 'community_create') out.push(c.old != null ? `Name: ${c.old} → ${c.new}` : `Name: ${c.new}`);
                break;
            case 'description': out.push(c.new ? 'Changed the description' : 'Removed the description'); break;
            case 'topic': out.push(c.new ? `Topic: ${c.new}` : 'Removed the topic'); break;
            case 'icon': out.push(`${c.new === 'removed' ? 'Removed' : c.new === 'added' ? 'Added' : 'Changed'} the icon`); break;
            case 'banner': out.push(`${c.new === 'removed' ? 'Removed' : c.new === 'added' ? 'Added' : 'Changed'} the banner`); break;
            case 'relays':
            case 'voice_relays': {
                const kind = c.field === 'relays' ? 'relay' : 'voice relay';
                for (const r of c.added || []) out.push(`Added the ${kind} ${r}`);
                for (const r of c.removed || []) out.push(`Removed the ${kind} ${r}`);
                break;
            }
            case 'color': out.push(c.old != null ? `Colour: ${hexColor(c.old)} → ${hexColor(c.new)}` : `Colour: ${hexColor(c.new)}`); break;
            case 'permissions':
                if (c.added?.length) out.push(`${creating ? 'Permissions' : 'Granted'}: ${perms(c.added)}`);
                if (c.removed?.length) out.push(`Revoked: ${perms(c.removed)}`);
                break;
            case 'allow':
                if (c.added?.length) out.push(`Allowed: ${perms(c.added)}`);
                if (c.removed?.length) out.push(`No longer allowed: ${perms(c.removed)}`);
                break;
            case 'deny':
                if (c.added?.length) out.push(`Denied: ${perms(c.added)}`);
                if (c.removed?.length) out.push(`No longer denied: ${perms(c.removed)}`);
                break;
            case 'position': out.push('Moved it in the list'); break;
            case 'display': out.push(c.new ? 'Shown on its own in the member list' : 'No longer shown on its own in the member list'); break;
            case 'mentionable': out.push(c.new ? 'Anyone can mention it' : 'No longer mentionable by everyone'); break;
            case 'view': if (!creating) out.push(`Type: ${VIEWS[c.old] || c.old} → ${VIEWS[c.new] || c.new}`); break;
            case 'private': if (!creating) out.push('Made it private'); break;
            case 'category':
                if (entry.action === 'channel_update') out.push(c.new ? `Moved to ${targetName(c.new, ctx)}` : 'Moved out of its category');
                break;
            case 'roles':
                for (const r of c.added || []) out.push(`Added ${targetName(r, ctx)}`);
                for (const r of c.removed || []) out.push(`Removed ${targetName(r, ctx)}`);
                if (c.held) out.push(c.held.length ? `Holds ${c.held.map((r) => targetName(r, ctx)).join(', ')}` : 'Holds no roles');
                break;
            case 'banned':
                if (c.held) out.push(c.held.length ? `Banned: ${c.held.map((m) => targetName(m, ctx)).join(', ')}` : 'Nobody is banned');
                break;
            default: break;
        }
    }
    if (entry.partial) out.push('What came before this was compacted when the community\'s keys were rotated, so what it changed isn\'t known.');
    return out;
}

/** The whole sentence as text, for search. */
export function plainText(entry, ctx) {
    return sentence(entry, ctx).map((p) => p.t).join('');
}

/** When an entry happened, as a person says it. */
export function when(ms, now = Date.now()) {
    const d = new Date(ms);
    const diff = now - ms;
    if (diff < 60_000) return 'Just now';
    if (diff < 3_600_000) return plural(Math.floor(diff / 60_000), 'A minute ago', 'minutes ago');
    const time = d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
    const today = new Date(now);
    const days = Math.round((new Date(today.getFullYear(), today.getMonth(), today.getDate()) - new Date(d.getFullYear(), d.getMonth(), d.getDate())) / 86_400_000);
    if (days === 0) return `Today at ${time}`;
    if (days === 1) return `Yesterday at ${time}`;
    return d.toLocaleDateString([], { day: 'numeric', month: 'short', year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric' });
}

export function whenFull(ms) {
    return new Date(ms).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
}
