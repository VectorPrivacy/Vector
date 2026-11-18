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
// Message ids the reader chose to show.
const REVEALED = new Set();

let n = 0;
const row = (npub, at = 1_000) => ({ id: `m${++n}`, npub, at, content: 'x' });
const sys = (at = 1_000) => ({ id: `s${++n}`, at, system_event: { event_type: 'join', member_npub: 'someone' } });

const rules = {
    collapse: (prev, curr) => prev.npub === curr.npub,
    differentDay: (a, b) => Math.floor(a / DAY) !== Math.floor(b / DAY),
    isCommand: () => false,
    mergeable: () => false,
    blocked: (m) => BLOCKED.has(m.npub) && !REVEALED.has(m.id),
    revealed: (m) => BLOCKED.has(m.npub) && REVEALED.has(m.id),
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

test('a shown run is headed by its first row, so one click hides it all again', () => {
    const msgs = [row('ok'), row('bad'), row('bad'), row('bad'), row('ok')];
    for (const m of msgs.slice(1, 4)) REVEALED.add(m.id);
    const items = derive(msgs);
    assert.deepEqual(items.map((it) => it.revealedRun?.length ?? 0), [0, 3, 0, 0, 0]);
    assert.deepEqual(items[1].revealedRun, msgs.slice(1, 4));
    assert.ok(items.every((it) => !it.merged && !it.blockedRun), 'shown rows stand on their own');
    for (const m of items[1].revealedRun) REVEALED.delete(m.id);
    assert.deepEqual(shape(derive(msgs)), ['ok×1', 'bad×3', 'ok×1'], 'hidden, they fold back into one');
});

test('a lone shown message heads its own run', () => {
    const msgs = [row('ok'), row('bad'), row('ok')];
    REVEALED.add(msgs[1].id);
    assert.deepEqual(derive(msgs).map((it) => it.revealedRun?.length ?? 0), [0, 1, 0]);
});

test('another author, a system event, a new day or a still-hidden row ends a shown run', () => {
    const heads = (msgs) => derive(msgs).map((it) => it.revealedRun?.length ?? 0);
    const show = (msgs) => { for (const m of msgs) if (m.npub) REVEALED.add(m.id); return msgs; };
    assert.deepEqual(heads(show([row('bad'), row('bad'), row('worse'), row('worse')])), [2, 0, 2, 0]);
    assert.deepEqual(heads(show([row('bad'), row('bad'), sys(), row('bad')])), [2, 0, 0, 1]);
    assert.deepEqual(heads(show([row('bad', 1_000), row('bad', 2_000), row('bad', DAY + 1_000)])), [2, 0, 1]);
    const mixed = [row('bad'), row('bad'), row('bad'), row('bad')];
    REVEALED.add(mixed[0].id);
    REVEALED.add(mixed[3].id);
    const items = derive(mixed);
    assert.deepEqual(items.map((it) => it.revealedRun?.length ?? 0), [1, 0, 0, 1]);
    assert.equal(items[1].blockedRun?.length, 2, 'the hidden rows between still fold together');
});
