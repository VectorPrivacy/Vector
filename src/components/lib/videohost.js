// Video elements that outlive the component showing them. A playing video moves between its
// chat row and the floating player instead of being created again: a new element reloads the
// file, and that reload is the hitch in picture and sound.
//
// Each element has one owner, the host that last created or adopted it. A host tearing down
// stops its element only while it still owns it, so a hand-off is never undone by the host
// that gave the element away.

const videos = new Map();   // key -> { el, owner }
let parking = null;
let seq = 0;

/** A new element playing `src`, owned by `owner`. */
export function createVideo(src, owner) {
    const key = `v${++seq}`;
    const el = document.createElement('video');
    el.setAttribute('playsinline', '');
    el.setAttribute('controlslist', 'nodownload');
    el.preload = 'metadata';
    el.src = src;
    videos.set(key, { el, owner });
    return { key, el };
}

/** Take over the live element `key` from whoever held it, or null when it is gone. */
export function adoptVideo(key, owner) {
    const v = key ? videos.get(key) : null;
    if (!v) return null;
    v.owner = owner;
    return v.el;
}

/** The playback lane of element `key`: every host holding it claims the same one, so a hand-off
 *  never stops the video it is handing on. */
export function videoLane(key) {
    return `video:${key}`;
}

export function ownsVideo(key, owner) {
    return videos.get(key)?.owner === owner;
}

/**
 * Keep `key` playing, out of sight, until another host adopts it. It stays in the page: an
 * element taken out of the document is paused at the end of the task.
 */
export function parkVideo(key) {
    const v = videos.get(key);
    if (!v) return;
    if (!parking) {
        parking = document.createElement('div');
        parking.className = 'video-parking';
        parking.setAttribute('aria-hidden', 'true');
        document.body.append(parking);
    }
    v.owner = null;
    parking.append(v.el);
}

/** Stop `key` and forget it, letting go of its file. */
export function dropVideo(key) {
    const v = videos.get(key);
    if (!v) return;
    videos.delete(key);
    v.el.pause();
    v.el.removeAttribute('src');
    v.el.load();
    v.el.remove();
}
