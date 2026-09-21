/**
 * The notification and mute sections shared by every menu that can reach a
 * community, a channel or a DM.
 *
 * Levels resolve Community → Channel, most specific opinion winning, so a
 * channel's list leads with what it would inherit and prints the resolved
 * value inline. Mute is the other axis: it greys the row and turns it down to
 * just your name, which is why "Nothing" is a level rather than a synonym for
 * it.
 */

/** Matches Discord's set. One list, used by the community, channel and DM menus alike. */
const MUTE_DURATIONS = [
    { label: 'For 15 minutes', ms: 15 * 60 * 1000 },
    { label: 'For 1 hour', ms: 60 * 60 * 1000 },
    { label: 'For 3 hours', ms: 3 * 60 * 60 * 1000 },
    { label: 'For 8 hours', ms: 8 * 60 * 60 * 1000 },
    { label: 'For 24 hours', ms: 24 * 60 * 60 * 1000 },
    { label: 'Until I turn it back on', ms: -1 },
];

const NOTIFY_LEVELS = [
    { value: 'all', label: 'All messages' },
    { value: 'mentions', label: 'Mentions' },
    { value: 'nothing', label: 'Nothing' },
];

function notifyLevelLabel(value) {
    return (NOTIFY_LEVELS.find(l => l.value === value) || NOTIFY_LEVELS[0]).label;
}

/**
 * Read the scopes a menu is about to render. `communityId` is null for a DM.
 * Returns `{ self, community }` views, or nulls when the backend refused.
 */
async function loadNotifyViews(scopeId, communityId) {
    const views = await invoke('get_notify_prefs', {
        communityId: communityId || null,
        scopeIds: [scopeId],
    }).catch(() => []);
    return {
        self: views.find(v => v.scope_id === scopeId) || null,
        community: communityId ? views.find(v => v.scope_id === communityId) || null : null,
    };
}

/** "until 19:30" for a timed mute, "" for anything else. No countdown, no ticking. */
function muteHint(view) {
    if (!view || view.mute_until <= 0) return '';
    const at = new Date(view.mute_until);
    const time = at.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    // A 24 hour mute lands at the same clock time tomorrow, which reads as "until now".
    const today = new Date();
    const sameDay = at.getDate() === today.getDate()
        && at.getMonth() === today.getMonth()
        && at.getFullYear() === today.getFullYear();
    return 'until ' + (sameDay ? time : `${at.toLocaleDateString([], { weekday: 'short' })} ${time}`);
}

/** Whether this scope's mute is its own, rather than one its community imposed. */
function ownsItsMute(view) {
    return !!view && view.mute_until !== 0;
}

/**
 * The level rows for one scope. A channel leads with the row that hands the
 * decision back to its community, so an inherited setting stays somewhere you
 * can see and return to.
 */
function notifyLevelItems(view, communityView, onChange) {
    if (!view) return [];
    const isChannel = !!communityView && communityView.scope_id !== view.scope_id;
    const items = [];
    if (isChannel) {
        items.push({
            label: `Use Community Default (${notifyLevelLabel(communityView.effective_level)})`,
            checked: view.level === null,
            onClick: () => onChange(null),
        });
    }
    for (const level of NOTIFY_LEVELS) {
        items.push({
            label: level.label,
            checked: view.level === level.value,
            onClick: () => onChange(level.value),
        });
    }
    return items;
}

/** Allow / Suppress for a community, greyed out while App Settings has the last word. */
function everyoneItems(communityView, onChange) {
    if (!communityView) return [];
    // The global kill switch wins outright, so a choice here would do nothing.
    // Show it anyway: the row is where someone looks for the reason.
    const globallyOff = !!communityView.everyone_muted_globally;
    const suppressed = communityView.suppress_everyone === true;
    return [
        { header: '@everyone' },
        {
            label: 'Allow',
            checked: !suppressed,
            disabled: globallyOff,
            hint: globallyOff ? 'off in App Settings' : undefined,
            onClick: () => onChange(false),
        },
        {
            label: 'Suppress',
            checked: suppressed,
            disabled: globallyOff,
            onClick: () => onChange(true),
        },
    ];
}

/** The duration rows, with an unmute at the top once something is in force. */
function muteItems(scopeId, view) {
    const items = [];
    const setMute = async (ms) => {
        try {
            await invoke('set_notify_mute', { scopeId, durationMs: ms });
        } catch (e) {
            showToast(e);
        }
    };
    // Mute unions downward, so a channel cannot lift the one its community set.
    // Offering Unmute there would be a button that does nothing.
    if (view && view.muted && !ownsItsMute(view)) {
        items.push({ label: 'Muted by the community', disabled: true });
        items.push({ divider: true });
    } else if (view && view.muted) {
        items.push({ label: 'Unmute', icon: 'volume-max', onClick: () => setMute(0) });
        items.push({ divider: true });
    }
    for (const choice of MUTE_DURATIONS) {
        items.push({ label: choice.label, onClick: () => setMute(choice.ms) });
    }
    return items;
}

/**
 * The two submenu entries every scope gets. `communityId` is null for a DM,
 * and equal to `scopeId` when the menu is the community's own.
 *
 * Nothing here repaints: every write ends in a `notify_prefs_changed` carrying
 * each chat's resolved pair, because a community-scope change moves rows this
 * menu never named.
 */
async function notifyMenuItems(scopeId, communityId) {
    const { self, community } = await loadNotifyViews(scopeId, communityId);
    // Say so rather than rendering a menu that quietly has no settings in it.
    if (!self) return [{ label: 'Notification settings unavailable', disabled: true }];

    const levels = notifyLevelItems(self, community, async (level) => {
        try {
            await invoke('set_notify_level', { scopeId, level });
        } catch (e) {
            showToast(e);
        }
    });

    // @everyone is a community-scope setting under a global kill switch; a
    // channel has no say, deliberately.
    const isCommunityMenu = !!communityId && communityId === scopeId;
    if (isCommunityMenu) {
        levels.push(...everyoneItems(community, async (suppress) => {
            try {
                await invoke('set_suppress_everyone', { communityId, suppress });
            } catch (e) {
                showToast(e);
            }
        }));
    }

    const hint = muteHint(self);
    return [
        { label: 'Notification Settings', icon: 'bell', submenu: levels },
        {
            label: self.muted ? 'Muted' : 'Mute',
            icon: self.muted ? 'volume-mute' : 'volume-max',
            hint: hint || undefined,
            submenu: muteItems(scopeId, self),
        },
    ];
}
