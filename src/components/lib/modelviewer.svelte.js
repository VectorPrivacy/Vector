// The fullscreen 3D model viewer: which model, and the file action this platform offers
// ('reveal' on desktop, 'share' on Android, 'save' on Vector Web). previewer.js opens and closes it.
const v = $state({ open: false, active: false, url: '', ext: '', name: '', fileAction: null });
let handlers = $state.raw({});   // close, file

export function modelViewerState() { return v; }
export function setModelViewer(patch) { Object.assign(v, patch); }
export function modelViewerHandlers() { return handlers; }
export function setModelViewerHandlers(h) { handlers = h || {}; }
