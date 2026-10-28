/**
 * deriveWindow (src/components/lib/chatwindow.js): which rows fold into a run's head.
 *
 *     node --test tests/chatwindow/
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { deriveWindow } from '../../src/components/lib/chatwindow.js';

const DAY = 86_400_000;
const BLOCKED = new Set(['bad', 'worse']);

let n = 0;
const row = (npub, at = 1_000) => ({ id: `m${++n}`, npub, at, content: 'x' });
const sys = (at = 1_000) => ({ id: `s${++n}`, at, system_event: { event_type: 'join', member_npub: 'someone' } });

const rules = {
    collapse: (prev, curr) => prev.npub === curr.npub,
    differentDay: (a, b) => Math.floor(a / DAY) !== Math.floor(b / DAY),
    isCommand: () => false,
    mergeable: () => false,
    blocked: (m) => BLOCKED.has(m.npub),
};
const derive = (msgs) => deriveWindow(msgs, 0, msgs.length, rules);
/** Each visible item as `npub×count`, merged ones dropped. */
const shape = (items) => items.filter((it) => !it.merged).map((it) => `${it.msg.npub ?? 'sys'}×${it.blockedRun?.length ?? 1}`);

test('one author\'s consecutive blocked messages fold into the first', () => {
    const msgs = [row('ok'), row('bad'), row('bad'), row('bad'), row('bad'), row('ok')];
    const items = derive(msgs);
    assert.deepEqual(shape(items), ['ok×1', 'bad×4', 'ok×1']);
    assert.deepEqual(items[1].blockedRun, msgs.slice(1, 5));
    assert.deepEqual(items.map((it) => it.merged), [false, false, true, true, true, false]);
    assert.equal(items[1].streak, 'first');
});

test('a lone blocked message stays as it was', () => {
    const items = derive([row('ok'), row('bad'), row('ok')]);
    assert.deepEqual(shape(items), ['ok×1', 'bad×1', 'ok×1']);
    assert.equal(items[1].blockedRun, null);
});

test('another author, a system event or a new day ends the run', () => {
    assert.deepEqual(shape(derive([row('bad'), row('bad'), row('worse'), row('worse')])), ['bad×2', 'worse×2']);
    assert.deepEqual(shape(derive([row('bad'), row('bad'), sys(), row('bad'), row('bad')])), ['bad×2', 'sys×1', 'bad×2']);
    const items = derive([row('bad', 1_000), row('bad', 2_000), row('bad', DAY + 1_000), row('bad', DAY + 2_000)]);
    assert.deepEqual(shape(items), ['bad×2', 'bad×2']);
    assert.ok(items.filter((it) => it.dayBreak).every((it) => !it.merged), 'a day separator never sits on a folded row');
});

test('a run reaching the window\'s end still folds', () => {
    const items = derive([row('ok'), row('bad'), row('bad'), row('bad')]);
    assert.deepEqual(shape(items), ['ok×1', 'bad×3']);
    assert.equal(items.at(-1).merged, true);
});
