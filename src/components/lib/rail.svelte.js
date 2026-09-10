// The rail strip's scroller element, for the fade depths rail.js writes on scroll.
const els = { spacesRows: null };
export function railEls() { return els; }
export function bindRailEl(name) { return (node) => { els[name] = node; return { destroy() { els[name] = null; } }; }; }
