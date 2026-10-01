// Vector Web's keyed file stores (IndexedDB, memory) must answer as OPFS does.
// Run: node --test scripts/test-web-storage.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { keyedFiles, memoryStore, filesFor } from '../web/storage.js';

const bytes = (s) => new TextEncoder().encode(s);
const text = (b) => new TextDecoder().decode(b);

test('write, read, size and a missing file', async () => {
    const f = keyedFiles(memoryStore());
    await f.write('/a/b.txt', bytes('hello'));
    assert.equal(text(await f.read('/a/b.txt')), 'hello');
    assert.equal(await f.size('/a/b.txt'), 5);
    assert.equal(await f.read('/a/missing'), null);
    assert.equal(await f.size('/a/missing'), -1);
});

test('paths normalise: slashes collapse, no leading slash needed', async () => {
    const f = keyedFiles(memoryStore());
    await f.write('a//b/c.txt', bytes('x'));
    assert.equal(text(await f.read('/a/b/c.txt')), 'x');
});

test('a read is a copy the caller may detach', async () => {
    const f = keyedFiles(memoryStore());
    await f.write('/f', bytes('keep'));
    const first = await f.read('/f');
    structuredClone(first.buffer, { transfer: [first.buffer] });
    assert.equal(text(await f.read('/f')), 'keep');
});

test('list: direct files only, or everything beneath with recursive', async () => {
    const f = keyedFiles(memoryStore());
    await f.write('/d/one', bytes('1'));
    await f.write('/d/two', bytes('22'));
    await f.write('/d/sub/three', bytes('333'));
    await f.write('/dx/other', bytes('4'));
    assert.deepEqual((await f.list('/d', false)).sort(), [['one', 1], ['two', 2]]);
    assert.deepEqual((await f.list('/d', true)).sort(), [['one', 1], ['sub/three', 3], ['two', 2]]);
    assert.deepEqual(await f.list('/nowhere', true), []);
});

test('remove takes a file or a whole directory, never a sibling with the same prefix', async () => {
    const f = keyedFiles(memoryStore());
    await f.write('/d/one', bytes('1'));
    await f.write('/d/sub/two', bytes('2'));
    await f.write('/dx/keep', bytes('3'));
    assert.equal(await f.remove('/d'), true);
    assert.equal(await f.read('/d/one'), null);
    assert.equal(await f.read('/d/sub/two'), null);
    assert.equal(text(await f.read('/dx/keep')), '3');
    assert.equal(await f.remove('/d'), false);
});

test('removing the root is refused', async () => {
    const f = keyedFiles(memoryStore());
    await f.write('/a', bytes('1'));
    assert.equal(await f.remove('/'), false);
    assert.equal(await f.remove(''), false);
    assert.equal(text(await f.read('/a')), '1');
});

test('the memory budget refuses a write past it and counts rewrites once', async () => {
    const f = keyedFiles(memoryStore(10));
    await f.write('/a', bytes('12345678'));
    await f.write('/a', bytes('1234567890'));
    await assert.rejects(f.write('/b', bytes('1')), /Storage full/);
    await f.remove('/a');
    await f.write('/b', bytes('1'));
});

test('a failing store answers like a missing file, except on write', async () => {
    const broken = { put: async () => { throw new Error('boom'); }, get: async () => { throw new Error('boom'); }, size: async () => { throw new Error('boom'); }, delete: async () => { throw new Error('boom'); }, keys: async () => { throw new Error('boom'); } };
    const f = keyedFiles(broken);
    assert.equal(await f.read('/a'), null);
    assert.equal(await f.size('/a'), -1);
    assert.equal(await f.remove('/a'), false);
    assert.deepEqual(await f.list('/', true), []);
    await assert.rejects(f.write('/a', bytes('1')));
});

test('writes and removals are announced', async () => {
    const heard = [];
    const f = filesFor('memory', (p) => heard.push(p));
    await f.write('/a/b', bytes('1'));
    await f.remove('/a');
    await f.remove('/a');
    assert.deepEqual(heard, ['/a/b', '/a']);
});
