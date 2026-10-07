// The open DM's wallpaper as the pane paints it: the layer's image and filter, whether a
// preview is staged (slider bar up, Cancel/Save over the header), the sliders' values,
// and the edit bar's label and lock. chat.js decides; the components render.
import { flushSync } from 'svelte';
import { streamerState } from './streamer.svelte.js';

const w = $state({
    image: '',            // CSS url(...) for the layer, '' for none
    filter: '',
    previewing: false,    // the slider bar is up and the layer shows the staged file
    editShown: false,     // the edit bar is in the layout (kept through its fade-out)
    editActive: false,    // the edit bar's opacity
    busy: false,
    label: 'Edit Mode is enabled.',
    blur: 5,
    dim: 50,
});
let editTimer = null;

export function wallpaperState() { return w; }

/** The image the pane paints: none while streaming with wallpapers hidden, except the user's own staged pick. */
export function wallpaperShown() {
    const st = streamerState();
    return st.on && st.hideWallpapers && !w.previewing ? '' : w.image;
}
export function setWallpaperLayer(image, filter) { w.image = image || ''; w.filter = filter || ''; flushSync(); }
export function setWallpaperSliders(blur, dim) { w.blur = blur; w.dim = dim; }
export function setWallpaperBusy(on) { w.busy = !!on; }
export function setWallpaperLabel(text) { w.label = text; }

/** The edit bar fades in from a fresh mount and out before it leaves the layout. */
export function setWallpaperPreviewing(on) {
    w.previewing = !!on;
    clearTimeout(editTimer);
    if (on) {
        w.editShown = true;
        w.editActive = false;
        flushSync();
        editTimer = setTimeout(() => { w.editActive = true; }, 10);
    } else {
        w.editActive = false;
        editTimer = setTimeout(() => { w.editShown = false; }, 250);
    }
    flushSync();
}
