/**
 * The chat view's rendered window as a derivation (Phase 2c, CHAT_VIEW_ISLAND_DESIGN.md).
 *
 * One pass over `messages.slice(start, end)` yields everything the vanilla list used
 * to compute by reading sibling DOM: each row's streak state, where a day separator
 * goes, and which repeated system events fold into their run's head. Pure: the
 * rules come in as functions so the vanilla helpers stay the single definition.
 *
 * @param {object[]} messages  raw messages, chronological
 * @param {number} start       slice start (inclusive)
 * @param {number} end         slice end (exclusive)
 * @param {object} rules
 * @param {(prev, curr) => boolean} rules.collapse       shouldCollapseStreak
 * @param {(a, b) => boolean}       rules.differentDay   _dmsgIsDifferentDay
 * @param {(msg) => boolean}        rules.isCommand      a command invocation renders as a continuation
 * @param {(type) => boolean}       rules.mergeable      MERGEABLE_SYSTEM_EVENTS.has
 * @returns {Array<{ msg, kind: 'row'|'system', streak: 'first'|'continuation', dayBreak: boolean,
 *                   merged: boolean, mergeCount: number }>}
 */
export function deriveWindow(messages, start, end, rules) {
    const items = [];
    let prev = null;          // previous item (row or system event)
    let prevAt = null;        // previous timestamp with a value (day separators)
    let run = [];             // current mergeable system-event run (items)
    const flush = () => {
        if (run.length > 1) {
            run[0].mergeCount = run.length;
            for (let i = 1; i < run.length; i++) run[i].merged = true;
        }
        run = [];
    };
    for (let i = start; i < end; i++) {
        const msg = messages[i];
        if (!msg) continue;
        const isSystem = !!msg.system_event;
        const item = {
            msg,
            kind: isSystem ? 'system' : 'row',
            streak: 'first',
            dayBreak: false,
            merged: false,
            mergeCount: 1,
        };
        // A day separator heads the first day-content item and every day change.
        if (msg.at) {
            item.dayBreak = prevAt === null || rules.differentDay(prevAt, msg.at);
            prevAt = msg.at;
        }
        if (isSystem) {
            const type = msg.system_event.event_type;
            const npub = msg.system_event.member_npub;
            if (rules.mergeable(type)) {
                const head = run[0];
                if (head
                    && head.msg.system_event.event_type === type
                    && head.msg.system_event.member_npub === npub) {
                    run.push(item);
                } else {
                    flush();
                    run = [item];
                }
            } else {
                flush();
            }
        } else {
            // Anything between two rows (a system event, a day change) breaks the streak.
            flush();
            const prevRow = prev && prev.kind === 'row' ? prev.msg : null;
            item.streak = prevRow && rules.collapse(prevRow, msg) ? 'continuation' : 'first';
            if (rules.isCommand(msg)) item.streak = 'continuation';
        }
        items.push(item);
        prev = item;
    }
    flush();
    return items;
}
