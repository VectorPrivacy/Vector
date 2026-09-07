// The GIF panel: what the grid shows. The app fetches (trending, search, load-more) and
// writes here; the grid derives skeletons, items, the load-more tail and the empty state.
const g = $state({ phase: 'idle', message: '', loadingMore: false, pageSize: 12 });
let items = $state.raw([]);   // [{ id, title, thumb }]

export function gifState() { return g; }
export function gifItems() { return items; }
export function gifLoading(pageSize) { g.phase = 'loading'; g.loadingMore = false; if (pageSize) g.pageSize = pageSize; items = []; }
export function gifResults(list, append) { items = append ? items.concat(list) : list; g.phase = 'ok'; g.loadingMore = false; }
export function gifEmpty(message) { items = []; g.phase = 'empty'; g.message = message; g.loadingMore = false; }
export function gifLoadingMore(on) { g.loadingMore = !!on; }
