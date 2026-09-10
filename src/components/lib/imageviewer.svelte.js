// The fullscreen image viewer: what it shows and how the image is placed. The zoom and
// pan arithmetic stays in previewer.js and writes here; the component paints it.
const v = $state({
    open: false, active: false, src: '',
    transform: 'translate(0, 0) scale(1)', zoomed: false,
    settling: false,            // hidden and unanimated until the first measured frame
    noAnim: false, dragging: false,   // the gesture's own transition suppression and cursor
    zoom: { text: '100%', visible: false },
    tip: { text: '', visible: false },
});
const els = { container: null, image: null };
let handlers = $state.raw({});   // close, rotate, load, error, wheel, mouseDown, touchStart, touchMove, touchEnd

export function imageViewerState() { return v; }
export function imageViewerEls() { return els; }
export function imageViewerHandlers() { return handlers; }
export function setImageViewerHandlers(h) { handlers = h || {}; }
export function setImageViewer(patch) { Object.assign(v, patch); }
export function setImageViewerZoom(text, visible) { v.zoom = { text, visible }; }
export function setImageViewerTip(text, visible) { v.tip = { text, visible }; }
