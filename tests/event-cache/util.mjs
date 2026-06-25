/**
 * Test utilities: seeded PRNG + event factories.
 *
 * The fuzz loop MUST be reproducible, so we never touch Math.random — a tiny
 * mulberry32 generator is seeded explicitly and the seed is printed on failure.
 */

/**
 * mulberry32 — deterministic 32-bit PRNG. Same seed → same stream.
 * @param {number} seed
 * @returns {() => number} a function returning a float in [0, 1)
 */
export function mulberry32(seed) {
    let a = seed >>> 0;
    return function () {
        a |= 0;
        a = (a + 0x6D2B79F5) | 0;
        let t = Math.imul(a ^ (a >>> 15), 1 | a);
        t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
}

/**
 * Make one synthetic displayable event. Only `.id` and `.at` matter to the
 * cache's ordering/dedup logic; the rest mirror the real shape loosely.
 * @param {string|number} id
 * @param {number} at - sort timestamp (ms)
 * @param {object} [extra]
 */
export function ev(id, at, extra = {}) {
    return { id: String(id), at, mine: false, pending: false, failed: false, ...extra };
}

/**
 * Make an ASC-by-.at run of events with ids prefixed by `prefix`.
 * @param {string} prefix
 * @param {number} startAt - .at of the first event
 * @param {number} count
 * @param {number} [step=1] - .at increment per event
 */
export function run(prefix, startAt, count, step = 1) {
    const out = [];
    for (let i = 0; i < count; i++) out.push(ev(`${prefix}-${i}`, startAt + i * step));
    return out;
}
