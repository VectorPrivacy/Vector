// The emoji picker's shell state: the equipped packs (the rail and their sections follow
// this order), the highlighted rail tab, the search query, and a recents version that
// moves when usage loads or an emoji is picked.
const picker = $state({ active: 'recents', query: '', recentsV: 0, chromeV: 0 });
let packs = $state.raw([]);

export function pickerState() { return picker; }
export function pickerPacks() { return packs; }
export function setPickerPacks(list) { packs = list.slice(); }
/** 'recents' | 'all' | a pack id */
export function setPickerActive(key) { picker.active = key; }
export function setPickerQuery(q) { picker.query = q; }
export function bumpPickerRecents() { picker.recentsV++; }
/** The pack sections' header chrome was re-measured: their intrinsic sizes re-derive. */
export function bumpPickerChrome() { picker.chromeV = (picker.chromeV || 0) + 1; }

// The panel's chrome: which mode is up, whether the islands have been asked for (the
// stock grids are built on first open, not at boot), the creator view, and the creator's
// in-panel overlays. The app's flows write here; PickerPanel paints it.
const panel = $state({
    mode: 'emoji',         // 'emoji' | 'gif'
    ready: false,          // the rail, grids and pack sections render once true
    creatorOpen: false,    // the creator swaps in over the sections
    error: null,           // { pretitle, title, detail, button }
    progress: null,        // { title, detail }
    confirm: null,         // { title, detail, icon, tone, okText, cancelText }
    naming: null,          // { src, value, mode, batch, error }
    cropperOpen: false,    // the cropper leaf is always mounted; the app drives its stage
});
export function panelState() { return panel; }
export function setPanelMode(mode) { panel.mode = mode; }
export function setPickerReady() { panel.ready = true; }
export function setCreatorOpen(open) { panel.creatorOpen = !!open; }
export function setPickerError(e) { panel.error = e; }
export function setPickerProgress(p) { panel.progress = p; }
export function setPickerProgressDetail(detail) { if (panel.progress) panel.progress.detail = detail || ''; }
export function setPickerConfirm(c) { panel.confirm = c; }
export function setPickerNaming(n) { panel.naming = n; }
export function setPickerNamingError(message) { if (panel.naming) panel.naming.error = message || ''; }
export function setPickerCropperOpen(open) { panel.cropperOpen = !!open; }
