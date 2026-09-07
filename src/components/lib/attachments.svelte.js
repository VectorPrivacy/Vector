// Transfer progress by id, for the attachment components: uploads by the pending
// message's id, downloads by the attachment's id. The app's listeners write here.
import { SvelteMap } from 'svelte/reactivity';

const upload = new SvelteMap();
const download = new SvelteMap();

export function uploadProgress(pendingId) { return upload.get(pendingId) ?? null; }
export function setUploadProgress(pendingId, pct) { upload.set(pendingId, pct); }
export function clearUploadProgress(pendingId) { upload.delete(pendingId); }
export function downloadProgress(attachmentId) { return download.get(attachmentId) ?? null; }
export function setDownloadProgress(attachmentId, pct) { download.set(attachmentId, pct); }
export function clearDownloadProgress(attachmentId) { download.delete(attachmentId); }
