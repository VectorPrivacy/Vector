// A list avatar is served from the cache's thumbs directory (main.js getProfileAvatarSrc).
// A thumb that is not there yet (a cache still being backfilled) falls back to the original.
const THUMBS = /avatars(%2F|%5C|[\\/])thumbs\1/i;

/** `.../avatars/<file>` → `.../avatars/thumbs/<file>`: the 160 px still a small face decodes
 *  instead of the original (Avatar falls back to the original until it exists). */
export function avatarThumb(path) {
    const m = path?.match(/^(.*[\\/])avatars([\\/])([^\\/]+)$/);
    return m ? `${m[1]}avatars${m[2]}thumbs${m[2]}${m[3]}` : path;
}

export function avatarFallback(src) {
    if (!src) return null;
    // Vector Web without a service worker hands out placeholders that name no path.
    const path = window.__vectorWeb?.pathOf(src);
    if (path) {
        const m = path.match(THUMBS);
        return m ? window.__TAURI__.core.convertFileSrc(path.replace(m[0], 'avatars' + m[1])) : null;
    }
    const m = src.match(THUMBS);
    return m ? src.replace(m[0], 'avatars' + m[1]) : null;
}
