// The one image cropper, above every overlay. It answers with a rectangle in the source
// image's pixels, or null when cancelled; the caller owns the encode.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const cropOverlay = popOverlay({ src: '', aspect: 1, title: '', hint: '', preview: 'square', context: null });

let answer = null;

/**
 * `context` dresses the banner preview as the real pane: { width, background, name, iconSrc,
 * members, sections: [{ label, rows: [{ name, tier }] }] }.
 * @param {{ src: string, aspect: number, title: string, hint?: string, preview?: 'square' | 'banner', context?: object }} opts
 * @returns {Promise<{ x: number, y: number, w: number, h: number } | null>}
 */
export function openCropper(opts) {
    answer?.(null);
    return new Promise((resolve) => {
        answer = resolve;
        cropOverlay.open({ hint: '', preview: 'square', context: null, ...opts });
    });
}

export function closeCropper(rect) {
    const done = answer;
    answer = null;
    cropOverlay.close();
    done?.(rect);
}
