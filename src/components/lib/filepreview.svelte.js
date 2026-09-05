// The send-file preview's state (Phase 4). The vanilla module keeps the SOURCES
// (a path, cached bytes, a File object, a zip in progress) and the send; this is
// what the overlay renders and the choices the user makes in it.
const fp = $state({
    open: false,
    stem: '',
    edited: false,       // the user renamed the stem (or a zip supplied its folder name)
    ext: '',
    size: '',
    spoiler: false,
    compress: false,     // the Compress Image option is offered
    compressChecked: true,
    compressInfo: 'Compressing...',
    metadata: false,     // the Keep Metadata option is shown (the image carries EXIF)
    metadataChecked: false,
    publish: false,
    sendDisabled: false,
    sendLabel: 'Send',
});
// The content area's model, raw: a zip's file list can run to thousands of entries.
// { kind: 'image', src, path } | { kind: 'video', src } | { kind: 'icon', icon }
// | { kind: 'miniapp', icon, name } | { kind: 'zip-progress', percent }
// | { kind: 'zip', files, total }
let content = $state.raw(null);

export function filePreview() { return fp; }
export function filePreviewContent() { return content; }

/** Show the overlay for a new file. Every field resets; `patch` sets what differs. */
export function openFilePreview(patch = {}) {
    fp.open = true;
    fp.stem = '';
    fp.edited = false;
    fp.ext = '';
    fp.size = '';
    fp.spoiler = false;
    fp.compress = false;
    fp.compressChecked = true;
    fp.compressInfo = 'Compressing...';
    fp.metadata = false;
    fp.metadataChecked = false;
    fp.publish = false;
    fp.sendDisabled = false;
    fp.sendLabel = 'Send';
    content = null;
    Object.assign(fp, patch);
}
export function closeFilePreview() {
    fp.open = false;
}
export function setFilePreviewContent(next) {
    content = next;
}
/** Patch fields on the open preview (size text, compress info, publish, send button). */
export function patchFilePreview(patch) {
    Object.assign(fp, patch);
}
