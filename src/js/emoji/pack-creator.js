// The pack creator: the in-panel editor, its grid, cropper, naming and save flow.
// One global scope: loads before picker.js and shares its globals.

// ============================================================================
// Pack Creator — in-panel view
// ============================================================================
//
// Replaces the emoji grid sections inside `.emoji-main` while editing one
// of the user's own packs. Uploads + the kind 30030 publish are deferred
// until the user exits the creator (clicks a sidebar tab, hits the close
// pencil, or closes the picker) so an abandoned edit costs zero network.

// Pack-creator per-pack emoji cap. `let` so the Vector badge can raise it
// at runtime (see `applyTierLimits`).
let PC_MAX_EMOJIS = 30;
const PC_MAX_FILE_BYTES = 256 * 1024;

const _pc = {
    open: false,
    mode: 'create',          // 'create' | 'edit'
    editingId: null,
    editingIdentifier: null,
    name: '',
    logoUrl: '',             // existing remote URL (edit mode)
    logoFile: null,          // File pending upload
    logoBlobUrl: '',
    emojis: [],              // Array<{ shortcode, url?, file?, blobUrl? }>
    // Remote URLs queued for Blossom cleanup at next save. Populated by
    // _pcRemoveEmoji (emoji × badge) and _pcSetLogoFile (logo replace).
    // Cleared after a successful publish; persists across save failures
    // so retries don't leak the orphan.
    pendingBlobDeletes: [],
    saving: false,
    dirty: false,            // tracks unsaved changes for auto-save on exit
};

// Remote URLs whose host definitively deleted the file (404/410). The editor
// renders from the local cache, which keeps serving bytes the host removed
// long ago — so the creator's own device needs an explicit remote probe to
// see the damage every cache-cold client already sees.
const _pcDeadUrls = new Set();

function _pcGoneMessage() {
    return emojiUnavailableMessage('404 not found').replace(/<br\s*\/?>/gi, ' ');
}

function _pcMarkCellBroken(idx, message) {
    VectorSvelte.markCreatorBroken(idx, message);
}

/** Probe the pack's remote media past the warm cache and mark dead cells.
 *  Only a definitive 404/410 marks anything — a flaky host stays quiet. */
async function _pcVerifyRemoteMedia() {
    const packId = _pc.editingId;
    const urls = _pc.emojis.map(e => e.url).filter(u => _isCacheableEmojiUrl(u));
    if (_isCacheableEmojiUrl(_pc.logoUrl)) urls.push(_pc.logoUrl);
    if (!urls.length) return;
    let results;
    try {
        results = await invoke('verify_remote_media', { urls });
    } catch (e) {
        console.warn('[pack-creator] remote media verify failed:', e);
        return;
    }
    const gone = results.filter(r => r.gone);
    if (!gone.length) return;
    for (const r of gone) _pcDeadUrls.add(r.url);
    // The editor may have closed or switched packs while we probed.
    if (!_pc.open || _pc.editingId !== packId) return;
    // Mark by index rather than re-syncing: a re-render mid-drag would yank cells out
    // from under the pointer.
    _pc.emojis.forEach((e, idx) => { if (e.url && _pcDeadUrls.has(e.url)) _pcMarkCellBroken(idx, _pcGoneMessage()); });
    if (_pcDeadUrls.has(_pc.logoUrl)) _pcRenderLogo();
}

/** URLs a blob cleanup must NOT delete: Blossom is content-addressed, so a
 *  removed cell's queued URL can be the very blob a re-add of identical
 *  bytes just published, or a blob another pack shares. Extras carry the
 *  just-published pack's URLs (arrEmojiPacks is stale at drain time). */
function _pcLiveBlobUrls(extras = []) {
    const live = new Set(extras.filter(Boolean));
    for (const p of arrEmojiPacks) {
        if (p.id === _pc.editingId) continue;
        for (const em of (p.emojis || [])) if (em.url) live.add(em.url);
        if (p.image_url) live.add(p.image_url);
    }
    return live;
}

function openEmojiPackCreator(id) {
    const editingPack = id ? arrEmojiPacks.find(p => p.id === id) : null;
    // Creating a new pack while at the equipped-pack cap would be
    // rejected by the backend — surface the same overlay we use for
    // other publish failures so the user gets actionable feedback.
    // Editing an existing own pack is always fine (no slot change).
    if (!editingPack && _userPackCount() >= MAX_EQUIPPED_PACKS) {
        _pcShowSlotFullError();
        return;
    }

    _pcRevokeBlobUrls();
    _pcDeadUrls.clear();
    _pc.open = true;
    _pc.saving = false;
    _pc.dirty = false;
    _pc.logoFile = null;
    _pc.logoBlobUrl = '';
    _pc.pendingBlobDeletes = [];
    _pc.savedPackId = null;
    if (editingPack) {
        _pc.mode = 'edit';
        _pc.editingId = editingPack.id;
        _pc.editingIdentifier = editingPack.identifier;
        _pc.name = editingPack.title || '';
        _pc.logoUrl = editingPack.image_url || '';
        _pc.emojis = editingPack.emojis.map(e => ({ shortcode: e.shortcode, url: e.url }));
    } else {
        _pc.mode = 'create';
        _pc.editingId = null;
        _pc.editingIdentifier = null;
        _pc.name = '';
        _pc.logoUrl = '';
        _pc.emojis = [];
    }
    VectorSvelte.setCreatorOpen(true);
    _pcRenderGrid();
    if (editingPack) _pcVerifyRemoteMedia();
    // Reset the scroll container to the top — without this, opening
    // edit mode for a pack while the picker was already scrolled to
    // that pack's section leaves the creator showing its grid bottom
    // instead of the pack title.
    if (_pickerEls.main) _pickerEls.main.scrollTop = 0;
    // Focus the title for immediate-edit affordance — but only on
    // desktop. On mobile the focus pops the soft keyboard, which lands
    // on top of the creator UI and is jarring when the user might not
    // even want to edit the name (e.g. just opened to add emojis).
    const isMobile = typeof platformFeatures !== 'undefined' && platformFeatures.is_mobile;
    if (!isMobile) VectorSvelte.focusCreatorName();
}

/** Saves pending edits (if dirty) and switches back to the normal
 *  emoji-section view. Safe to call multiple times. */
// Returns true when the creator actually closed, false when a failed save kept
// it open (so awaited callers don't tear the view out from under a live batch).
async function closeEmojiPackCreator() {
    if (!_pc.open) return true;
    // A save in flight owns teardown/close. A concurrent close (sidebar tab,
    // panel toggle) must no-op — otherwise it wipes _pc.emojis mid-upload and a
    // later background failure loses the whole batch.
    if (_pc.saving) return false;
    if (_pc.dirty && !_pc.saving) {
        const saved = await _pcSave();
        // On failure keep the editor open with previews intact so the user can
        // retry; _pcSave surfaced the reason. Wiping below would drop the batch.
        if (!saved) return false;
    }
    _pc.open = false;
    VectorSvelte.setCreatorOpen(false);
    _pcRevokeBlobUrls();
    _pc.emojis = [];
    _pc.logoFile = null;
    // Land on the pack we were editing (saved or not) instead of the open-time
    // scroll-to-top, so its emojis are right there. rAF lets the un-hidden
    // sections lay out first.
    const savedId = _pc.savedPackId || _pc.editingId;
    _pc.savedPackId = null;
    if (savedId) {
        requestAnimationFrame(() => {
            const section = _pickerEls.main?.querySelector(
                `.emoji-pack-section[data-pack-id="${CSS.escape(savedId)}"]`);
            if (!section) return;
            // Expand first — landing on a collapsed section hides the very
            // emojis the user just edited.
            section.classList.remove('collapsed');
            section.scrollIntoView({ block: 'start' });
        });
    }
    return true;
}

function _pcRevokeBlobUrls() {
    if (_pc.logoBlobUrl) {
        try { URL.revokeObjectURL(_pc.logoBlobUrl); } catch (_e) {}
        _pc.logoBlobUrl = '';
    }
    for (const e of _pc.emojis) {
        if (e.blobUrl) {
            try { URL.revokeObjectURL(e.blobUrl); } catch (_e) {}
            e.blobUrl = '';
        }
    }
}

function _pcRenderGrid() {
    VectorSvelte.setCreator({
        name: _pc.name,
        logo: { blobUrl: _pc.logoBlobUrl || '', url: _pc.logoUrl || '', dead: !!_pc.logoUrl && _pcDeadUrls.has(_pc.logoUrl) },
        emojis: _pc.emojis.map(e => ({ shortcode: e.shortcode, url: e.url || '', blobUrl: e.blobUrl || '', dead: !!e.url && _pcDeadUrls.has(e.url) })),
        max: PC_MAX_EMOJIS,
    });
}
const _pcRenderLogo = _pcRenderGrid;

function _pcSanitizeShortcode(s) {
    return String(s).replace(/[^a-zA-Z0-9_-]/g, '').slice(0, 22);
}

function _pcShortcodeFromFilename(name) {
    const base = String(name || 'emoji').replace(/\.[^.]+$/, '');
    let sc = _pcSanitizeShortcode(base);
    if (!sc) sc = 'emoji';
    const seen = new Set(_pc.emojis.map(e => e.shortcode));
    if (!seen.has(sc)) return sc;
    let i = 2;
    while (seen.has(`${sc}_${i}`)) i++;
    return `${sc}_${i}`;
}

function _pcClearDropMarkers() {
    const grid = _pickerEls.creatorGrid;
    if (!grid) return;
    grid.querySelectorAll('.drop-before, .drop-after').forEach(c => {
        c.classList.remove('drop-before', 'drop-after');
    });
}

// Reorder via pointer events. We can't use the HTML5 drag API because
// Tauri's `dragDropEnabled: true` swallows DOM drag events at the native
// layer (it's needed for the OS file-drop → onDragDropEvent pipeline).
const _PC_DRAG_THRESHOLD_PX = 6;
let _pcDragActive = false;
function _pcInstallReorderHandlers(cell, idx) {
    let ghost = null;
    let ghostOffsetX = 0;
    let ghostOffsetY = 0;

    installReorderGestures(cell, {
        armClass: 'is-drag-armed',
        // The remove-× is its own target; a press there must never become a drag.
        ignore: (ev) => !!ev.target.closest('.emoji-creator-cell-remove'),
        onMenu: (x, y) => {
            if (typeof showContextMenu !== 'function') return;
            showContextMenu({
                x, y,
                items: [
                    { label: 'Rename Emoji', icon: 'edit',  onClick: () => _pcRenameEmoji(idx) },
                    { label: 'Delete Emoji', icon: 'trash', danger: true, onClick: () => _pcRemoveEmoji(idx) },
                ],
            });
        },
        onDragStart: () => {
            _pcDragActive = true;
            cell.classList.add('is-dragging');
            // Clear any stuck hover state — we own this class now, so a drag
            // start is the right moment to normalize it.
            const grid = _pickerEls.creatorGrid;
            if (grid) {
                grid.querySelectorAll('.is-hovered').forEach(c =>
                    c.classList.remove('is-hovered'));
            }
            const rect = cell.getBoundingClientRect();
            ghost = cell.cloneNode(true);
            // Drop transient state from the clone so it reads as a static
            // preview: kill the remove button, the hover-only chrome, and any
            // nested pointer-capturing behaviour.
            ghost.classList.add('emoji-creator-cell-ghost');
            ghost.classList.remove('is-dragging', 'is-drag-armed');
            const ghostRemove = ghost.querySelector('.emoji-creator-cell-remove');
            if (ghostRemove) ghostRemove.remove();
            ghost.style.position = 'fixed';
            ghost.style.left = `${rect.left}px`;
            ghost.style.top = `${rect.top}px`;
            ghost.style.width = `${rect.width}px`;
            ghost.style.height = `${rect.height}px`;
            ghost.style.pointerEvents = 'none';
            ghost.style.zIndex = '2200';
            document.body.appendChild(ghost);
            // Ghost is scaled (transform: scale 0.6) around its center, so
            // centering the unscaled box on the cursor keeps the visible
            // thumbnail anchored under the pointer regardless of where the
            // user grabbed the cell.
            ghostOffsetX = rect.width / 2;
            ghostOffsetY = rect.height / 2;
        },
        onDragMove: (mv) => {
            if (ghost) {
                ghost.style.left = `${mv.clientX - ghostOffsetX}px`;
                ghost.style.top  = `${mv.clientY - ghostOffsetY}px`;
            }
            _pcUpdateDropTarget(mv.clientX, mv.clientY);
        },
        onDragEnd: (up) => {
            cell.dataset.suppressClick = '1';
            cell.classList.remove('is-dragging');
            _pcDragActive = false;
            if (ghost) { ghost.remove(); ghost = null; }
            const target = _pcResolveDropTarget(up.clientX, up.clientY);
            _pcClearDropMarkers();
            if (!target) return;
            const { targetIdx, isBefore } = target;
            let to = targetIdx + (isBefore ? 0 : 1);
            if (idx === to || idx === to - 1) return;
            const [moved] = _pc.emojis.splice(idx, 1);
            if (idx < to) to--;
            if (to < 0) to = 0;
            if (to > _pc.emojis.length) to = _pc.emojis.length;
            _pc.emojis.splice(to, 0, moved);
            _pc.dirty = true;
            _pcRenderGrid();
        },
    });
}

function _pcResolveDropTarget(x, y) {
    const grid = _pickerEls.creatorGrid;
    if (!grid) return null;
    // Scope to cells that carry an index — skips the "+" add-cell, which
    // sits in the grid but has no data-idx.
    const cells = grid.querySelectorAll('.emoji-creator-cell[data-idx]');
    if (cells.length === 0) return null;

    // Confine to the grid bounds so drops on the dropzone / footer
    // don't snap-attach to a phantom slot.
    const gridRect = grid.getBoundingClientRect();
    if (x < gridRect.left || x > gridRect.right) return null;
    if (y < gridRect.top || y > gridRect.bottom) return null;

    // Pick the cell whose centre is closest to the pointer. Covers
    // direct hits, the 4px inter-cell gutters, and inter-row gaps with
    // one rule. isBefore/after is determined by the pointer's x vs the
    // chosen cell's horizontal midpoint.
    let bestCell = null;
    let bestDist = Infinity;
    for (const c of cells) {
        const r = c.getBoundingClientRect();
        const cx = r.left + r.width / 2;
        const cy = r.top + r.height / 2;
        const dx = x - cx;
        const dy = y - cy;
        const d = dx * dx + dy * dy;
        if (d < bestDist) { bestDist = d; bestCell = c; }
    }
    if (!bestCell) return null;
    const r = bestCell.getBoundingClientRect();
    const targetIdx = parseInt(bestCell.dataset.idx, 10);
    if (Number.isNaN(targetIdx)) return null;
    return { targetIdx, isBefore: x < r.left + r.width / 2 };
}

function _pcUpdateDropTarget(x, y) {
    _pcClearDropMarkers();
    const t = _pcResolveDropTarget(x, y);
    if (!t) return;
    const cell = _pickerEls.creatorGrid?.querySelector(`.emoji-creator-cell[data-idx="${t.targetIdx}"]`);
    if (!cell) return;
    cell.classList.add(t.isBefore ? 'drop-before' : 'drop-after');
}

function _pcRemoveEmoji(idx) {
    const e = _pc.emojis[idx];
    if (e && e.blobUrl) {
        try { URL.revokeObjectURL(e.blobUrl); } catch (_err) {}
    }
    // If the emoji was already on Blossom (came from a previously
    // published pack), queue its URL for cleanup at next save so the
    // file doesn't linger after the pack republishes without it.
    if (e && e.url && !e.file) {
        _pc.pendingBlobDeletes.push(e.url);
    }
    _pc.emojis.splice(idx, 1);
    _pc.dirty = true;
    _pcRenderGrid();
}

async function _pcRenameEmoji(idx) {
    const e = _pc.emojis[idx];
    if (!e) return;
    _pcNamingOwnIdx = idx;
    const next = await _pcShowNaming(
        { src: e.blobUrl || e.url, initial: e.shortcode },
        'edit',
    );
    _pcNamingOwnIdx = -1;
    if (next == null || next === e.shortcode) return;
    _pc.emojis[idx].shortcode = next;
    _pc.dirty = true;
    _pcRenderGrid();
}

// Accept anything tagged image/* OR with a known extension — browser
// MIME detection is unreliable for renamed files; the backend's magic-
// bytes check is the final word.
function _pcIsSupportedImage(file) {
    if (file.type && file.type.startsWith('image/')) return true;
    const name = (file.name || '').toLowerCase();
    return /\.(png|jpe?g|gif|webp)$/.test(name);
}

/** Load the file as an Image() to read its natural dimensions. Resolves
 *  with `{ ok: true, width, height }` for valid images, `{ ok: false }`
 *  when the file can't be decoded (treat as a format-reject). The blob
 *  URL is scoped to this check — revoked before resolve, so it doesn't
 *  leak into the editor's render path. */
function _pcReadImageDims(file) {
    return new Promise((resolve) => {
        const url = URL.createObjectURL(file);
        const img = new Image();
        img.onload = () => {
            const out = { ok: true, width: img.naturalWidth, height: img.naturalHeight };
            URL.revokeObjectURL(url);
            resolve(out);
        };
        img.onerror = () => {
            URL.revokeObjectURL(url);
            resolve({ ok: false });
        };
        img.src = url;
    });
}

function _pcShowSquareError() {
    _pcShowError(
        'Emojis Must Be Square!',
        'Please crop your emoji to equal width and height before uploading.',
        { title: 'Square Images Only.', buttonText: 'GOT IT' },
    );
}

// Sniff the web mime of imported bytes (magic bytes) so the File carries a type
// the cropper preview + emoji_crop backend can route on.
function _pcSniffImageMime(bytes) {
    if (bytes.length > 3 && bytes[0] === 0x89 && bytes[1] === 0x50) return 'image/png';
    if (bytes.length > 2 && bytes[0] === 0xFF && bytes[1] === 0xD8) return 'image/jpeg';
    if (bytes.length > 3 && bytes[0] === 0x47 && bytes[1] === 0x49) return 'image/gif';
    if (bytes.length > 11 && bytes[0] === 0x52 && bytes[8] === 0x57) return 'image/webp';
    return 'image/png';
}

// Decode a base64 command result to bytes. Image commands return base64 (a JSON
// string) because Tauri ships a Vec<u8>/ipc::Response as a raw response, which
// Android's WebView drops on the way back (only inbound raw works there).
function _b64ToBytes(b64) {
    const bin = atob(b64 || '');
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
}

// Encode bytes to base64 (chunked to stay under fromCharCode's arg limit) for
// commands whose byte input goes over JSON, since Android's WebView won't
// reliably deliver a raw request body for them.
function _bytesToB64(bytes) {
    let bin = '';
    const chunk = 0x8000;
    for (let i = 0; i < bytes.length; i += chunk) {
        bin += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
    }
    return btoa(bin);
}

// Native image picker -> Rust import (reads via ContentResolver/file I/O and
// normalizes any decodable format, e.g. TIFF, to a web-safe one) -> File(s).
// Replaces the WebView <input type=file>, whose content-URI reads are unreliable
// on Android and which can't hand back undisplayable formats.
async function _pcPickImage(multiple) {
    const { open } = window.__TAURI__.dialog;
    let picked;
    try {
        picked = await open({
            multiple,
            directory: false,
            filters: [{ name: 'Image', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'tiff', 'tif', 'bmp', 'ico'] }],
        });
    } catch (_e) { picked = null; }
    if (!picked) return multiple ? [] : null;
    const sources = Array.isArray(picked) ? picked : [picked];
    const out = [];
    let failed = 0;
    for (const source of sources) {
        try {
            const bytes = _b64ToBytes(await invoke('import_image_for_emoji', { source }));
            const mime = _pcSniffImageMime(bytes);
            const ext = mime.split('/')[1].replace('jpeg', 'jpg');
            const base = (String(source).split(/[\\/]/).pop() || 'image').replace(/\.[^.]*$/, '') || 'image';
            out.push(new File([bytes], `${base}.${ext}`, { type: mime }));
        } catch (e) {
            console.warn('[emoji-creator] image import failed:', e);
            failed++;
        }
    }
    // One summary rather than a modal per failed pick.
    if (failed) {
        _pcShowError('Oops! Couldn’t Read That!',
            (failed === 1 ? 'That image couldn’t be imported.' : `${failed} images couldn’t be imported.`)
                + ' Try PNG, JPG, GIF, or WebP.',
            { title: 'Unsupported Image.', buttonText: 'GOT IT' });
    }
    return multiple ? out : (out[0] || null);
}

async function _pcAddFiles(fileList) {
    if (!fileList || !fileList.length) return;
    let rejectedFormat = false;
    let rejectedSize = false;
    let rejectedSquare = false;
    let rejectedCropSize = false;
    const justAdded = [];
    for (const file of fileList) {
        if (_pc.emojis.length >= PC_MAX_EMOJIS) break;
        if (!_pcIsSupportedImage(file)) { rejectedFormat = true; continue; }
        if (file.size > PC_MAX_FILE_BYTES) { rejectedSize = true; continue; }
        // Square-only — Vector renders foreign clients' non-square
        // emojis stretched-to-fit (object-fit: fill), but we won't
        // author distorted emojis ourselves. The dim check decodes a
        // copy via Image() so animated formats (GIF / WebP) report the
        // canvas dimensions, not frame-by-frame.
        const dims = await _pcReadImageDims(file);
        if (!dims.ok) { rejectedFormat = true; continue; }
        let workingFile = file;
        if (dims.width !== dims.height) {
            // Static formats get the in-panel cropper. Animated formats
            // (GIF / animated WebP) keep the square-or-reject path
            // because the backend doesn't yet round-trip their timing.
            if (!_pcIsCroppableImage(file)) { rejectedSquare = true; continue; }
            const cropped = await _pcShowCropper(file);
            if (!cropped) continue; // user cancelled — silent skip
            // Squaring re-encodes every frame and can inflate a file that was
            // under the cap past it. Re-check the crop OUTPUT (not the original)
            // so it can't sail in and fail the whole batch at upload.
            if (cropped.size > PC_MAX_FILE_BYTES) { rejectedCropSize = true; continue; }
            workingFile = cropped;
        }
        const entry = {
            shortcode: _pcShortcodeFromFilename(workingFile.name),
            file: workingFile,
            blobUrl: URL.createObjectURL(workingFile),
        };
        _pc.emojis.push(entry);
        justAdded.push(entry);
    }
    if (justAdded.length > 0) _pc.dirty = true;
    _pcRenderGrid();
    // Reject precedence (most actionable first): size → crop-size → square →
    // format. Only one overlay surfaces per batch.
    if (rejectedSize) _pcShowSizeError();
    else if (rejectedCropSize) _pcShowCropSizeError();
    else if (rejectedSquare) _pcShowSquareError();
    else if (rejectedFormat) _pcShowFormatError();
    if (justAdded.length > 0) _pcQueueNamingForEntries(justAdded);
}

/** Walk freshly-added emoji entries, popping the naming overlay for each.
 *  Tracks by entry reference (not index) so removal/reorder mid-queue
 *  doesn't shift the target out from under us. Skipping keeps the
 *  auto-generated shortcode the file landed with. */
async function _pcQueueNamingForEntries(entries) {
    for (let i = 0; i < entries.length; i++) {
        if (!_pc.open) return; // panel closed mid-queue
        const entry = entries[i];
        const idx = _pc.emojis.indexOf(entry);
        if (idx < 0) continue; // user already removed it via the × badge
        _pcNamingOwnIdx = idx;
        const next = await _pcShowNaming({
            src: entry.blobUrl || entry.url,
            initial: entry.shortcode,
            batch: { current: i + 1, total: entries.length },
        }, 'create');
        _pcNamingOwnIdx = -1;
        if (next != null && next !== entry.shortcode) {
            entry.shortcode = next;
            _pc.dirty = true;
            _pcRenderGrid();
        }
    }
}

// Native-drop bridge: Tauri's window-level `onDragDropEvent` hands us
// absolute filesystem paths, never DOM File objects, so the panel's own
// `drop` listeners never fire. `convertFileSrc` exposes each path as a
// fetchable URL — slurp the bytes into a Blob, wrap it as a File so the
// existing validator + uploader code paths stay unchanged.
async function _pcAddPaths(paths) {
    if (!Array.isArray(paths) || paths.length === 0) return;
    const { convertFileSrc } = window.__TAURI__.core;
    const files = [];
    let failed = 0;
    for (const p of paths) {
        try {
            const res = await fetch(convertFileSrc(p));
            if (!res.ok) { failed++; continue; }
            const blob = await res.blob();
            const name = p.split(/[\\/]/).pop() || 'emoji';
            // Preserve the blob's real type — defaulting to image/png would
            // mask a non-image (e.g. .toml) and bypass the format filter.
            files.push(new File([blob], name, { type: blob.type || '' }));
        } catch (e) {
            failed++;
            console.warn('[emoji-pack-creator] failed to read', p, e);
        }
    }
    // Total failure → user gets the same overlay treatment as a bad
    // format/size, so the drop never looks silently ignored.
    if (files.length === 0 && failed > 0) {
        _pcShowError('Oops! Couldn’t Read That!',
            failed === 1 ? 'The file couldn’t be opened.' :
                           `${failed} files couldn’t be opened.`);
        return;
    }
    _pcAddFiles(files);
}

function isEmojiPackCreatorOpen() {
    return _pc.open === true;
}

async function _pcSetLogoFile(file) {
    if (!file) return;
    if (!_pcIsSupportedImage(file)) { _pcShowFormatError(); return; }
    if (file.size > PC_MAX_FILE_BYTES) { _pcShowSizeError(); return; }
    // Logos must be square too — they render in the same stretch-to-fit
    // way as emojis everywhere downstream.
    const dims = await _pcReadImageDims(file);
    if (!dims.ok) { _pcShowFormatError(); return; }
    let workingFile = file;
    if (dims.width !== dims.height) {
        if (!_pcIsCroppableImage(file)) { _pcShowSquareError(); return; }
        const cropped = await _pcShowCropper(file);
        if (!cropped) return; // user cancelled
        if (cropped.size > PC_MAX_FILE_BYTES) { _pcShowCropSizeError(); return; }
        workingFile = cropped;
    }
    if (_pc.logoBlobUrl) {
        try { URL.revokeObjectURL(_pc.logoBlobUrl); } catch (_e) {}
    }
    // If a remote logo was already published, queue its URL for Blossom
    // cleanup at next save — otherwise the replaced logo would orphan.
    if (_pc.logoUrl) {
        _pc.pendingBlobDeletes.push(_pc.logoUrl);
    }
    _pc.logoFile = workingFile;
    _pc.logoBlobUrl = URL.createObjectURL(workingFile);
    _pc.logoUrl = '';
    _pc.dirty = true;
    _pcRenderLogo();
}

// In-panel naming overlay — Promise-based so it composes with both the
// upload queue (await per-emoji) and cell-click editing. Resolves to the
// new shortcode on Save, `null` on Skip/Cancel/Esc.
let _pcNamingResolver = null;

function _pcShowNaming({ src, initial, batch }, mode = 'create') {
    return new Promise((resolve) => {
        // Late-arriving prompt while one is already open — coalesce so
        // the caller doesn't deadlock waiting on a never-resolved promise.
        // `null` is this primitive's "no decision" sentinel (skip /
        // cancel / esc all resolve to null); the user didn't actively
        // press Cancel, the prompt was superseded.
        if (_pcNamingResolver) _pcNamingResolver(null);
        _pcNamingResolver = resolve;

        VectorSvelte.setPickerNaming({ src, value: initial || '', mode, batch: batch || null, error: '' });
    });
}

// The emoji being renamed keeps its own shortcode without tripping the collision check.
let _pcNamingOwnIdx = -1;
function _pcNamingTryCommit(raw) {
    const sc = _pcSanitizeShortcode(raw);
    if (!sc) {
        VectorSvelte.setPickerNamingError('Letters, numbers, and underscores only.');
        return;
    }
    // Reject collisions with other emojis in this pack — saving an
    // identical shortcode would silently drop one of them at publish
    // (_pcSave dedups).
    const conflict = _pc.emojis.some((e, i) =>
        i !== _pcNamingOwnIdx && _pcSanitizeShortcode(e.shortcode) === sc);
    if (conflict) {
        VectorSvelte.setPickerNamingError(`:${sc}: is already used in this pack.`);
        // Also surface the prominent panel overlay — the inline hint is
        // easy to miss, and a duplicate name silently drops one emoji at
        // publish. Duplicate names across DIFFERENT packs are fine (they
        // disambiguate as `~1`/`~2`); only same-pack collisions are blocked.
        _pcShowError(
            'Oops! Name Already Taken!',
            `:${sc}: is already used by another emoji in this pack. Pick a different name.`,
            { title: 'Duplicate Shortcode.', buttonText: 'GOT IT' },
        );
        return;
    }
    _pcNamingFinish(sc);
}

function _pcNamingFinish(value) {
    VectorSvelte.setPickerNaming(null);
    const r = _pcNamingResolver;
    _pcNamingResolver = null;
    if (r) r(value);
}

/** Display the in-panel error overlay. `opts.title` overrides the
 *  default "Please Try Again." headline (some errors aren't retryable
 *  — e.g. equipped-pack cap), and `opts.buttonText` overrides the
 *  default "TRY AGAIN" CTA so it can read "GOT IT" for accept-only
 *  states. Also forces the picker visible — callers fire this from
 *  the modal / chat preview where the picker might be closed, and a
 *  hidden picker means a hidden overlay. */
function _pcShowError(pretitle, detail, opts = {}) {
    // Ensure the picker is visible so the in-panel overlay actually
    // surfaces. Without this, an error triggered from the deep-link
    // modal / in-chat preview would sit invisible until the user
    // opens the picker themselves.
    if (!VectorSvelte.pickerVisible()) {
        VectorSvelte.setPickerAnchor({ messageType: true }, false);
        VectorSvelte.setPickerVisible(true);
    }
    VectorSvelte.setPickerError({ pretitle, detail, title: opts.title || '', button: opts.buttonText || '' });
}
function _pcShowSizeError() {
    _pcShowError('Oops! File Size Exceeded!', 'File Size must be under 256Kb.');
}
function _pcShowCropSizeError() {
    _pcShowError(
        'Oops! Too Big After Cropping!',
        'Cropping this to a square re-encoded it over the 256 KB limit. Try an already-square version, or shrink it a little before adding.',
        { title: 'Crop Pushed It Over.', buttonText: 'GOT IT' },
    );
}
function _pcShowFormatError() {
    _pcShowError('Oops! Unsupported Format!', 'Use PNG, JPG, GIF, or WebP.');
}
function _pcShowSlotFullError() {
    _pcShowError(
        'Pack Slots Full!',
        `Vector supports up to ${MAX_EQUIPPED_PACKS} equipped packs. Remove one to add another.`,
        { title: 'Remove a Pack First.', buttonText: 'GOT IT' },
    );
}
/** Editor: a broken emoji can't be renamed. Explain the cause (deleted vs oversized) and the fix. */
function _pcBrokenEmojiError(reason) {
    if ((reason || '').toLowerCase().includes('too large')) {
        _pcShowError(
            'Oops! Emoji Too Large!',
            'This emoji is over the 1 MB size limit, so it cannot be used. Compress it under 1 MB, then delete this one and upload the smaller version.',
            { title: 'Compress & Re-upload.', buttonText: 'GOT IT' },
        );
    } else {
        _pcShowError(
            'Oops! Emoji Unavailable!',
            'This emoji has been deleted by its host. Re-uploading will fix it: delete this emoji, then upload it again.',
            { title: 'Delete It & Re-upload.', buttonText: 'GOT IT' },
        );
    }
}
function _pcHideSizeError() {
    VectorSvelte.setPickerError(null);
}

/** Whether a file is eligible for the cropper. All supported image
 *  formats route through the backend: static formats decode + re-encode
 *  in place, animated formats (GIF, animated WebP) crop every frame and
 *  preserve per-frame durations. */
function _pcIsCroppableImage(file) {
    const t = (file.type || '').toLowerCase();
    return t === 'image/png'
        || t === 'image/jpeg'
        || t === 'image/jpg'
        || t === 'image/gif'
        || t === 'image/webp';
}

// Pure display-pixel floor so the crop box's "middle" stays grabbable
// (move-drag) even when the box is small. The 14px corner dots already
// eat 7px from each edge — below ~36px the dots crowd the middle out.
// This floor is display-only; source-pixel output has no minimum.
const PC_CROP_MIN_DISP = 36;

/** In-panel square cropper. Resolves to a freshly-encoded File (same
 *  mime as input) on confirm, or null on cancel. Caller is expected to
 *  have already vetted the file is `_pcIsCroppableImage`. */
function _pcShowCropper(file) {
    return new Promise((resolve) => {
        const { stage, img, box, preview, cancel, ok } = _cropperEls;
        if (!stage || !img || !box || !preview || !cancel || !ok) { resolve(null); return; }

        const blobUrl = URL.createObjectURL(file);
        let srcW = 0, srcH = 0;
        // Display rect of the image inside the stage (stage-local coords).
        let imgRect = { left: 0, top: 0, width: 0, height: 0 };
        // Crop box in stage-local display pixels.
        let crop = { x: 0, y: 0, size: 0 };
        // Minimum crop edge in *display* pixels. Updates with imgRect.
        let minDisp = 0;

        const cleanupListeners = () => {
            stage.removeEventListener('pointerdown', onStageDown);
            stage.removeEventListener('pointermove', onPointerMove);
            stage.removeEventListener('pointerup',   onPointerUp);
            stage.removeEventListener('pointercancel', onPointerUp);
            box.removeEventListener('pointerdown', onBoxDown);
            for (const h of box.querySelectorAll('.epcc-handle')) {
                h.removeEventListener('pointerdown', onHandleDown);
            }
            cancel.removeEventListener('click', onCancel);
            ok.removeEventListener('click',     onOk);
            document.removeEventListener('keydown', onKey, true);
        };
        const finish = (result) => {
            cleanupListeners();
            URL.revokeObjectURL(blobUrl);
            VectorSvelte.setPickerCropperOpen(false);
            img.removeAttribute('src');
            // Drop the preview's background-image so CSS doesn't pin
            // the (revoked) blob URL alive in the layout tree.
            preview.style.backgroundImage = '';
            ok.disabled = false;
            resolve(result);
        };
        const onCancel = () => finish(null);
        const onOk = async () => {
            // Convert display-space crop to source-pixel crop.
            const scale = srcW / imgRect.width;
            const srcX = Math.max(0, Math.round((crop.x - imgRect.left) * scale));
            const srcY = Math.max(0, Math.round((crop.y - imgRect.top)  * scale));
            const srcSize = Math.max(1, Math.min(
                srcW - srcX,
                srcH - srcY,
                Math.round(crop.size * scale),
            ));
            ok.disabled = true;
            try {
                // Uint8Array, not a bare ArrayBuffer — Android's WebView only
                // delivers the former as a raw IPC body.
                const buf = new Uint8Array(await file.arrayBuffer());
                const out = await invoke('emoji_crop_and_reencode', {
                    sourceB64: _bytesToB64(buf),
                    mime: file.type || '',
                    x: srcX,
                    y: srcY,
                    w: srcSize,
                    h: srcSize,
                });
                const cropped = new File(
                    [_b64ToBytes(out)],
                    file.name,
                    { type: file.type },
                );
                finish(cropped);
            } catch (err) {
                console.warn('[cropper] backend rejected crop:', err);
                ok.disabled = false;
                _pcShowError('Oops! Couldn’t Crop That!',
                    typeof err === 'string' ? err : 'Please try a different image.');
                finish(null);
            }
        };

        // --- pointer interaction --------------------------------------
        let mode = null;          // 'move' | 'resize'
        let start = null;         // anchor state at pointerdown
        let activePointerId = null;
        // Stage origin in viewport coords, captured once at layout time
        // so pointer math doesn't pay a getBoundingClientRect() per
        // pointermove (forces a sync layout read).
        let stageOriginX = 0, stageOriginY = 0;
        let previewSize = 48;

        const applyBox = () => {
            box.style.left   = `${crop.x}px`;
            box.style.top    = `${crop.y}px`;
            box.style.width  = `${crop.size}px`;
            box.style.height = `${crop.size}px`;
            // Live preview: scale the full source image so the cropped
            // region fills the preview chip exactly, then offset so the
            // crop's top-left lands at (0,0) of the chip. backgroundImage
            // is set once in onload — only size/position varies here.
            if (crop.size > 0 && imgRect.width > 0) {
                const k = previewSize / crop.size;
                preview.style.backgroundSize     = `${imgRect.width  * k}px ${imgRect.height * k}px`;
                preview.style.backgroundPosition = `${(imgRect.left - crop.x) * k}px ${(imgRect.top - crop.y) * k}px`;
            }
        };
        const clampToImg = () => {
            // Size first, then position.
            const maxEdge = Math.min(imgRect.width, imgRect.height);
            crop.size = Math.max(minDisp, Math.min(maxEdge, crop.size));
            crop.x = Math.max(imgRect.left, Math.min(imgRect.left + imgRect.width  - crop.size, crop.x));
            crop.y = Math.max(imgRect.top,  Math.min(imgRect.top  + imgRect.height - crop.size, crop.y));
        };

        const onBoxDown = (e) => {
            if (mode || imgRect.width === 0) return;
            // Stop propagation so the stage's own pointerdown doesn't
            // also fire and reset mode mid-gesture. Capture lives on
            // stage so subsequent moves still route correctly.
            e.stopPropagation();
            activePointerId = e.pointerId;
            try { stage.setPointerCapture(e.pointerId); } catch {}
            mode = 'move';
            const px = e.clientX - stageOriginX;
            const py = e.clientY - stageOriginY;
            start = { px, py, cx: crop.x, cy: crop.y };
            e.preventDefault();
        };
        const onStageDown = (e) => {
            if (mode || imgRect.width === 0) return;
            // Marquee: pointerdown on the stage background or the
            // image itself (NOT box/handle/preview) starts a fresh
            // resize anchored at the pointer location. Only fire when
            // the press lands inside the image rect — drawing from the
            // letterbox empty space is confusing.
            if (e.target !== stage && e.target !== img) return;
            const px = e.clientX - stageOriginX;
            const py = e.clientY - stageOriginY;
            if (px < imgRect.left || px > imgRect.left + imgRect.width)  return;
            if (py < imgRect.top  || py > imgRect.top  + imgRect.height) return;
            activePointerId = e.pointerId;
            try { stage.setPointerCapture(e.pointerId); } catch {}
            mode = 'resize';
            start = { anchorX: px, anchorY: py };
            e.preventDefault();
        };
        const onPointerMove = (e) => {
            if (!mode || e.pointerId !== activePointerId) return;
            const px = e.clientX - stageOriginX;
            const py = e.clientY - stageOriginY;
            if (mode === 'move') {
                crop.x = start.cx + (px - start.px);
                crop.y = start.cy + (py - start.py);
            } else if (mode === 'resize') {
                // Anchor stays fixed; the dragged corner tracks the
                // pointer with a 1:1 aspect lock. Size = max(|dx|, |dy|).
                // Used by both corner-handle resizes (anchor = opposite
                // corner) and marquee draws (anchor = pointerdown point).
                const ax = start.anchorX;
                const ay = start.anchorY;
                const dx = Math.abs(px - ax);
                const dy = Math.abs(py - ay);
                let size = Math.max(dx, dy);
                // Clamp size to the image extent in the direction we're
                // growing so the box never escapes the image rect.
                const maxX = (px >= ax) ? (imgRect.left + imgRect.width  - ax) : (ax - imgRect.left);
                const maxY = (py >= ay) ? (imgRect.top  + imgRect.height - ay) : (ay - imgRect.top);
                size = Math.min(size, maxX, maxY);
                size = Math.max(minDisp, size);
                crop.size = size;
                crop.x = (px >= ax) ? ax : ax - size;
                crop.y = (py >= ay) ? ay : ay - size;
            }
            clampToImg();
            applyBox();
        };
        const onPointerUp = (e) => {
            if (e.pointerId !== activePointerId) return;
            try { stage.releasePointerCapture(e.pointerId); } catch {}
            mode = null;
            start = null;
            activePointerId = null;
        };
        const onHandleDown = (e) => {
            if (mode || imgRect.width === 0) return;
            e.stopPropagation();
            activePointerId = e.pointerId;
            try { stage.setPointerCapture(e.pointerId); } catch {}
            mode = 'resize';
            const handle = e.currentTarget.dataset.handle;
            // Anchor = opposite corner of the box, in stage coords.
            const ax = (handle === 'tl' || handle === 'bl') ? (crop.x + crop.size) : crop.x;
            const ay = (handle === 'tl' || handle === 'tr') ? (crop.y + crop.size) : crop.y;
            start = { anchorX: ax, anchorY: ay };
            e.preventDefault();
        };
        const onKey = (e) => {
            // Capture-phase + stopPropagation so the picker's global
            // Escape handler (closes the whole panel) doesn't fire on
            // top of ours.
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                finish(null);
            } else if (e.key === 'Enter' && !ok.disabled) {
                e.preventDefault();
                e.stopPropagation();
                onOk();
            }
        };

        // --- show + layout --------------------------------------------
        img.onload = () => {
            srcW = img.naturalWidth;
            srcH = img.naturalHeight;
            // Stage size is fixed by CSS — read it after the overlay
            // is visible so getBoundingClientRect returns real px.
            // Origin is cached so pointer math doesn't sync-read layout
            // every move.
            const sr = stage.getBoundingClientRect();
            const stageW = sr.width;
            const stageH = sr.height;
            stageOriginX = sr.left;
            stageOriginY = sr.top;
            previewSize  = preview.offsetWidth || 48;
            // Letterbox-fit *inside* a small inset so the corner handles
            // (positioned at -7px from the box edge) never overflow the
            // stage and get clipped by `overflow: hidden`. Handle radius
            // is 7px + a px of breathing room.
            const HANDLE_INSET = 10;
            const fitW = Math.max(1, stageW - HANDLE_INSET * 2);
            const fitH = Math.max(1, stageH - HANDLE_INSET * 2);
            const scale = Math.min(fitW / srcW, fitH / srcH);
            const dispW = Math.round(srcW * scale);
            const dispH = Math.round(srcH * scale);
            imgRect = {
                left: Math.round((stageW - dispW) / 2),
                top:  Math.round((stageH - dispH) / 2),
                width:  dispW,
                height: dispH,
            };
            img.style.left   = `${imgRect.left}px`;
            img.style.top    = `${imgRect.top}px`;
            img.style.width  = `${imgRect.width}px`;
            img.style.height = `${imgRect.height}px`;

            // Initial crop: largest centered square inside the image.
            const initEdge = Math.min(imgRect.width, imgRect.height);
            crop.size = initEdge;
            crop.x = imgRect.left + (imgRect.width  - initEdge) / 2;
            crop.y = imgRect.top  + (imgRect.height - initEdge) / 2;
            minDisp = PC_CROP_MIN_DISP;
            // Set backgroundImage once — only size/position vary per
            // pointermove inside applyBox.
            preview.style.backgroundImage = `url("${blobUrl}")`;
            clampToImg();
            applyBox();
        };

        // Wire handlers + show.
        stage.addEventListener('pointerdown', onStageDown);
        stage.addEventListener('pointermove', onPointerMove);
        stage.addEventListener('pointerup',   onPointerUp);
        stage.addEventListener('pointercancel', onPointerUp);
        box.addEventListener('pointerdown', onBoxDown);
        for (const h of box.querySelectorAll('.epcc-handle')) {
            h.addEventListener('pointerdown', onHandleDown);
        }
        cancel.addEventListener('click', onCancel);
        ok.addEventListener('click',     onOk);
        document.addEventListener('keydown', onKey, true);

        VectorSvelte.setPickerCropperOpen(true);
        img.src = blobUrl;
    });
}

/** In-panel confirm overlay. Generalised question modal in the same
 *  visual family as the error / naming / progress overlays. Returns a
 *  Promise that resolves to true (Continue) or false (Cancel / Esc).
 *  Use this instead of `popupConfirm` from inside the creator —
 *  popupConfirm lives outside .emoji-picker so any click on it triggers
 *  the picker's outside-click-close handler.
 *
 *  Options: { title, detail?, icon? (file name in /icons/), tone?
 *  ('default'|'danger'), confirmText?, cancelText? }. */
let _pcConfirmResolver = null;

function _pcShowConfirm(opts = {}) {
    return new Promise((resolve) => {
        // Coalesce a stale resolver so the caller can't deadlock. `false`
        // is this primitive's "no decision" sentinel — the user didn't
        // explicitly press Cancel, the prompt was just superseded.
        if (_pcConfirmResolver) _pcConfirmResolver(false);
        _pcConfirmResolver = resolve;

        VectorSvelte.setPickerConfirm({
            title: opts.title || '', detail: opts.detail || '', icon: opts.icon || '',
            tone: opts.tone || 'default', confirmText: opts.confirmText || '', cancelText: opts.cancelText || '',
        });
    });
}

function _pcConfirmFinish(value) {
    VectorSvelte.setPickerConfirm(null);
    const r = _pcConfirmResolver;
    _pcConfirmResolver = null;
    if (r) r(value);
}

/** Full-panel progress overlay for long ops (pack delete in particular,
 *  where one slow Blossom server can stall for ~30s and per-cell rings
 *  alone leave too much unexplained dead time). */
function _pcShowProgress(title, detail) {
    VectorSvelte.setPickerProgress({ title, detail: detail || '' });
}
function _pcSetProgressDetail(detail) {
    VectorSvelte.setPickerProgressDetail(detail);
}
function _pcHideProgress() {
    VectorSvelte.setPickerProgress(null);
}

/** Per-cell busy state painter. State: 'pending' | 'uploading' | 'deleting'
 *  | null (clear). Cells are referenced by their current data-idx in the
 *  DOM. Renders a dimmed overlay + progress ring without disturbing the
 *  underlying cell DOM, so a parallel re-render (e.g. shortcode tweak)
 *  doesn't strand the overlay. */
function _pcSetCellBusy(idx, state) {
    VectorSvelte.setCreatorBusy(idx, state);
}

function _pcClearAllBusy() {
    VectorSvelte.clearCreatorBusy();
}

// Deterministic rejections (too big / empty / wrong account / no server) won't
// improve on retry; a Blossom/network flake gets a couple quick re-attempts so
// one hiccup can't sink a whole batch.
function _pcUploadErrorIsPermanent(msg) {
    const m = String(msg || '').toLowerCase();
    return m.includes('max is') || m.includes('is empty')
        || m.includes('account swap') || m.includes('no blossom')
        || m.includes('too large') || m.includes('too detailed')
        || m.includes("couldn't be read");
}

async function _pcUploadFile(file, kind = 'emoji') {
    let lastErr;
    for (let attempt = 0; attempt < 3; attempt++) {
        try {
            // base64 JSON in: async commands don't reliably receive a raw request
            // body on Android's WebView. Fresh read per attempt (retries are rare).
            const bytes = new Uint8Array(await file.arrayBuffer());
            return await invoke('emoji_pack_upload_image', { bytesB64: _bytesToB64(bytes), kind });
        } catch (e) {
            lastErr = e;
            if (_pcUploadErrorIsPermanent(e)) break;
            await new Promise(r => setTimeout(r, 400 * (attempt + 1)));
        }
    }
    throw lastErr;
}

/** Persist current state to relays + DB. Called on exit when dirty,
 *  not on every keystroke (would publish a new kind 30030 per stroke). */
async function _pcSave() {
    if (_pc.saving) return false;
    // .slice(0, 26) catches legacy packs whose titles predate the 26-char
    // cap — the input's maxlength only constrains new typing, not values
    // we hydrated into the field from an existing pack.
    const name = (_pc.name || '').trim().slice(0, 26);
    if (!name || !_pc.emojis.length) {
        // Empty pack — drop the in-progress edit silently. Better than
        // publishing a useless empty/no-name set the user clearly bailed on.
        _pc.dirty = false;
        return true;
    }

    const seenCodes = new Set();
    const sanitized = [];
    _pc.emojis.forEach((e, originalIdx) => {
        const sc = _pcSanitizeShortcode(e.shortcode);
        if (!sc || seenCodes.has(sc)) return;
        seenCodes.add(sc);
        // Preserve the original _pc.emojis index so the upload loop can
        // paint the cell's busy state without searching by reference.
        sanitized.push({ ...e, shortcode: sc, originalIdx });
    });
    if (!sanitized.length) { _pc.dirty = false; return true; }

    // Defense-in-depth: only forward `editingIdentifier` when the matching
    // pack is still in `arrEmojiPacks` AND owned by the current user.
    // Protects against a malformed pack list (or stale state after an
    // account swap) tricking publish into overwriting a stranger's pack.
    let safeIdentifier = null;
    if (_pc.editingId && _pc.editingIdentifier) {
        const owned = arrEmojiPacks.find(p =>
            p.id === _pc.editingId && p.is_own === true);
        if (owned && owned.identifier === _pc.editingIdentifier) {
            safeIdentifier = _pc.editingIdentifier;
        }
    }

    _pc.saving = true;
    _pcSetSavingChrome(true);

    // Pre-paint queue: every entry that still needs an upload (has a
    // local File, no remote URL yet) gets a "pending" overlay so the
    // user sees the batch lined up before the first upload completes.
    for (const e of sanitized) {
        if (e.file) _pcSetCellBusy(e.originalIdx, 'pending');
    }

    try {
        let logoUrl = _pc.logoUrl || '';
        if (_pc.logoFile) {
            logoUrl = await _pcUploadFile(_pc.logoFile, 'emoji_pack_icon');
            // Commit so a retry after a partial failure skips re-uploading it.
            _pc.logoUrl = logoUrl;
            _pc.logoFile = null;
        }

        const emojis = [];
        for (const e of sanitized) {
            let url = e.url;
            if (e.file) {
                _pcSetCellBusy(e.originalIdx, 'uploading');
                url = await _pcUploadFile(e.file, 'emoji');
                _pcSetCellBusy(e.originalIdx, null);
                // Commit each URL as it lands so a later failure in the batch
                // keeps finished uploads: retry re-sends only what's left (no
                // re-upload, no orphan blobs). blobUrl stays valid for preview.
                const slot = _pc.emojis[e.originalIdx];
                if (slot) { slot.url = url; slot.file = null; }
            }
            if (!url) continue;
            emojis.push({ shortcode: e.shortcode, url });
        }

        const savedPack = await invoke('emoji_pack_create', {
            input: {
                identifier: safeIdentifier,
                title: name,
                image_url: logoUrl || null,
                description: null,
                emojis,
            },
        });

        // Pack is published — now drain any blob-cleanup queue (emojis
        // removed via × badge, or a replaced logo). Parallel since these
        // are best-effort and the cells are already gone from the UI.
        // Cleared only on success; a failed publish leaves the queue
        // intact so the next save attempt still gets to clean up.
        // Fire-and-forget: a single hung Blossom DELETE shouldn't freeze
        // the creator with `_pc.saving=true`. The queue has been moved
        // out of `_pc.pendingBlobDeletes` so re-entrance is safe. Also
        // evict each URL from the emoji cache memo on success so a stale
        // local path can't outlive its deleted Blossom file.
        if (_pc.pendingBlobDeletes.length > 0) {
            const live = _pcLiveBlobUrls(emojis.map(x => x.url).concat(logoUrl ? [logoUrl] : []));
            const queue = _pc.pendingBlobDeletes.filter(url => !live.has(url));
            _pc.pendingBlobDeletes = [];
            Promise.allSettled(queue.map(async url => {
                try {
                    await invoke('emoji_pack_delete_blob', { url });
                    _emojiCacheMemo.delete(url);
                } catch (err) {
                    console.warn('[emoji-pack-creator] orphan blob delete:', err);
                }
            })).catch(() => { /* allSettled never rejects */ });
        }

        await loadEmojiPacks();
        _pc.dirty = false;
        // Remember which pack to land on once the editor closes (the returned
        // id covers both create and edit) — see closeEmojiPackCreator.
        _pc.savedPackId = (savedPack && savedPack.id) || _pc.editingId || null;
        return true;
    } catch (e) {
        console.warn('[emoji-pack-creator] save failed:', e);
        // Keep dirty + the editor open (caller bails on false) and show why,
        // instead of failing silently and dropping the batch on close.
        _pcShowError(
            'Couldn’t Save Pack',
            `Your emojis are safe, nothing was lost. An upload didn’t finish (usually a passing network glitch). Tap Done to retry.\n\n(${String(e)})`,
            { title: 'Your Progress Is Safe.', buttonText: 'GOT IT' },
        );
        return false;
    } finally {
        // Clear any lingering busy overlays — success or failure, the
        // upload phase is over.
        _pcClearAllBusy();
        _pc.saving = false;
        _pcSetSavingChrome(false);
    }
}

function _pcSetSavingChrome(on) {
    VectorSvelte.setCreatorSaving(on);
}

async function _pcDelete() {
    if (_pc.saving) return;
    // Create mode: nothing exists yet to delete, so this is "discard the draft" —
    // and it's the only non-committal way out of the creator, since the other exit
    // is labelled "Save and exit". Draft emojis hold a File + blobUrl and are not
    // uploaded until save, so there's normally nothing server-side to sweep; a
    // `url` only appears on a cell whose upload landed during a save that later
    // failed, and those are already queued in `pendingBlobDeletes`.
    // A successful save closes the creator, so an open create-mode editor has never
    // published — which is what the discard copy promises the user. Belt-and-braces
    // in case a future change keeps it open after saving: adopt the saved id so this
    // takes the real delete path rather than claiming nothing exists.
    if (!_pc.editingId && _pc.savedPackId) _pc.editingId = _pc.savedPackId;
    if (!_pc.editingId) {
        const discard = await _pcShowConfirm({
            title: 'Discard This Pack?',
            detail: 'This pack was never saved. The emojis you added will be lost.',
            icon: 'vector_warning.svg',
            tone: 'danger',
            confirmText: 'DISCARD',
        });
        if (!discard) return;
        // Sweep anything a failed save already pushed to Blossom, so bailing out
        // can't strand orphan blobs. Best-effort and unblocking.
        const liveElsewhere = _pcLiveBlobUrls();
        const orphans = _pc.emojis.map(e => e.url).filter(Boolean)
            .concat(_pc.pendingBlobDeletes, _pc.logoUrl ? [_pc.logoUrl] : [])
            .filter(url => !liveElsewhere.has(url));
        _pc.pendingBlobDeletes = [];
        if (orphans.length) {
            Promise.allSettled(orphans.map(url =>
                invoke('emoji_pack_delete_blob', { url })
                    .then(() => _emojiCacheMemo.delete(url))
            )).catch(() => {});
        }
        _pc.dirty = false;
        _pc.open = false;
        _pc.emojis = [];
        _pc.logoFile = null;
        _pc.logoUrl = '';
        VectorSvelte.setCreatorOpen(false);
        _pcRevokeBlobUrls();
        return;
    }
    // In-panel confirm — popupConfirm lives outside .emoji-picker and
    // any click on it would trip the outside-close handler, snapping
    // the picker shut mid-flow.
    const ok = await _pcShowConfirm({
        title: 'Delete This Pack?',
        detail: 'This action permanently removes the emoji files from your media servers and the pack from Nostr. This action cannot be undone.',
        icon: 'vector_warning.svg',
        tone: 'danger',
        confirmText: 'DELETE',
    });
    if (!ok) return;
    _pc.saving = true;
    _pcSetSavingChrome(true);

    // Snapshot URLs to delete before any state mutation. We use the
    // current _pc.emojis (what's on screen) rather than arrEmojiPacks
    // because the user may have removed cells in this edit session that
    // haven't been re-published yet — those files should still die.
    // Blobs another pack shares (content-addressed dedup) must survive.
    const liveElsewhere = _pcLiveBlobUrls();
    const cellOps = _pc.emojis
        .map((e, idx) => ({ idx, url: e.url }))
        .filter(op => Boolean(op.url) && !liveElsewhere.has(op.url));
    const logoUrl = (_pc.logoUrl && !liveElsewhere.has(_pc.logoUrl)) ? _pc.logoUrl : '';
    // Any URLs queued for cleanup (× removals + replaced logo) get
    // swept here too so a "delete pack" run after an unsaved edit still
    // tears down those orphans.
    const pendingExtras = _pc.pendingBlobDeletes.filter(url => !liveElsewhere.has(url));
    _pc.pendingBlobDeletes = [];

    // Full-panel overlay survives the whole flow so the user gets a
    // continuous "Deleting…" signal even while a single slow Blossom
    // server hangs the per-cell ring for ~30s.
    const totalBlobs = cellOps.length + (logoUrl ? 1 : 0) + pendingExtras.length;
    _pcShowProgress('Deleting Pack', totalBlobs > 0
        ? `Removing ${totalBlobs} file${totalBlobs === 1 ? '' : 's'} from media servers…`
        : 'Removing from Nostr…');

    try {
        // Layer 1 — Blossom blob deletes. Sequential so each cell's
        // ring spins for the duration of its actual request, giving the
        // user a real (not faked) progress signal.
        let done = 0;
        // Evict each URL from the JS memo on a successful Blossom delete
        // so the memo can't outlive its server-side data. Local cached
        // files persist (Rust only deletes Blossom-side), so subsequent
        // renders re-IPC to `get_or_cache_image` which returns the still-
        // valid local path — minor cost for cleaner bookkeeping.
        for (const op of cellOps) {
            done++;
            _pcSetProgressDetail(`Deleting emoji ${done} of ${totalBlobs} from media servers…`);
            _pcSetCellBusy(op.idx, 'deleting');
            try {
                await invoke('emoji_pack_delete_blob', { url: op.url });
                _emojiCacheMemo.delete(op.url);
            } catch (err) { console.warn('[emoji-pack-creator] blob delete:', err); }
            _pcSetCellBusy(op.idx, null);
        }
        // Logo file (best-effort, no per-cell anchor).
        if (logoUrl) {
            done++;
            _pcSetProgressDetail(`Deleting pack icon (${done} of ${totalBlobs})…`);
            try {
                await invoke('emoji_pack_delete_blob', { url: logoUrl });
                _emojiCacheMemo.delete(logoUrl);
            } catch (err) { console.warn('[emoji-pack-creator] logo delete:', err); }
        }
        // Drain any orphan URLs queued before the user pressed Delete.
        if (pendingExtras.length > 0) {
            _pcSetProgressDetail(`Cleaning up ${pendingExtras.length} orphan file${pendingExtras.length === 1 ? '' : 's'}…`);
            const results = await Promise.allSettled(pendingExtras.map(url =>
                invoke('emoji_pack_delete_blob', { url })));
            results.forEach((r, i) => {
                if (r.status === 'fulfilled') _emojiCacheMemo.delete(pendingExtras[i]);
            });
        }
        // Layer 2 — Nostr tombstone + local DB cleanup + 10030 republish.
        _pcSetProgressDetail('Removing pack from Nostr…');
        await invoke('emoji_pack_delete', { id: _pc.editingId });
        _pcSetProgressDetail('Refreshing your packs…');
        await loadEmojiPacks();
        _pc.dirty = false;
        _pc.open = false;
        _pcHideProgress();
        VectorSvelte.setCreatorOpen(false);
        _pcRevokeBlobUrls();
    } catch (e) {
        console.warn('[emoji-pack-creator] delete failed:', e);
        _pcHideProgress();
    } finally {
        _pcClearAllBusy();
        _pc.saving = false;
        _pcSetSavingChrome(false);
    }
}

// Enter / Escape route through the active confirm resolver, and only while one is up.
document.addEventListener('keydown', (e) => {
    if (!_pcConfirmResolver) return;
    if (e.key === 'Enter') { e.preventDefault(); _pcConfirmFinish(true); }
    else if (e.key === 'Escape') { e.preventDefault(); _pcConfirmFinish(false); }
});
