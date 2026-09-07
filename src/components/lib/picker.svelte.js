// The emoji picker's shell state: the equipped packs (the rail and their sections follow
// this order), the highlighted rail tab, the search query, and a recents version that
// moves when usage loads or an emoji is picked.
const picker = $state({ active: 'recents', query: '', recentsV: 0 });
let packs = $state.raw([]);

export function pickerState() { return picker; }
export function pickerPacks() { return packs; }
export function setPickerPacks(list) { packs = list.slice(); }
/** 'recents' | 'all' | a pack id */
export function setPickerActive(key) { picker.active = key; }
export function setPickerQuery(q) { picker.query = q; }
export function bumpPickerRecents() { picker.recentsV++; }
