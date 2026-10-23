// What the Nexus reads off a listing: its kind, its tags, how a tag is said, and whether a
// search finds it. Pure, so every page agrees.

// `game` and `app` say what a listing IS; the rest say what it's like. Tags arrive
// trimmed and lowercased (marketplace.js syncMarketplaceApps).
const KINDS = new Set(['game', 'app']);
// Tags whose letters are an abbreviation, said the way players say them.
const SAID = { '2d': '2D', '3d': '3D', fps: 'FPS', rpg: 'RPG', mmo: 'MMO', pvp: 'PvP', dnd: 'D&D', asmr: 'ASMR', io: 'IO', kbm: 'Keyboard & Mouse' };

// What a listing says it's played with. `mobile`, `controller` and `keyboard` are the tags
// the publish form writes; the rest are the same thing said differently. An `-only` tag
// says it takes nothing else.
const TOUCH = new Set(['mobile', 'touch', 'touchscreen', 'tap', 'mobile-only', 'touch-only']);
const PAD = new Set(['controller', 'gamepad', 'controller-only', 'gamepad-only']);
const KEYS = new Set(['keyboard', 'mouse', 'kbm']);
const ONLY = new Set(['mobile-only', 'touch-only', 'controller-only', 'gamepad-only']);
// The one tag each control is browsed and written under.
const CANON = { touch: 'mobile', pad: 'controller', keys: 'keyboard' };
const canonical = (t) => ONLY.has(t) ? t : TOUCH.has(t) ? CANON.touch : PAD.has(t) ? CANON.pad : KEYS.has(t) ? CANON.keys : t;

/** A listing's tags as the publish form edits them: the free tags, and the controls as switches
 *  (an `-only` tag stays a free tag, so it isn't lost). */
export function splitControlTags(tags) {
    const plain = (set) => tags.some(t => set.has(t) && !ONLY.has(t));
    return {
        rest: tags.filter(t => ONLY.has(t) || !(TOUCH.has(t) || PAD.has(t) || KEYS.has(t))),
        touch: plain(TOUCH), pad: plain(PAD), keys: plain(KEYS),
    };
}
/** The switches back as tags. */
export function controlTags({ touch, pad, keys }) {
    return [touch && CANON.touch, pad && CANON.pad, keys && CANON.keys].filter(Boolean);
}

/** What a listing that doesn't fit is made for, said briefly: "a keyboard or a controller".
 *  One naming no controls is taken to be made for a keyboard and mouse. */
export function needsLabel(app) {
    const c = controlsOf(app);
    const parts = [c.keys && 'a keyboard', c.pad && 'a controller', c.touch && 'a touchscreen'].filter(Boolean);
    return parts.join(' or ') || 'a keyboard and mouse';
}

/** `{ touch, pad, keys, only }`: the controls a listing names. */
export function controlsOf(app) {
    const c = app.categories || [];
    return { touch: c.some(t => TOUCH.has(t)), pad: c.some(t => PAD.has(t)), keys: c.some(t => KEYS.has(t)), only: c.some(t => ONLY.has(t)) };
}

/**
 * Whether a listing plays on a device `{ touch, pad, keys }`. A web app takes a keyboard and
 * mouse unless it says it takes only something else; a device without them needs a listing
 * that names touch, or a controller it has.
 */
export function fitsDevice(app, d) {
    const c = controlsOf(app);
    if (d.keys && !c.only) return true;
    if (d.touch && c.touch) return true;
    return d.pad && c.pad;
}

export const isGame = (app) => {
    const c = app.categories || [];
    return c.includes('game') || !c.includes('app');
};
export const isInstalled = (app) => !!(app.installed || app.local_path);
/** A category page's apps; 'app' is every listing that isn't a game. */
export const inCategory = (app, cat) => cat === 'app' ? !isGame(app)
    : cat === CANON.touch ? controlsOf(app).touch
    : cat === CANON.pad ? controlsOf(app).pad
    : cat === CANON.keys ? controlsOf(app).keys
    : (app.categories || []).includes(cat);
export const isMultiplayer = (app) => (app.categories || []).includes('multiplayer');
export const tagsOf = (app) => (app.categories || []).filter(c => !KINDS.has(c));
export const tagLabel = (tag) => SAID[tag] || tag.split(/([ -])/).map(w => w.charAt(0).toUpperCase() + w.slice(1)).join('');

/** The tag a listing is best summed up by: its first that isn't how it's played with others. */
export function leadTag(app) {
    const tags = tagsOf(app);
    return tags.find(t => t !== 'multiplayer') || tags[0] || (isGame(app) ? 'game' : 'app');
}

export const newestFirst = (list) => [...list].sort((a, b) => b.published_at - a.published_at);

/** Every tag with how many listings carry it, the commonest first. */
export function tagCounts(apps) {
    const count = new Map();
    for (const app of apps) for (const t of new Set(tagsOf(app).map(canonical))) count.set(t, (count.get(t) || 0) + 1);
    return [...count].map(([tag, n]) => ({ tag, n })).sort((a, b) => b.n - a.n || a.tag.localeCompare(b.tag));
}

/** A search (already lowercased) finds a listing by its name, its tags, its maker or its words. */
export function matches(app, q) {
    if (!q) return true;
    return app.name.toLowerCase().includes(q)
        || (app.categories || []).some(c => c.includes(q) || tagLabel(c).toLowerCase().includes(q))
        || (app.developer || '').toLowerCase().includes(q)
        || (app.description || '').toLowerCase().includes(q);
}

/** Name hits before word hits, so typing a title puts it first. */
export function searchRank(app, q) {
    const name = app.name.toLowerCase();
    if (name === q) return 0;
    if (name.startsWith(q)) return 1;
    if (name.includes(q)) return 2;
    if ((app.categories || []).some(c => c.includes(q))) return 3;
    return 4;
}
