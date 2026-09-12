// A list avatar is served from the cache's thumbs directory (main.js getProfileAvatarSrc).
// A thumb that is not there yet (a cache still being backfilled) falls back to the original.
export function avatarFallback(src) {
    if (!src) return null;
    const m = src.match(/avatars(%2F|%5C|[\\/])thumbs\1/i);
    return m ? src.replace(m[0], 'avatars' + m[1]) : null;
}
