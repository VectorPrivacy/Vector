// How the app names an attachment's bytes to the backend.

/** Its hash while this account holds the bytes in memory, else its file. */
export function mediaPath(att) {
    return att.resident ? `vmedia:${att.id}` : att.path;
}

/** Whether a bubble can draw the attachment now: on disk, or held in memory. */
export function attachmentReady(att) {
    return !!(att.downloaded || att.resident);
}

/** Calls `seen` once `node` comes within a screen of the chat's visible area, then stops. */
export function nearScreen(node, seen) {
    const io = new IntersectionObserver((entries) => {
        if (entries.some((e) => e.isIntersecting)) {
            io.disconnect();
            seen();
        }
    }, { root: node.closest('.chat-messages'), rootMargin: '100% 0px' });
    io.observe(node);
    return { destroy: () => io.disconnect() };
}
