/**
 * File Preview Overlay
 * Shows a preview of files before sending with options like compression for images.
 * The overlay itself is a Svelte island (components/composer/FilePreview.svelte) over
 * VectorSvelte's file preview state; this module owns the SOURCE (a path, cached
 * bytes, a File object, a zip in progress) and the send.
 */

// Reveal the Keep Metadata toggle only when the image carries strip-worthy EXIF
// (screenshots/memes have none). filePath empty => check the JS-cached bytes.
async function revealMetadataOptionIfPresent(filePath) {
    try {
        const has = await invoke('file_has_metadata', { filePath: filePath || '' });
        if (has) VectorSvelte.fpPatch({ metadata: true });
    } catch (_) { /* leave hidden on error */ }
}

let pendingFile = null;
let pendingFileBytes = null; // For Android: flag indicating bytes mode
let pendingFileName = null;  // For Android: stores file name
let pendingFileExt = null;   // For Android: stores file extension
let pendingReceiver = null;
let pendingReplyRef = null;
let compressionInProgress = false;
let compressionComplete = false;
let compressionPollingInterval = null;
let pendingMiniAppInfo = null; // For marketplace publishing: stores Mini App info
let pendingZipPath = null; // For folder zip: temp zip path for cleanup
let zipInProgress = false; // For folder zip: compression in progress
let pendingZipUnlisten = null; // For folder zip: unlisten function for zip_progress events
let pendingBlobUrl = null; // A video preview's object URL, revoked on close
let filePreviewGeneration = 0; // Guards against async results landing on a newer preview

/**
 * FilePreviewHelpers: the send-file preview overlay.
 * @typedef {Object} FilePreviewHelpers
 * @property {() => void} close
 * @property {() => void} send
 * @property {() => void} publish
 * @property {(path: string) => Promise<string|null>} readImagePreview
 * @property {(path: string) => Promise<string|null>} thumbhash
 * @property {(files: object[], total: number) => string} buildFileListHtml
 * @property {() => void} initFileTreeToggles
 * @property {(stem: string) => string} sanitizeStem
 */
VectorSvelte.setScreen('filePreview', {
        h: {
            close: () => closeFilePreview(),
            send: () => sendPreviewedFile(),
            publish: () => publishPendingMiniApp(),
            readImagePreview: (path) => invoke('read_image_preview', { path }).then(convertFileSrc),
            thumbhash: (path) => invoke('generate_thumbhash_for_preview', { filePath: path || '' }),
            buildFileListHtml: (files, total) => buildFileListHtml(files, total),
            initFileTreeToggles: () => initFileTreeToggles(),
            sanitizeStem: (str) => sanitizeFilenameStem(str),
        },
});

// Image extensions supported by the image crate
const SUPPORTED_IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'tiff', 'tif', 'ico'];

// Video extensions supported for preview (mp4, webm, mov - except on Linux)
const SUPPORTED_VIDEO_EXTENSIONS = ['mp4', 'webm', 'mov'];

/**
 * Validate that a string is a safe image source (data URL or blob URL)
 * Prevents XSS via malicious src injection (e.g., javascript: protocol)
 * @param {string} src - The source string to validate
 * @returns {string|null} The validated src or null if invalid
 */
function validateImageSrc(src) {
    if (!src || typeof src !== 'string') return null;
    // Inline images, blobs, and the app's own asset route — never a remote origin.
    if (src.startsWith('data:image/') || src.startsWith('blob:') || src.startsWith('asset://')
        || src.startsWith('http://asset.localhost/') || src.startsWith('https://asset.localhost/')) {
        return src;
    }
    console.warn('[file-preview] Rejected invalid image src:', src.substring(0, 50));
    return null;
}

/**
 * Detect a SPOILER_ prefix: returns the clean name and whether the preview opens spoilered.
 * @param {string} name - The filename (with or without extension)
 * @returns {{ name: string, spoiler: boolean }}
 */
function detectAndStripSpoilerPrefix(name) {
    const stem = getFileStem(name);
    if (stem.toUpperCase().startsWith('SPOILER_')) {
        const cleanStem = stem.substring(8); // strip "SPOILER_"
        const ext = getFileExtension(name);
        return { name: ext ? `${cleanStem}.${ext}` : cleanStem, spoiler: true };
    }
    return { name, spoiler: false };
}

/**
 * Strip dangerous characters from a filename stem.
 * Permissive: allows spaces, accents, parentheses, etc. — only blocks
 * path separators, null bytes, and chars that break common filesystems.
 */
function sanitizeFilenameStem(str) {
    // eslint-disable-next-line no-control-regex
    return str.replace(/[\/\\:\*\?"<>\|\x00]/g, '');
}

/**
 * Get file extension from path
 * @param {string} filepath - File path
 * @returns {string} File extension (lowercase)
 */
function getFileExtension(filepath) {
    const parts = filepath.split('.');
    return parts.length > 1 ? parts.pop().toLowerCase() : '';
}

/**
 * Get file stem (name without extension)
 * @param {string} name - File name
 * @returns {string} File stem
 */
function getFileStem(name) {
    const ext = getFileExtension(name);
    if (!ext) return name;
    return name.substring(0, name.length - ext.length - 1);
}

/**
 * Get file name from path
 * @param {string} filepath - File path
 * @returns {string} File name
 */
function getFileName(filepath) {
    // Handle both Windows and Unix paths
    const parts = filepath.replace(/\\/g, '/').split('/');
    let name = parts.pop() || filepath;
    // URL decode in case it's encoded (common with Android content URIs)
    try {
        name = decodeURIComponent(name);
    } catch (e) {
        // Ignore decode errors
    }
    return name;
}

/**
 * Check if file is a supported image
 * @param {string} filepath - File path
 * @returns {boolean} True if file is a supported image
 */
function isSupportedImage(filepath) {
    const ext = getFileExtension(filepath);
    return SUPPORTED_IMAGE_EXTENSIONS.includes(ext);
}

/**
 * Check if file is a supported video (for preview)
 * @param {string} filepath - File path
 * @returns {boolean} True if file is a supported video
 */
function isSupportedVideo(filepath) {
    // Video preview not supported on Linux
    if (platformFeatures?.os === 'linux') {
        return false;
    }
    const ext = getFileExtension(filepath);
    return SUPPORTED_VIDEO_EXTENSIONS.includes(ext);
}

/**
 * Get appropriate icon for file type
 * @param {string} filepath - File path
 * @returns {string} Icon class name
 */
function getFileIcon(filepath) {
    const ext = getFileExtension(filepath);
    
    if (SUPPORTED_IMAGE_EXTENSIONS.includes(ext)) {
        return 'icon-image';
    }
    
    // Video extensions
    if (['mp4', 'webm', 'mov', 'avi', 'mkv'].includes(ext)) {
        return 'icon-film';
    }
    
    // Audio extensions
    if (['mp3', 'wav', 'ogg', 'flac', 'm4a', 'aac'].includes(ext)) {
        return 'icon-volume-max';
    }

    // Archive extensions
    if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz', 'zst', 'tgz', 'tbz2'].includes(ext)) {
        return 'icon-folder';
    }

    return 'icon-file';
}

/**
 * Check if file is a Mini App (.xdc file)
 * @param {string} ext - File extension
 * @returns {boolean}
 */
function isMiniAppExtension(ext) {
    return ext === 'xdc';
}

/** The Mini App preview's content model: its icon when the manifest carries one. */
function miniAppPreviewContent(miniAppInfo) {
    const validatedIcon = miniAppInfo ? validateImageSrc(miniAppInfo.icon_data) : null;
    return { kind: 'miniapp', icon: validatedIcon, name: miniAppInfo?.name || 'Mini App' };
}

/** Hand the pending mini app to the Nexus publish flow (the dialog itself is marketplace.js'). */
async function publishPendingMiniApp() {
    if (!pendingFile || !pendingMiniAppInfo) {
        return console.error('No pending file or Mini App info for publishing');
    }

    // Capture before closeFilePreview() clears them
    const filePath = pendingFile;
    const miniAppInfo = pendingMiniAppInfo;

    // Close the file preview first
    closeFilePreview();

    // Open the publish dialog
    await showPublishAppDialog(filePath, miniAppInfo);
}

/**
 * Pre-flight: returns true (and shows a popup) only when every enabled
 * server would refuse this file — by its own account where it publishes
 * one, else from what earlier uploads taught us. Unknown servers count as
 * likely-accept, so the check is optimistic.
 */
async function checkUploadBlocked(fileSize, extension) {
    try {
        const verdict = await invoke('blossom_upload_verdict', {
            extension: extension || 'bin',
            sizeBytes: fileSize,
            isEncrypted: true,
        });
        if (!verdict.likely) {
            const reasons = (verdict.reasons || []).map(r => `<li>${escapeHtml(r)}</li>`).join('');
            popupConfirm(
                'No media server will take this file',
                `This file is <b>${formatBytes(fileSize, 1)}</b>.`
                    + (reasons ? `<ul style="text-align: left; margin: 10px 0 0; padding-left: 18px;">${reasons}</ul>` : '')
                    + '<br><span style="opacity: 0.5; font-size: 12px;">Shrink the file, or add a server in Settings → Network.</span>',
                true, '', 'vector_warning.svg',
            );
            return true;
        }
        return false;
    } catch (err) {
        // Fall open — the real upload's failover surfaces any real failure.
        console.warn('[Blossom] pre-flight check failed:', err);
        return false;
    }
}

async function openFilePreview(filepath, receiver, replyRef = '') {
    const myGeneration = ++filePreviewGeneration;
    releasePendingVideo();

    pendingFile = filepath;
    pendingFileBytes = null; // Clear bytes mode since we're using file path
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    pendingFileExt = null; // Will be set after extension is resolved
    pendingZipPath = null;
    zipInProgress = false;

    const isAndroid = platformFeatures?.os === 'android';

    // Get file info from backend
    // On Android, use cache_android_file which reads and caches the file bytes immediately
    // This is critical because Android content URI permissions expire quickly
    let fileSize = 0;
    let fileName = getFileName(filepath);
    let ext = getFileExtension(filepath);
    let androidPreview = null; // Android: the backend's preview file, as an asset URL

    try {
        // On Android, cache the file bytes immediately while we still have permission
        // On other platforms, this just returns file info without caching
        const fileInfo = await invoke('cache_android_file', { filePath: filepath });
        fileSize = fileInfo.size;

        if (await checkUploadBlocked(fileSize, ext)) return;
        // On Android, use the backend's filename and extension since URI doesn't contain them
        if (isAndroid && fileInfo.name) fileName = fileInfo.name;
        if (isAndroid && fileInfo.extension) ext = fileInfo.extension;
    } catch (e) {
        console.error('Failed to get/cache file info:', e);
    }
    if (filePreviewGeneration !== myGeneration) return;
    if (isAndroid && SUPPORTED_IMAGE_EXTENSIONS.includes(ext)) {
        // The pick is cached under its URI; the downscaled preview file is a second call.
        const previewPath = await invoke('preview_cached_file', { filePath: filepath }).catch(() => null);
        if (filePreviewGeneration !== myGeneration) return;
        if (previewPath) androidPreview = convertFileSrc(previewPath);
    }

    // Determine file type using the resolved extension
    const isImage = SUPPORTED_IMAGE_EXTENSIONS.includes(ext);
    const isVideo = SUPPORTED_VIDEO_EXTENSIONS.includes(ext);
    const isMiniApp = isMiniAppExtension(ext);

    // For Mini Apps, try to load the app info to get name and icon
    let miniAppInfo = null;
    if (isMiniApp) {
        try {
            miniAppInfo = await loadMiniAppInfo(filepath);
        } catch (e) {
            console.error('Failed to load Mini App info:', e);
        }
        if (filePreviewGeneration !== myGeneration) return;
    }

    // Detect SPOILER_ prefix and strip from display name
    const detected = detectAndStripSpoilerPrefix(fileName);
    fileName = detected.name;
    pendingFileExt = ext;
    pendingMiniAppInfo = miniAppInfo;

    const displayName = (isMiniApp && miniAppInfo && miniAppInfo.name) ? miniAppInfo.name : fileName;

    let content;
    if (isMiniApp) {
        content = miniAppPreviewContent(miniAppInfo);
    } else if (isImage) {
        const validatedAndroidPreview = validateImageSrc(androidPreview);
        if (isAndroid && validatedAndroidPreview) {
            content = { kind: 'image', src: validatedAndroidPreview, path: filepath };
        } else if (isAndroid) {
            // Fallback: On Android without preview, show icon
            content = { kind: 'icon', icon: 'icon-image' };
        } else {
            // The asset protocol serves only its scoped dirs; the island falls back to
            // an inline backend read when the img errors.
            content = { kind: 'image', src: convertFileSrc(filepath), path: filepath };
        }
    } else if (isVideo) {
        // Video preview is unreliable on Android; show a generic film icon.
        if (isAndroid) {
            content = { kind: 'icon', icon: 'icon-film' };
        } else {
            // A picked video can live outside the asset scope; admit this one file first.
            const allowed = await invoke('allow_video_preview', { path: filepath }).then(() => true, () => false);
            if (filePreviewGeneration !== myGeneration) return;
            content = allowed ? { kind: 'video', src: mediaUrl(filepath) } : { kind: 'icon', icon: 'icon-film' };
        }
    } else {
        content = { kind: 'icon', icon: getFileIcon(filepath) };
    }

    // Reset compression state
    compressionInProgress = false;
    compressionComplete = false;
    stopCompressionPolling();

    // Keep Metadata shows for any non-GIF image; Compress only above 25KB.
    // Mini Apps don't get either option.
    const MIN_COMPRESS_SIZE = 25 * 1024; // 25KB
    const isGif = ext === 'gif';
    const offerOptions = isImage && !isGif && !isMiniApp;
    const showCompress = offerOptions && fileSize > MIN_COMPRESS_SIZE;

    VectorSvelte.fpOpen({
        stem: getFileStem(displayName) || displayName,
        ext,
        size: formatBytes(fileSize),
        spoiler: content.kind === 'image' && detected.spoiler,
        compress: showCompress,
    });
    VectorSvelte.fpContent(content);

    if (offerOptions) {
        // Start pre-compression in background (only when compression is offered)
        if (showCompress) startPrecompression(filepath);
        revealMetadataOptionIfPresent(filepath);
    }
    // Show/hide publish button for trusted publishers with Mini Apps
    if (isMiniApp) checkAndShowPublishButton();
}

/** Check if current user is trusted publisher and show publish button */
async function checkAndShowPublishButton() {
    try {
        // isCurrentUserTrustedPublisher is provided by marketplace.js
        const isTrusted = await isCurrentUserTrustedPublisher();
        VectorSvelte.fpPatch({ publish: !!isTrusted });
    } catch (e) {
        console.error('Failed to check trusted publisher status:', e);
        VectorSvelte.fpPatch({ publish: false });
    }
}

function stopCompressionPolling() {
    if (compressionPollingInterval) {
        clearInterval(compressionPollingInterval);
        compressionPollingInterval = null;
    }
}

/** A video preview's object URL is released once it is off screen. */
function releasePendingVideo() {
    if (pendingBlobUrl) {
        URL.revokeObjectURL(pendingBlobUrl);
        pendingBlobUrl = null;
    }
}

function compressionInfoText(status) {
    return status.savings_percent > 0
        ? `~${formatBytes(status.estimated_size)} (${status.savings_percent}% smaller)`
        : 'No significant savings';
}

/**
 * Start pre-compression and poll for status
 * @param {string} filepath - Path to the image file
 */
async function startPrecompression(filepath) {
    try {
        // Start the pre-compression
        compressionInProgress = true;
        compressionComplete = false;
        await invoke('start_image_precompression', { filePath: filepath });

        // Poll for completion
        compressionPollingInterval = setInterval(async () => {
            try {
                const status = await invoke('get_compression_status', { filePath: filepath });
                if (status !== null) {
                    // Compression complete
                    compressionInProgress = false;
                    compressionComplete = true;
                    stopCompressionPolling();
                    VectorSvelte.fpPatch({ compressInfo: compressionInfoText(status) });
                }
            } catch (e) {
                // File might have been cancelled
                stopCompressionPolling();
            }
        }, 200);
    } catch (e) {
        console.error('Failed to start compression:', e);
        VectorSvelte.fpPatch({ compressInfo: 'Compression failed' });
        compressionInProgress = false;
    }
}

// UTF-8-safe base64 for header values that may carry non-ASCII (e.g. filenames),
// which raw HTTP header values can't hold.
function _b64utf8(str) {
    return btoa(String.fromCharCode.apply(null, new TextEncoder().encode(str || '')));
}

/**
 * Open file preview with raw bytes (legacy, used for clipboard paste)
 * @param {Uint8Array} bytes - File bytes
 * @param {string} fileName - File name
 * @param {string} ext - File extension
 * @param {number} fileSize - File size in bytes
 * @param {string} receiver - Receiver pubkey or group ID
 * @param {string} replyRef - Reply reference (optional)
 */
async function openFilePreviewWithBytes(bytes, fileName, ext, fileSize, receiver, replyRef = '') {
    if (await checkUploadBlocked(fileSize, ext)) return;
    const myGeneration = ++filePreviewGeneration;
    releasePendingVideo();

    // Determine file type
    const isImage = SUPPORTED_IMAGE_EXTENSIONS.includes(ext);
    const isVideo = SUPPORTED_VIDEO_EXTENSIONS.includes(ext);
    const isMiniApp = isMiniAppExtension(ext);

    // Android has no raw IPC: the bytes travel as a JSON number array, which Tauri
    // materialises at 32 bytes per element before the command runs.
    const ANDROID_PASTE_MAX = 16 * 1024 * 1024;
    if (platformFeatures?.os === 'android' && bytes.length > ANDROID_PASTE_MAX) {
        popupConfirm('File too large', 'Pasted files over 16 MB can’t be previewed here. Share the file from another app instead.', true, '', 'vector_warning.svg');
        return;
    }

    // Cache bytes in Rust immediately; the preview is a second call so the decode
    // runs off the IPC thread.
    let preview = null;
    try {
        await invoke('cache_file_bytes', bytes, {
            headers: {
                'file-name': _b64utf8(fileName),
                'extension': ext
            }
        });
    } catch (e) {
        console.error('Failed to cache file bytes:', e);
        return;
    }
    if (filePreviewGeneration !== myGeneration) return;
    if (isImage) {
        const previewPath = await invoke('preview_cached_file', { filePath: '' }).catch(() => null);
        if (filePreviewGeneration !== myGeneration) return;
        if (previewPath) preview = convertFileSrc(previewPath);
    }

    // For Mini Apps, read the app info from the bytes just cached (nothing crosses IPC twice)
    let miniAppInfo = null;
    if (isMiniApp) {
        try {
            miniAppInfo = await loadMiniAppInfoFromCachedFile();
        } catch (e) {
            console.error('Failed to load Mini App info:', e);
        }
        if (filePreviewGeneration !== myGeneration) return;
    }

    // Mark that we're using bytes mode (no file path)
    pendingFileBytes = true; // Flag to indicate bytes mode
    const detected = detectAndStripSpoilerPrefix(fileName);
    fileName = detected.name;
    pendingFileName = fileName;
    pendingFileExt = ext;
    pendingFile = null; // Clear file path since we're using bytes
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    pendingZipPath = null;
    zipInProgress = false;
    pendingMiniAppInfo = miniAppInfo;

    const displayName = (isMiniApp && miniAppInfo && miniAppInfo.name) ? miniAppInfo.name : fileName;
    const isAndroid = platformFeatures?.os === 'android';

    // Reset compression state
    compressionInProgress = false;
    compressionComplete = false;
    stopCompressionPolling();

    let content;
    let offerOptions = false;
    let showCompress = false;
    if (isMiniApp) {
        content = miniAppPreviewContent(miniAppInfo);
    } else if (isImage) {
        // The backend's preview file, or an image icon when it could not make one.
        const validatedPreview = validateImageSrc(preview);
        content = validatedPreview ? { kind: 'image', src: validatedPreview, path: '' } : { kind: 'icon', icon: 'icon-image' };
        // Compress above 25KB; Keep Metadata for any non-GIF image.
        const MIN_COMPRESS_SIZE = 25 * 1024; // 25KB
        offerOptions = ext !== 'gif';
        showCompress = offerOptions && fileSize > MIN_COMPRESS_SIZE;
    } else if (isVideo) {
        if (isAndroid) {
            // Video preview is unreliable on Android; show a film icon.
            content = { kind: 'icon', icon: 'icon-film' };
        } else {
            // A data URL does not work well for video: an object URL, released on close.
            const blob = new Blob([bytes], { type: `video/${ext}` });
            pendingBlobUrl = URL.createObjectURL(blob);
            content = { kind: 'video', src: pendingBlobUrl };
        }
    } else {
        content = { kind: 'icon', icon: getFileIcon(fileName) };
    }

    VectorSvelte.fpOpen({
        stem: getFileStem(displayName) || displayName,
        ext,
        size: formatBytes(fileSize),
        spoiler: content.kind === 'image' && detected.spoiler,
        compress: showCompress,
    });
    VectorSvelte.fpContent(content);

    if (offerOptions) {
        // Start pre-compression in background (only when compression is offered)
        if (showCompress) startCachedBytesCompression();
        // Bytes were cached above via cache_file_bytes.
        revealMetadataOptionIfPresent('');
    }
    if (isMiniApp) checkAndShowPublishButton();
}

/**
 * Start compression for cached bytes (Android)
 */
async function startCachedBytesCompression() {
    compressionInProgress = true;
    compressionComplete = false;
    try {
        // Start compression in Rust
        await invoke('start_cached_bytes_compression');

        // Poll for completion
        compressionPollingInterval = setInterval(async () => {
            try {
                const status = await invoke('get_cached_bytes_compression_status');
                if (status) {
                    stopCompressionPolling();
                    compressionComplete = true;
                    compressionInProgress = false;
                    VectorSvelte.fpPatch({ compressInfo: compressionInfoText(status) });
                }
            } catch (e) {
                // Still compressing or error
            }
        }, 200);
    } catch (e) {
        console.error('Failed to start compression:', e);
        VectorSvelte.fpPatch({ compressInfo: 'Compression failed' });
        compressionInProgress = false;
    }
}

/**
 * Build a tree structure from the flat file list
 * @param {Array} fileList - Array of {path, size, is_dir} entries
 * @returns {object} Tree root with children
 */
function buildFileTree(fileList) {
    const root = { name: '', children: [], files: [] };

    for (const entry of fileList) {
        const cleanPath = entry.path.replace(/\/$/, '');
        const parts = cleanPath.split('/');

        if (entry.is_dir) {
            // Ensure directory nodes exist in tree
            let node = root;
            for (const part of parts) {
                let child = node.children.find(c => c.name === part);
                if (!child) {
                    child = { name: part, children: [], files: [] };
                    node.children.push(child);
                }
                node = child;
            }
        } else {
            // Place file in its parent directory node
            const fileName = parts.pop();
            let node = root;
            for (const part of parts) {
                let child = node.children.find(c => c.name === part);
                if (!child) {
                    child = { name: part, children: [], files: [] };
                    node.children.push(child);
                }
                node = child;
            }
            node.files.push({ name: fileName, size: entry.size });
        }
    }

    return root;
}

/**
 * Render a tree node as collapsible HTML
 * @param {object} node - Tree node
 * @param {number} depth - Nesting depth
 * @returns {string} HTML string
 */
function renderTreeNode(node, depth) {
    if (depth > 50) return '<div class="zip-file-more">Deeply nested...</div>';
    let html = '';

    // Render child directories first (sorted)
    const sortedDirs = [...node.children].sort((a, b) => a.name.localeCompare(b.name));
    for (const child of sortedDirs) {
        const childHasContent = child.children.length > 0 || child.files.length > 0;
        html += `<div class="zip-tree-dir" style="padding-left: ${depth * 16}px;">
            <div class="zip-tree-dir-header${childHasContent ? ' zip-tree-toggle' : ''}">
                <span class="zip-tree-chevron icon icon-chevron-down"></span>
                <span class="zip-file-icon icon-folder"></span>
                <span class="zip-file-name">${escapeHtml(child.name)}</span>
            </div>
            <div class="zip-tree-children">
                ${renderTreeNode(child, depth + 1)}
            </div>
        </div>`;
    }

    // Render files (sorted)
    const sortedFiles = [...node.files].sort((a, b) => a.name.localeCompare(b.name));
    for (const file of sortedFiles) {
        html += `<div class="zip-file-entry" style="padding-left: ${depth * 16 + 22}px;">
            <span class="zip-file-icon icon-file"></span>
            <span class="zip-file-name">${escapeHtml(file.name)}</span>
            <span class="zip-file-size">${formatBytes(file.size)}</span>
        </div>`;
    }

    return html;
}

/**
 * Build HTML for the file tree in zip preview
 * @param {Array} fileList - Array of {path, size, is_dir} entries
 * @param {number} totalCount - Total file + dir count (from server, may exceed fileList length)
 * @returns {string} HTML string
 */
function buildFileListHtml(fileList, totalCount) {
    const tree = buildFileTree(fileList);
    let html = '<div class="file-preview-file-list">';
    html += renderTreeNode(tree, 0);

    const displayedCount = fileList.length;
    if (totalCount > displayedCount) {
        html += `<div class="zip-file-more">...and ${totalCount - displayedCount} more</div>`;
    }

    html += '</div>';
    return html;
}

/**
 * Set up click handlers for collapsible directory toggles
 * Call this after inserting buildFileListHtml into the DOM
 */
function initFileTreeToggles() {
    // Auto-expand top-level directories
    const topLevel = document.querySelectorAll('.file-preview-file-list > .zip-tree-dir > .zip-tree-dir-header + .zip-tree-children');
    for (const children of topLevel) {
        children.style.maxHeight = 'none';
        const chevron = children.previousElementSibling.querySelector('.zip-tree-chevron');
        if (chevron) chevron.classList.add('zip-tree-chevron-open');
    }

    const toggles = document.querySelectorAll('.zip-tree-toggle');
    for (const toggle of toggles) {
        toggle.addEventListener('click', () => {
            const children = toggle.nextElementSibling;
            const chevron = toggle.querySelector('.zip-tree-chevron');

            if (children.style.maxHeight && children.style.maxHeight !== '0px') {
                // Close: animate from current height to 0
                children.style.maxHeight = children.scrollHeight + 'px';
                // Force reflow so the browser registers the starting value
                children.offsetHeight; // eslint-disable-line no-unused-expressions
                children.style.maxHeight = '0px';
                chevron.classList.remove('zip-tree-chevron-open');
            } else {
                // Open: animate from 0 to exact content height, then unset for flexibility
                children.style.maxHeight = children.scrollHeight + 'px';
                chevron.classList.add('zip-tree-chevron-open');
                // After transition, remove max-height so nested toggles can expand freely
                const onEnd = (e) => {
                    if (e.propertyName !== 'max-height') return;
                    children.removeEventListener('transitionend', onEnd);
                    if (children.style.maxHeight !== '0px') {
                        children.style.maxHeight = 'none';
                    }
                };
                children.addEventListener('transitionend', onEnd);
            }
        });
    }
}

/**
 * Open folder zip preview: compresses a directory and shows a preview
 * @param {string} dirPath - Path to the directory
 * @param {string} receiver - Receiver pubkey or group ID
 * @param {string} replyRef - Reply reference (optional)
 */
async function openFolderZipPreview(dirPath, receiver, replyRef = '') {
    releasePendingVideo();

    // Clean up any previous zip state (e.g., drag-drop while overlay is already open)
    if (pendingZipUnlisten) { pendingZipUnlisten(); pendingZipUnlisten = null; }
    if (pendingZipPath || zipInProgress) {
        invoke('cleanup_zip').catch(() => {});
    }

    pendingFile = null;
    pendingFileBytes = null;
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    pendingZipPath = null;
    pendingFileExt = 'zip';
    pendingMiniAppInfo = null;
    zipInProgress = true;
    compressionInProgress = false;
    compressionComplete = false;
    stopCompressionPolling();

    // Track generation so stale results don't land on a newer preview
    const myGeneration = ++filePreviewGeneration;

    const folderName = dirPath.replace(/\\/g, '/').split('/').filter(Boolean).pop() || 'folder';

    // Show the overlay immediately with the progress spinner; the send waits for the zip.
    VectorSvelte.fpOpen({
        stem: folderName,
        edited: true,   // the attachment takes the folder's name, not the temp file's
        ext: 'zip',
        size: 'Compressing...',
        sendDisabled: true,
        sendLabel: 'Compressing...',
    });
    VectorSvelte.fpContent({ kind: 'zip-progress', percent: 0 });

    // Listen for progress events (stored for cleanup in closeFilePreview)
    const { listen } = window.__TAURI__.event;
    pendingZipUnlisten = await listen('zip_progress', (event) => {
        if (filePreviewGeneration !== myGeneration) return;
        VectorSvelte.fpContent({ kind: 'zip-progress', percent: event.payload.percent });
    });

    try {
        const result = await invoke('zip_directory', { dirPath });
        // If a newer preview was opened while we were compressing, discard this result
        if (filePreviewGeneration !== myGeneration) return;
        if (pendingZipUnlisten) { pendingZipUnlisten(); pendingZipUnlisten = null; }
        zipInProgress = false;

        pendingFile = result.zip_path;
        pendingZipPath = result.zip_path;

        // Same pre-flight as `checkUploadBlocked`, scoped to .zip.
        let tooLarge = false;
        try {
            const verdict = await invoke('blossom_upload_verdict', {
                extension: 'zip',
                sizeBytes: result.compressed_size,
                isEncrypted: true,
            });
            tooLarge = !verdict.likely;
        } catch (_) { /* fall open */ }
        if (filePreviewGeneration !== myGeneration) return;

        VectorSvelte.fpContent({ kind: 'zip', files: result.file_list, total: result.file_count + result.dir_count });
        if (tooLarge) {
            VectorSvelte.fpPatch({
                size: `${formatBytes(result.compressed_size)} — Too Large`,
                sendDisabled: true,
                sendLabel: 'Too Large',
            });
        } else {
            const sizeLabel = `${formatBytes(result.compressed_size)} (${result.file_count} file${result.file_count !== 1 ? 's' : ''}${result.dir_count > 0 ? `, ${result.dir_count} folder${result.dir_count !== 1 ? 's' : ''}` : ''})`;
            VectorSvelte.fpPatch({ size: sizeLabel, sendDisabled: false, sendLabel: 'Send' });
        }
    } catch (e) {
        // If a newer preview was opened while we were compressing, discard silently
        if (filePreviewGeneration !== myGeneration) return;

        if (pendingZipUnlisten) { pendingZipUnlisten(); pendingZipUnlisten = null; }
        zipInProgress = false;

        // "Cancelled" is expected when user hits Cancel during compression — no error popup
        const errStr = String(e);
        if (!errStr.includes('Cancelled')) {
            console.error('Failed to zip directory:', e);
            VectorSvelte.fpClose();
            popupConfirm('Folder Compression Failed', escapeHtml(errStr), true, '', 'vector_warning.svg');
        }

        // Reset state
        pendingFile = null;
        pendingReceiver = null;
        pendingReplyRef = null;
        pendingZipPath = null;
        VectorSvelte.fpPatch({ sendDisabled: false, sendLabel: 'Send' });
    }
}

/**
 * Close file preview overlay
 */
function closeFilePreview() {
    stopCompressionPolling();
    releasePendingVideo();

    // Cancel any pending compression
    if (pendingFile) {
        invoke('cancel_compression', { filePath: pendingFile }).catch(() => {});
    }
    // A cancelled paste must not leave its bytes behind: the byte-cache commands
    // would serve them to the next preview.
    if (pendingFileBytes) {
        invoke('clear_cached_file').catch(() => {});
    }

    // Clean up pending zip file or cancel in-progress zip
    if (pendingZipPath || zipInProgress) {
        invoke('cleanup_zip').catch(() => {});
    }

    // Clean up zip progress listener
    if (pendingZipUnlisten) {
        pendingZipUnlisten();
        pendingZipUnlisten = null;
    }

    // Clear state immediately to prevent a race with a rapid reopen
    ++filePreviewGeneration;
    pendingFile = null;
    pendingFileBytes = null;
    pendingFileName = null;
    pendingFileExt = null;
    pendingReceiver = null;
    pendingReplyRef = null;
    compressionInProgress = false;
    compressionComplete = false;
    pendingZipPath = null;
    zipInProgress = false;
    pendingMiniAppInfo = null;

    VectorSvelte.fpClose();
}

/**
 * Send the previewed file
 */
async function sendPreviewedFile() {
    if (!pendingReceiver) {
        console.error('No receiver set for file preview');
        closeFilePreview();
        return;
    }
    
    // Capture all values we need before closing dialog
    const receiver = pendingReceiver;
    const replyRef = pendingReplyRef || '';
    const filePath = pendingFile;
    const fileBytes = pendingFileBytes;
    const fileName = pendingFileName;
    const ext = pendingFileExt;
    const fp = VectorSvelte.filePreview();
    const editedStem = fp.edited ? fp.stem : null;
    const isSpoiler = fp.spoiler;
    // Build nameOverride: if spoiler, always ensure SPOILER_ prefix (requires a name)
    let nameOverride;
    if (isSpoiler) {
        // Need a stem to prefix — use edited name, pending name, file path, or fallback
        const stem = editedStem || (fileName ? getFileStem(fileName) : null) || (filePath ? getFileStem(getFileName(filePath)) : null) || 'image';
        const spoilerStem = stem.toUpperCase().startsWith('SPOILER_') ? stem : `SPOILER_${stem}`;
        nameOverride = ext ? `${spoilerStem}.${ext}` : spoilerStem;
    } else {
        nameOverride = editedStem
            ? (ext ? `${editedStem}.${ext}` : editedStem)
            : '';
    }
    const usingBytes = !!fileBytes;
    const isZipSend = !!pendingZipPath;
    
    // Check if this is an image for compression logic
    const isImage = usingBytes
        ? SUPPORTED_IMAGE_EXTENSIONS.includes(ext)
        : isSupportedImage(filePath);
    const shouldCompress = !!(isImage && fp.compress && fp.compressChecked && ext !== 'gif');
    // Default off = strip EXIF (location, camera, timestamps). When on, metadata
    // is preserved (re-attached onto compressed images, kept as-is otherwise).
    const keepMetadata = !!(isImage && fp.metadata && fp.metadataChecked);
    // Check if compression was started (bytes are cached in Rust)
    const compressionWasStarted = compressionInProgress || compressionComplete;
    
    // Stop polling but don't clear the cache (the send uses it); the overlay closes
    // now and the island stops any video.
    stopCompressionPolling();
    releasePendingVideo();
    ++filePreviewGeneration;
    VectorSvelte.fpClose();

    // Clear pending state (but not the cache)
    pendingFile = null;
    pendingFileBytes = null;
    pendingFileName = null;
    pendingFileExt = null;
    pendingReceiver = null;
    pendingReplyRef = null;
    compressionInProgress = false;
    compressionComplete = false;
    pendingZipPath = null;
    zipInProgress = false;
    pendingMiniAppInfo = null;
    
    // Determine if this is a group or DM
    const isGroup = receiver.startsWith('group:');

    // Send file in background
    const chatId = isGroup ? receiver.replace('group:', '') : receiver;

    // Community channels send through their own multi-attachment envelope path. The backend
    // drives the pending → sent/failed lifecycle, so there's nothing to finalize here. Path
    // sources go through send_community_files; clipboard pastes send from the byte cache.
    const communityChat = arrChats.find(c => c.id === chatId && c.chat_type === 'Community');
    if (communityChat) {
        // Name drives extension + display name: prefer spoiler/edited name, then source name.
        const sendName = nameOverride || fileName || (ext ? `image.${ext}` : 'image.png');
        try {
            if (filePath) {
                // On-disk source (file picker, drag-drop, voice). nameOverride carries
                // spoiler/rename (empty = derive from the path) — parity with DM file sends.
                await invoke('send_community_files', { channelId: chatId, content: '', filePaths: [filePath], nameOverrides: [nameOverride || ''], useCompression: shouldCompress, keepMetadata, repliedTo: replyRef });
            } else if (usingBytes) {
                // Clipboard paste: the bytes live Rust-side (JS only holds a flag), so send
                // from the cache. nameOverride applies the spoiler/rename to the cached name.
                await invoke('send_community_cached_file', { channelId: chatId, content: '', nameOverride: nameOverride || null, useCompression: shouldCompress, keepMetadata, repliedTo: replyRef });
            } else {
                popupConfirm('Send failed', 'Could not read the attachment to send.', true, '', 'vector_warning.svg');
            }
        } catch (e) {
            // Silently ignore cancelled uploads — the user intentionally aborted (the
            // pending bubble is already removed by cancel_upload).
            if (e && e.toString().includes('Upload cancelled')) return;
            const { title, body } = humanizeUploadError(String(e));
            popupConfirm(title, body, true, '', 'vector_warning.svg');
        }
        return;
    }

    let result;
    try {
        if (usingBytes) {
            // Legacy flow: use cached bytes from JS (clipboard paste)
            result = await invoke("send_cached_file", {
                receiver: chatId,
                repliedTo: replyRef,
                useCompression: shouldCompress,
                keepMetadata,
                nameOverride
            });
        } else if (shouldCompress) {
            // Desktop: use cached compressed file (will wait if still compressing)
            result = await invoke("send_cached_compressed_file", {
                receiver: chatId,
                repliedTo: replyRef,
                filePath: filePath,
                keepMetadata,
                nameOverride
            });
        } else {
            // Desktop: send without compression, but clear the cache first
            await invoke("clear_compression_cache", { filePath: filePath });
            result = await invoke("file_message", {
                receiver: chatId,
                repliedTo: replyRef,
                filePath: filePath,
                keepMetadata,
                nameOverride
            });
        }

        // Finalize the pending message with the real event ID
        if (result && result.event_id) {
            finalizePendingMessage(chatId, result.pending_id, result.event_id);
        }

        // Clean up temp zip file after successful send
        if (isZipSend) {
            invoke('cleanup_zip').catch(() => {});
        }
    } catch (e) {
        // Silently ignore cancelled uploads — the user intentionally aborted
        if (e && e.toString().includes('Upload cancelled')) {
            if (isZipSend) invoke('cleanup_zip').catch(() => {});
            return;
        }
        console.error('Failed to send file:', e);
        const { title, body } = humanizeUploadError(String(e));
        popupConfirm(title, body, true, '', 'vector_warning.svg');
        // Clean up temp zip on error too
        if (isZipSend) {
            invoke('cleanup_zip').catch(() => {});
        }
    }
    
    nLastTypingIndicator = 0;
}