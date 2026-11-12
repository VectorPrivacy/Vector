/**
 * diffWords / foldUnchanged (src/components/lib/textdiff.js): what the edit history shows.
 *
 *     node --test tests/textdiff/
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { diffWords, foldUnchanged, changeCounts } from '../../src/components/lib/textdiff.js';

const rebuild = (ops, side) => ops.filter((r) => r.op === 'eq' || r.op === side).map((r) => r.text).join('');

test('both sides rebuild exactly from the runs', () => {
    const pairs = [
        ['', 'hello'], ['hello', ''], ['a b c', 'a b c'],
        ['the quick brown fox', 'the slow brown dog'],
        ['For any questions, tag @Team.', 'For any questions, tag @Team or @Mods.\n\nThanks!'],
        ['x  y\n\nz', 'x y\nz w'],
        [':cat: says hi', ':dog: says hi :cat:'],
    ];
    for (const [a, b] of pairs) {
        const ops = diffWords(a, b);
        assert.equal(rebuild(ops, 'del'), a, JSON.stringify([a, b]));
        assert.equal(rebuild(ops, 'ins'), b, JSON.stringify([a, b]));
    }
});

test('a changed word is one deletion then one insertion', () => {
    assert.deepEqual(diffWords('the quick brown fox', 'the slow brown fox'), [
        { op: 'eq', text: 'the ' }, { op: 'del', text: 'quick' }, { op: 'ins', text: 'slow' }, { op: 'eq', text: ' brown fox' },
    ]);
});

test('a lone space between changes joins them', () => {
    const ops = diffWords('one two three', 'uno dos three');
    assert.deepEqual(ops, [{ op: 'del', text: 'one two' }, { op: 'ins', text: 'uno dos' }, { op: 'eq', text: ' three' }]);
});

test('a shortcode is never split', () => {
    const ops = diffWords('hi :cat:', 'hi :cats:');
    assert.ok(ops.some((r) => r.op === 'del' && r.text === ':cat:'));
});

test('long unchanged stretches fold to context around the change', () => {
    const words = Array.from({ length: 200 }, (_, i) => `w${i}`);
    const before = words.join(' ');
    const after = [...words.slice(0, 100), 'NEW', ...words.slice(100)].join(' ');
    const folded = foldUnchanged(diffWords(before, after), 5);
    assert.deepEqual(folded.map((r) => r.op), ['gap', 'eq', 'ins', 'eq', 'gap']);
    assert.equal(folded[0].words + 5, 100);
    assert.match(folded[1].text, /^w95 w96 w97 w98 w99 $/);
    assert.equal(folded[2].text, 'NEW ');
    assert.equal(folded[4].words + 5, 100);
});

test('a short unchanged run stays whole, an unchanged text folds to one gap', () => {
    assert.deepEqual(foldUnchanged(diffWords('a b c', 'a x c'), 8).map((r) => r.op), ['eq', 'del', 'ins', 'eq']);
    const ten = 'a b c d e f g h i j';
    assert.deepEqual(foldUnchanged(diffWords(ten, ten), 8), [{ op: 'gap', words: 10, lines: false }]);
});

test('very long rewrites stay bounded', () => {
    const a = Array.from({ length: 3000 }, (_, i) => `a${i}`).join(' ');
    const b = Array.from({ length: 3000 }, (_, i) => `b${i}`).join(' ');
    const t = Date.now();
    const ops = diffWords(a, b);
    assert.ok(Date.now() - t < 2000);
    assert.equal(rebuild(ops, 'del'), a);
    assert.equal(rebuild(ops, 'ins'), b);
});

test('counts words added and removed', () => {
    assert.deepEqual(changeCounts(diffWords('a b c', 'a x y c')), { added: 2, removed: 1 });
});

test('punctuation stays put when words are added before it', () => {
    assert.deepEqual(diffWords('with kindness.', 'with kindness and patience.'), [
        { op: 'eq', text: 'with kindness' }, { op: 'ins', text: ' and patience' }, { op: 'eq', text: '.' },
    ]);
});

test('a paragraph added after the last sentence keeps that sentence whole', () => {
    const ops = diffWords('Off-topic discussion.', 'Off-topic discussion.\n\nFor questions, tag @Team.');
    assert.deepEqual(ops, [{ op: 'eq', text: 'Off-topic discussion.' }, { op: 'ins', text: '\n\nFor questions, tag @Team.' }]);
});

test('a removed clause leaves no lone space behind as an insertion', () => {
    const ops = diffWords('with messages, images, or links', 'with messages or links');
    assert.ok(!ops.some((r) => r.op === 'ins'), JSON.stringify(ops));
    assert.equal(rebuild(ops, 'del'), 'with messages, images, or links');
    assert.equal(rebuild(ops, 'ins'), 'with messages or links');
});

test('folds keep a changed line whole and fold the lines around it', () => {
    const before = '# Rules\n\n**• Respect** Treat everyone well.\n**• Clean** Keep it clean.\n\nFor any questions, tag @Admin.';
    const after = before.replace('@Admin', '@Team');
    const folded = foldUnchanged(diffWords(before, after));
    assert.deepEqual(folded.map((r) => r.op), ['gap', 'eq', 'del', 'ins', 'eq']);
    assert.ok(folded[0].lines, 'the fold spans lines');
    assert.equal(folded[1].text, 'For any questions, tag ');
    assert.ok(!folded.some((r) => r.op === 'eq' && r.text.includes('**')), 'no bold marker is cut in half');
});

test('a changed bold word keeps its markers outside the change', () => {
    const ops = diffWords('**• Rule 3**', '**• Rule Three**');
    assert.deepEqual(ops, [{ op: 'eq', text: '**• Rule ' }, { op: 'del', text: '3' }, { op: 'ins', text: 'Three' }, { op: 'eq', text: '**' }]);
});

test('a pack emoji with a tilde in its shortcode is one token', () => {
    const ops = diffWords('Really happy! :love~3: :love~3:', 'Really happy! :love~3:');
    assert.deepEqual(ops, [{ op: 'eq', text: 'Really happy! :love~3:' }, { op: 'del', text: ' :love~3:' }]);
});
