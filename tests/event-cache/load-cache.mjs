/**
 * Loader for the UNMODIFIED src/js/event-cache.js.
 *
 * event-cache.js is a concatenated-bundle global script: it declares top-level
 * `const`/`class` with NO module exports, and it references browser/Tauri
 * globals (`invoke`, `window`). We must not touch the file (the bundler copies
 * all of src/), so instead we:
 *   1. Read the source as a string.
 *   2. Append an IN-MEMORY shim that hangs the real identifiers on globalThis.
 *   3. Run source + shim in a vm context whose sandbox stubs every external
 *      global the script touches.
 *   4. Read the re-exported identifiers back off the context.
 *
 * Nothing is written to disk; the file on disk stays byte-for-byte identical.
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import vm from 'node:vm';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SOURCE_PATH = resolve(__dirname, '../../src/js/event-cache.js');

/**
 * Load a FRESH, isolated copy of the cache module.
 *
 * Each call builds a brand-new vm context so tests can't leak the `eventCache`
 * singleton or EVENT_CACHE_CONFIG mutations into one another.
 *
 * @returns {{ EventCache: Function, ConversationCacheEntry: Function,
 *             eventCache: object, EVENT_CACHE_CONFIG: object,
 *             invokeCalls: Array }}
 */
export function loadCache() {
    const source = readFileSync(SOURCE_PATH, 'utf8');

    // The in-memory shim. Uses the REAL identifiers found in the source:
    //   - class EventCache
    //   - class ConversationCacheEntry
    //   - const eventCache  (singleton)
    //   - const EVENT_CACHE_CONFIG
    const shim = `
;globalThis.__CACHE__ = {
    EventCache: EventCache,
    ConversationCacheEntry: ConversationCacheEntry,
    eventCache: eventCache,
    EVENT_CACHE_CONFIG: EVENT_CACHE_CONFIG,
};`;

    // Record every invoke() the script makes at load/drive time so tests can
    // assert the pure methods never reach for the backend.
    const invokeCalls = [];

    // Stubs for EVERY external global the source references:
    //   - invoke  : async Tauri bridge. Used by loadInitialEvents /
    //               loadMoreEvents / _evictIfNeeded. Returns [] so any
    //               accidental call resolves harmlessly to "no rows".
    //   - window  : line 791 does `window.eventCache = eventCache`.
    //   - console : the source has none, but keep a real one for safety.
    //   - Date/Set/Map/Math/Infinity : Node natives, exposed via globalThis.
    const sandbox = {
        invoke: async (cmd, args) => {
            invokeCalls.push({ cmd, args });
            // get_chat_message_count expects a number; everything else an array.
            if (cmd === 'get_chat_message_count') return 0;
            return [];
        },
        window: {},
        console,
        Date,
        Set,
        Map,
        Math,
        Infinity,
        structuredClone: globalThis.structuredClone,
        performance: globalThis.performance,
    };
    // Let the script's top-level `const`s resolve against this same object.
    sandbox.globalThis = sandbox;

    const context = vm.createContext(sandbox);
    vm.runInContext(source + shim, context, { filename: 'event-cache.js' });

    const exported = context.__CACHE__;
    return {
        EventCache: exported.EventCache,
        ConversationCacheEntry: exported.ConversationCacheEntry,
        eventCache: exported.eventCache,
        EVENT_CACHE_CONFIG: exported.EVENT_CACHE_CONFIG,
        invokeCalls,
    };
}

export { SOURCE_PATH };
