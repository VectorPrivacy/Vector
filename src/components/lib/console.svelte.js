// The list pane's filters and the slim layout's Centre Console: the community rail open
// (Discord) or folded away (Telegram), the filter tab, the community the pane shows beside
// the rail, and the search text. widescreen.js owns the writes that reach past the pane.
import { flushSync } from 'svelte';
import { shellState } from './shell.svelte.js';

const FILTERS_OPEN = ['messages', 'requests'];
const FILTERS_FOLDED = ['all', 'messages', 'communities', 'requests'];

// `active`: signed in and slim, so the console is the list pane's layout. `layer`: a
// community channel is on screen, so the pane is its drawer (lib/drawer.js).
const cc = $state({ active: false, railOpen: true, filter: 'messages', space: null, query: '', layer: false });

export function consoleState() { return cc; }

/** The tabs on offer: communities live on the rail while it shows, as widescreen's always does. */
export function consoleFilters() { return cc.railOpen || shellState().ws ? FILTERS_OPEN : FILTERS_FOLDED; }

/** The tab in force, which a fold or an unfold can leave pointing at one that's gone. */
export function consoleFilter() {
    return consoleFilters().includes(cc.filter) ? cc.filter : consoleFilters()[0];
}

export function setConsoleActive(on) { if (cc.active !== !!on) { cc.active = !!on; flushSync(); } }
export function setConsoleRail(open) { cc.railOpen = !!open; flushSync(); }
export function setConsoleFilter(filter) { cc.filter = filter; }
export function setConsoleSpace(communityId) { cc.space = communityId || null; }
export function setConsoleQuery(text) { cc.query = text || ''; }
export function setConsoleLayer(on) { if (cc.layer !== !!on) { cc.layer = !!on; flushSync(); } }
