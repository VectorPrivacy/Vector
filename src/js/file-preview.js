/**
 * File Preview Overlay
 * Shows a preview of files before sending with options like compression for images
 */

let filePreviewOverlay = null;
let filePreviewNameEl = null; // Persistent reference to #file-preview-name (survives hotswap)
let filePreviewExtEl = null;  // Reference to #file-preview-ext badge
let pendingFile = null;
let pendingFileBytes = null; // For Android: flag indicating bytes mode
let pendingFileObject = null; // For Android: stores the File object directly
let pendingFileName = null;  // For Android: stores file name
let pendingFileExt = null;   // For Android: stores file extension
let pendingReceiver = null;
let pendingReplyRef = null;
let compressionInProgress = false;
let compressionComplete = false;
let compressionPollingInterval = null;
let pendingEditedName = null; // User-edited filename (null = use original)
let pendingMiniAppInfo = null; // For marketplace publishing: stores Mini App info
let pendingZipPath = null; // For folder zip: temp zip path for cleanup
let zipInProgress = false; // For folder zip: compression in progress
let pendingZipUnlisten = null; // For folder zip: unlisten function for zip_progress events
let filePreviewGeneration = 0; // Guards against setTimeout race on rapid close+reopen

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
    // Allow data URLs for images and blob URLs only
    if (src.startsWith('data:image/') || src.startsWith('blob:')) {
        return src;
    }
    console.warn('[file-preview] Rejected invalid image src:', src.substring(0, 50));
    return null;
}

/**
 * Format file size in human-readable format
 * @param {number} bytes - File size in bytes
 * @returns {string} Formatted file size
 */
function formatFileSize(bytes) {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

/**
 * Ensure the file-preview-name element is in the DOM (not swapped for an input).
 * If editing was active, restores the original element.
 */
function restoreFilePreviewName() {
    if (!filePreviewNameEl) return;
    const activeInput = filePreviewOverlay?.querySelector('.file-preview-name-input');
    if (activeInput) {
        activeInput.replaceWith(filePreviewNameEl);
    }
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
 * Set up click-to-edit behavior on the file preview name element.
 * Uses the same text-input hotswap mechanic as Group Chat Overview editing.
 */
function setupEditableFileName() {
    const nameEl = filePreviewNameEl;
    if (!nameEl) return;
    nameEl.onclick = () => {
        const input = document.createElement('input');
        input.type = 'text';
        input.className = 'file-preview-name-input';
        input.value = nameEl.textContent;
        input.maxLength = 64;
        nameEl.replaceWith(input);
        input.focus();
        input.select();
        // Live-filter: strip disallowed chars as the user types
        input.oninput = () => {
            const pos = input.selectionStart;
            const before = input.value;
            input.value = sanitizeFilenameStem(before);
            // Preserve cursor position (adjust if chars were removed before cursor)
            const diff = before.length - input.value.length;
            input.setSelectionRange(pos - diff, pos - diff);
        };
        let saved = false;
        const save = () => {
            if (saved) return;
            saved = true;
            const newName = input.value.trim();
            input.replaceWith(nameEl);
            if (newName) {
                nameEl.textContent = newName;
                pendingEditedName = newName;
            }
        };
        input.onblur = save;
        input.onkeydown = (e) => {
            if (e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); input.blur(); }
            if (e.key === 'Escape') { e.stopPropagation(); saved = true; input.replaceWith(nameEl); }
        };
    };
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
    if (typeof platformFeatures !== 'undefined' && platformFeatures.os === 'linux') {
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

/**
 * Display Mini App preview in the content area
 * @param {HTMLElement} contentArea - The content area element
 * @param {object} miniAppInfo - Mini App info from loadMiniAppInfo
 * @param {string} fileName - Fallback file name if no Mini App info
 */
function displayMiniAppPreview(contentArea, miniAppInfo, fileName) {
    const validatedIcon = miniAppInfo ? validateImageSrc(miniAppInfo.icon_data) : null;
    if (validatedIcon) {
        // Show Mini App icon
        contentArea.innerHTML = `
            <div class="file-preview-image-container file-preview-miniapp">
                <img src="${validatedIcon}" class="file-preview-image file-preview-miniapp-icon" alt="${escapeHtml(miniAppInfo.name || 'Mini App')}">
            </div>
        `;
    } else {
        // Show generic Mini App icon
        contentArea.innerHTML = `
            <div class="file-preview-icon-container file-preview-miniapp">
                <div class="icon icon-file file-preview-icon"></div>
                <span class="file-preview-miniapp-badge">Mini App</span>
            </div>
        `;
    }
}

/**
 * Create the file preview overlay element
 */
function createFilePreviewOverlay() {
    filePreviewOverlay = document.createElement('div');
    filePreviewOverlay.className = 'file-preview-overlay';
    filePreviewOverlay.innerHTML = `
        <div class="file-preview-container">
            <div class="file-preview-inner">
                <div id="file-preview-content"></div>
                <div class="file-preview-info">
                    <div class="file-preview-name-row">
                        <div class="file-preview-name" id="file-preview-name"></div>
                        <span class="file-preview-ext-badge" id="file-preview-ext"></span>
                    </div>
                    <div class="file-preview-details">
                        <span class="file-preview-detail" id="file-preview-size"></span>
                    </div>
                </div>
                <div class="file-preview-options" id="file-preview-options"></div>
            </div>
            <div class="file-preview-buttons">
                <button class="file-preview-btn file-preview-btn-publish" id="file-preview-publish" style="display: none;">
                    <span class="icon icon-star"></span> Publish
                </button>
                <button class="file-preview-btn file-preview-btn-cancel" id="file-preview-cancel">Cancel</button>
                <button class="file-preview-btn file-preview-btn-send" id="file-preview-send">Send</button>
            </div>
        </div>
    `;
    
    document.body.appendChild(filePreviewOverlay);
    filePreviewNameEl = document.getElementById('file-preview-name');
    filePreviewExtEl = document.getElementById('file-preview-ext');

    // Event listeners
    document.getElementById('file-preview-cancel').addEventListener('click', closeFilePreview);
    document.getElementById('file-preview-send').addEventListener('click', sendPreviewedFile);
    document.getElementById('file-preview-publish').addEventListener('click', openPublishDialog);
    
    // Close on background click
    filePreviewOverlay.addEventListener('click', (e) => {
        if (e.target === filePreviewOverlay) {
            closeFilePreview();
        }
    });
    
    // Keyboard shortcuts
    document.addEventListener('keydown', (e) => {
        if (!filePreviewOverlay || !filePreviewOverlay.classList.contains('active')) return;

        if (e.key === 'Escape') {
            closeFilePreview();
        } else if (e.key === 'Enter') {
            const sendBtn = document.getElementById('file-preview-send');
            if (sendBtn && sendBtn.disabled) return;
            e.preventDefault();
            sendPreviewedFile();
        }
    });
}

/**
 * Open the publish dialog for marketplace publishing
 */
async function openPublishDialog() {
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
 * Open file preview overlay
 * @param {string} filepath - Path to the file
 * @param {string} receiver - Receiver pubkey or group ID
 * @param {string} replyRef - Reply reference (optional)
 */
// Maximum file size allowed (100MB)
const MAX_FILE_SIZE = 100 * 1024 * 1024;

async function openFilePreview(filepath, receiver, replyRef = '') {
    if (!filePreviewOverlay) {
        createFilePreviewOverlay();
    }
    
    pendingFile = filepath;
    pendingFileBytes = null; // Clear bytes mode since we're using file path
    pendingFileObject = null; // Clear File object since we're using file path
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    pendingFileExt = null; // Will be set after extension is resolved
    
    const isAndroid = typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android';
    
    // Get file info from backend
    // On Android, use cache_android_file which reads and caches the file bytes immediately
    // This is critical because Android content URI permissions expire quickly
    // It also returns a base64 preview for images
    let fileSize = 0;
    let fileName = getFileName(filepath);
    let ext = getFileExtension(filepath);
    let androidPreview = null; // Base64 preview from Android backend
    
    try {
        // On Android, cache the file bytes immediately while we still have permission
        // On other platforms, this just returns file info without caching
        const fileInfo = await invoke('cache_android_file', { filePath: filepath });
        console.log('File info from backend:', fileInfo);
        fileSize = fileInfo.size;
        
        // Check if file is too large (100MB or more)
        if (fileSize >= MAX_FILE_SIZE) {
            popupConfirm('File Too Large', 'Files 100MB or larger cannot be uploaded yet.', true, '', 'vector_warning.svg');
            return;
        }
        // On Android, use the backend's filename and extension since URI doesn't contain them
        if (isAndroid && fileInfo.name) {
            fileName = fileInfo.name;
            console.log('Using backend filename:', fileName);
        }
        if (isAndroid && fileInfo.extension) {
            ext = fileInfo.extension;
            console.log('Using backend extension:', ext);
        }
        // On Android, use the preview from backend if available
        if (isAndroid && fileInfo.preview) {
            androidPreview = fileInfo.preview;
            console.log('Got preview from backend');
        }
    } catch (e) {
        console.error('Failed to get/cache file info:', e);
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
            console.log('Mini App info:', miniAppInfo);
        } catch (e) {
            console.error('Failed to load Mini App info:', e);
        }
    }
    
    // Update file name — show stem (editable) + extension badge (read-only)
    pendingEditedName = null;
    pendingFileExt = ext;
    restoreFilePreviewName();
    const displayName = (isMiniApp && miniAppInfo && miniAppInfo.name) ? miniAppInfo.name : fileName;
    filePreviewNameEl.textContent = getFileStem(displayName) || displayName;
    filePreviewExtEl.textContent = ext ? `.${ext}` : '';
    filePreviewExtEl.style.display = ext ? '' : 'none';
    setupEditableFileName();

    // Update file size
    document.getElementById('file-preview-size').textContent = formatFileSize(fileSize);

    // Update content area
    const contentArea = document.getElementById('file-preview-content');

    if (isMiniApp) {
        // Show Mini App preview with icon
        displayMiniAppPreview(contentArea, miniAppInfo, fileName);
    } else if (isImage) {
        // Show image preview
        const isAndroid = typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android';
        
        const validatedAndroidPreview = validateImageSrc(androidPreview);
        if (isAndroid && validatedAndroidPreview) {
            // On Android, use the base64 preview we already got from cache_android_file
            contentArea.innerHTML = `
                <div class="file-preview-image-container">
                    <img src="${validatedAndroidPreview}" class="file-preview-image" alt="Preview">
                </div>
            `;
        } else if (isAndroid) {
            // Fallback: On Android without preview, show icon
            contentArea.innerHTML = `
                <div class="file-preview-icon-container">
                    <div class="icon icon-image file-preview-icon"></div>
                </div>
            `;
        } else {
            const imgSrc = convertFileSrc(filepath);
            contentArea.innerHTML = `
                <div class="file-preview-image-container">
                    <img src="${imgSrc}" class="file-preview-image" alt="Preview">
                </div>
            `;
        }
    } else if (isVideo) {
        const videoSrc = mediaUrl(filepath);
        contentArea.innerHTML = `
            <div class="file-preview-video-container">
                <video src="${videoSrc}" class="file-preview-video" controls muted></video>
            </div>
        `;
    } else {
        // Show file icon
        const iconClass = getFileIcon(filepath);
        contentArea.innerHTML = `
            <div class="file-preview-icon-container">
                <div class="icon ${iconClass} file-preview-icon"></div>
            </div>
        `;
    }
    
    // Update options area
    const optionsArea = document.getElementById('file-preview-options');
    
    // Reset compression state
    compressionInProgress = false;
    compressionComplete = false;
    if (compressionPollingInterval) {
        clearInterval(compressionPollingInterval);
        compressionPollingInterval = null;
    }
    
    // Only show compress option for images larger than 25KB (excluding GIFs to preserve animation)
    // Mini Apps don't get compression option
    const MIN_COMPRESS_SIZE = 25 * 1024; // 25KB
    const isGif = ext === 'gif';
    if (isImage && !isGif && !isMiniApp && fileSize > MIN_COMPRESS_SIZE) {
        // Show compress option for images with loading state
        optionsArea.innerHTML = `
            <label class="file-preview-option">
                <div>
                    <div class="file-preview-option-label">Compress Image</div>
                    <div class="file-preview-option-sublabel" id="file-preview-compress-info">Compressing...</div>
                </div>
                <input type="checkbox" id="file-preview-compress" checked>
                <span class="neon-toggle"></span>
            </label>
        `;
        
        // Start pre-compression in background
        startPrecompression(filepath);
    } else {
        optionsArea.innerHTML = '';
    }
    
    // Store Mini App info for potential publishing
    pendingMiniAppInfo = miniAppInfo;
    
    // Show/hide publish button for trusted publishers with Mini Apps
    const publishBtn = document.getElementById('file-preview-publish');
    if (publishBtn) {
        if (isMiniApp) {
            // Check if current user is a trusted publisher
            checkAndShowPublishButton(publishBtn);
        } else {
            publishBtn.style.display = 'none';
        }
    }
    
    // Show overlay
    filePreviewOverlay.style.display = 'flex';
    setTimeout(() => filePreviewOverlay.classList.add('active'), 10);
}

/**
 * Check if current user is trusted publisher and show publish button
 * @param {HTMLElement} publishBtn - The publish button element
 */
async function checkAndShowPublishButton(publishBtn) {
    console.log('[FilePreview] Checking if user is trusted publisher...');
    try {
        // Check if isCurrentUserTrustedPublisher is available from marketplace.js
        if (typeof isCurrentUserTrustedPublisher === 'function') {
            const isTrusted = await isCurrentUserTrustedPublisher();
            console.log('[FilePreview] isTrusted:', isTrusted);
            publishBtn.style.display = isTrusted ? 'flex' : 'none';
        } else {
            console.log('[FilePreview] isCurrentUserTrustedPublisher function not available');
            publishBtn.style.display = 'none';
        }
    } catch (e) {
        console.error('Failed to check trusted publisher status:', e);
        publishBtn.style.display = 'none';
    }
}

/**
 * Start pre-compression and poll for status
 * @param {string} filepath - Path to the image file
 */
async function startPrecompression(filepath) {
    const infoElement = document.getElementById('file-preview-compress-info');
    
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
                    clearInterval(compressionPollingInterval);
                    compressionPollingInterval = null;
                    
                    if (infoElement) {
                        if (status.savings_percent > 0) {
                            infoElement.textContent = `~${formatFileSize(status.estimated_size)} (${status.savings_percent}% smaller)`;
                        } else {
                            infoElement.textContent = 'No significant savings';
                        }
                    }
                }
            } catch (e) {
                // File might have been cancelled
                clearInterval(compressionPollingInterval);
                compressionPollingInterval = null;
            }
        }, 200);
    } catch (e) {
        console.error('Failed to start compression:', e);
        if (infoElement) {
            infoElement.textContent = 'Compression failed';
        }
        compressionInProgress = false;
    }
}

/**
 * Open file preview overlay with bytes (for Android)
 * @param {Uint8Array} bytes - File bytes
 * @param {string} fileName - File name
 * @param {string} ext - File extension
 * @param {number} fileSize - File size in bytes
 * @param {string} receiver - Receiver pubkey or group ID
 * @param {string} replyRef - Reply reference (optional)
 */
/**
 * Open file preview with a File object (Android optimized)
 * This is more efficient because it uses blob URLs for preview and only reads bytes when sending
 * @param {File} file - The File object from the file input
 * @param {string} fileName - File name
 * @param {string} ext - File extension
 * @param {string} receiver - Receiver pubkey or group ID
 * @param {string} replyRef - Reply reference (optional)
 */
async function openFilePreviewWithFile(file, fileName, ext, receiver, replyRef = '') {
    // Check if file is too large (100MB or more)
    if (file.size >= MAX_FILE_SIZE) {
        popupConfirm('File Too Large', 'Files 100MB or larger cannot be uploaded yet.', true, '', 'vector_warning.svg');
        return;
    }
    
    if (!filePreviewOverlay) {
        createFilePreviewOverlay();
    }
    
    // Determine file type
    const isImage = SUPPORTED_IMAGE_EXTENSIONS.includes(ext);
    const isVideo = SUPPORTED_VIDEO_EXTENSIONS.includes(ext);
    const isMiniApp = isMiniAppExtension(ext);
    
    // Store the File object for later use when sending
    pendingFileObject = file;
    pendingFileBytes = null; // Clear bytes mode - we're using File object
    pendingFileName = fileName;
    pendingFileExt = ext;
    pendingFile = null; // Clear file path since we're using File object
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    
    // For Mini Apps, try to load the app info directly from bytes (no temp file needed)
    let miniAppInfo = null;
    if (isMiniApp) {
        try {
            // Read file bytes and load Mini App info directly
            const bytes = await file.arrayBuffer();
            miniAppInfo = await loadMiniAppInfoFromBytes(new Uint8Array(bytes), fileName);
            console.log('Mini App info:', miniAppInfo);
        } catch (e) {
            console.error('Failed to load Mini App info:', e);
        }
    }
    
    // Update file name — show stem (editable) + extension badge (read-only)
    pendingEditedName = null;
    restoreFilePreviewName();
    const displayName = (isMiniApp && miniAppInfo && miniAppInfo.name) ? miniAppInfo.name : fileName;
    filePreviewNameEl.textContent = getFileStem(displayName) || displayName;
    filePreviewExtEl.textContent = ext ? `.${ext}` : '';
    filePreviewExtEl.style.display = ext ? '' : 'none';
    setupEditableFileName();

    // Update file size
    document.getElementById('file-preview-size').textContent = formatFileSize(file.size);

    // Update content area
    const contentArea = document.getElementById('file-preview-content');

    // Update options area
    const optionsArea = document.getElementById('file-preview-options');
    
    // Reset compression state
    compressionInProgress = false;
    compressionComplete = false;
    if (compressionPollingInterval) {
        clearInterval(compressionPollingInterval);
        compressionPollingInterval = null;
    }
    
    if (isMiniApp) {
        // Show Mini App preview with icon
        displayMiniAppPreview(contentArea, miniAppInfo, fileName);
        optionsArea.innerHTML = '';
        optionsArea.style.display = 'none';
    } else if (isImage) {
        // Show loading state first
        contentArea.innerHTML = `
            <div class="file-preview-image-container">
                <div class="file-preview-loading">Loading preview...</div>
            </div>
        `;
        
        // Show compression option for images larger than 25KB (except GIFs)
        const MIN_COMPRESS_SIZE = 25 * 1024; // 25KB
        const showCompression = ext !== 'gif' && file.size > MIN_COMPRESS_SIZE;
        
        if (showCompression) {
            optionsArea.innerHTML = `
                <label class="file-preview-option">
                    <div>
                        <div class="file-preview-option-label">Compress Image</div>
                        <div class="file-preview-option-sublabel" id="file-preview-compress-info">Compressing...</div>
                    </div>
                    <input type="checkbox" id="file-preview-compress" checked>
                    <span class="neon-toggle"></span>
                </label>
            `;
            optionsArea.style.display = 'flex';
        } else {
            optionsArea.innerHTML = '';
            optionsArea.style.display = 'none';
        }
        
        // Read bytes and cache in Rust - this returns a preview
        // This approach works on all Android versions (uses base64 data URL from backend)
        startFileObjectCacheAndPreview(file, fileName, ext, contentArea, showCompression);
    } else if (isVideo) {
        // For video, show icon on Android (video preview is unreliable on older devices)
        contentArea.innerHTML = `
            <div class="file-preview-icon-container">
                <div class="icon icon-film file-preview-icon"></div>
            </div>
        `;
        optionsArea.innerHTML = '';
        optionsArea.style.display = 'none';
    } else {
        // Generic file icon
        contentArea.innerHTML = `
            <div class="file-preview-generic">
                <img src="/icons/file.svg" class="file-preview-icon" alt="File">
                <span class="file-preview-ext">.${ext}</span>
            </div>
        `;
        optionsArea.innerHTML = '';
        optionsArea.style.display = 'none';
    }

    // Store Mini App info for potential publishing
    pendingMiniAppInfo = miniAppInfo;

    // Show/hide publish button for trusted publishers with Mini Apps
    const publishBtn = document.getElementById('file-preview-publish');
    if (publishBtn) {
        if (isMiniApp) {
            // Check if current user is a trusted publisher
            checkAndShowPublishButton(publishBtn);
        } else {
            publishBtn.style.display = 'none';
        }
    }

    // Show overlay
    filePreviewOverlay.style.display = 'flex';
    requestAnimationFrame(() => {
        filePreviewOverlay.classList.add('active');
    });
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
    // Check if file is too large (100MB or more)
    if (fileSize >= MAX_FILE_SIZE) {
        popupConfirm('File Too Large', 'Files 100MB or larger cannot be uploaded yet.', true, '', 'vector_warning.svg');
        return;
    }
    
    if (!filePreviewOverlay) {
        createFilePreviewOverlay();
    }
    
    // Determine file type
    const isImage = SUPPORTED_IMAGE_EXTENSIONS.includes(ext);
    const isVideo = SUPPORTED_VIDEO_EXTENSIONS.includes(ext);
    const isMiniApp = isMiniAppExtension(ext);
    
    // Cache bytes in Rust immediately - Rust will generate a thumbnail preview for images
    let preview = null;
    try {
        const result = await invoke('cache_file_bytes', {
            bytes: Array.from(bytes),
            fileName: fileName,
            extension: ext
        });
        // Rust returns a preview for images
        if (result.preview) {
            preview = result.preview;
        }
    } catch (e) {
        console.error('Failed to cache file bytes:', e);
        return;
    }
    
    // For Mini Apps, try to load the app info directly from bytes (no temp file needed)
    let miniAppInfo = null;
    if (isMiniApp) {
        try {
            miniAppInfo = await loadMiniAppInfoFromBytes(bytes, fileName);
            console.log('Mini App info:', miniAppInfo);
        } catch (e) {
            console.error('Failed to load Mini App info:', e);
        }
    }
    
    // Mark that we're using bytes mode (no file path)
    pendingFileBytes = true; // Flag to indicate bytes mode
    pendingFileObject = null; // Clear File object since we're using cached bytes
    pendingFileName = fileName;
    pendingFileExt = ext;
    pendingFile = null; // Clear file path since we're using bytes
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    
    // Update file name — show stem (editable) + extension badge (read-only)
    pendingEditedName = null;
    restoreFilePreviewName();
    const displayName = (isMiniApp && miniAppInfo && miniAppInfo.name) ? miniAppInfo.name : fileName;
    filePreviewNameEl.textContent = getFileStem(displayName) || displayName;
    filePreviewExtEl.textContent = ext ? `.${ext}` : '';
    filePreviewExtEl.style.display = ext ? '' : 'none';
    setupEditableFileName();

    // Update file size
    document.getElementById('file-preview-size').textContent = formatFileSize(fileSize);
    
    // Update content area
    const contentArea = document.getElementById('file-preview-content');
    
    // Update options area
    const optionsArea = document.getElementById('file-preview-options');
    
    // Reset compression state
    compressionInProgress = false;
    compressionComplete = false;
    if (compressionPollingInterval) {
        clearInterval(compressionPollingInterval);
        compressionPollingInterval = null;
    }
    
    if (isMiniApp) {
        // Show Mini App preview with icon
        displayMiniAppPreview(contentArea, miniAppInfo, fileName);
        optionsArea.innerHTML = '';
    } else if (isImage) {
        // Use the preview from Rust (already a base64 data URL)
        const validatedPreview = validateImageSrc(preview);
        if (validatedPreview) {
            contentArea.innerHTML = `
                <div class="file-preview-image-container">
                    <img src="${validatedPreview}" class="file-preview-image" alt="Preview">
                </div>
            `;
        } else {
            // Fallback: show image icon if no preview
            contentArea.innerHTML = `
                <div class="file-preview-icon-container">
                    <div class="icon icon-image file-preview-icon"></div>
                </div>
            `;
        }
        
        // Show compress option for images larger than 25KB (excluding GIFs to preserve animation)
        const MIN_COMPRESS_SIZE = 25 * 1024; // 25KB
        if (ext !== 'gif' && fileSize > MIN_COMPRESS_SIZE) {
            optionsArea.innerHTML = `
                <label class="file-preview-option">
                    <div>
                        <div class="file-preview-option-label">Compress Image</div>
                        <div class="file-preview-option-sublabel" id="file-preview-compress-info">Compressing...</div>
                    </div>
                    <input type="checkbox" id="file-preview-compress" checked>
                    <span class="neon-toggle"></span>
                </label>
            `;
            
            // Start pre-compression in background
            startCachedBytesCompression();
        } else {
            optionsArea.innerHTML = '';
        }
    } else if (isVideo) {
        // For video, we still need blob URL as data URLs don't work well for video
        // But we'll keep it simple and just show an icon on Android
        const isAndroid = typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android';
        
        if (isAndroid) {
            // On Android, just show video icon - video preview is unreliable
            contentArea.innerHTML = `
                <div class="file-preview-icon-container">
                    <div class="icon icon-film file-preview-icon"></div>
                </div>
            `;
        } else {
            // On desktop, use blob URL for video preview
            const blob = new Blob([bytes], { type: `video/${ext}` });
            const blobUrl = URL.createObjectURL(blob);
            contentArea.dataset.blobUrl = blobUrl;
            
            contentArea.innerHTML = `
                <div class="file-preview-video-container">
                    <video src="${blobUrl}" class="file-preview-video" controls muted></video>
                </div>
            `;
        }
        
        optionsArea.innerHTML = '';
    } else {
        // Show file icon for other types
        const iconClass = getFileIcon(fileName);
        contentArea.innerHTML = `
            <div class="file-preview-icon-container">
                <span class="icon ${iconClass} file-preview-icon"></span>
            </div>
        `;

        optionsArea.innerHTML = '';
    }

    // Store Mini App info for potential publishing
    pendingMiniAppInfo = miniAppInfo;

    // Show/hide publish button for trusted publishers with Mini Apps
    const publishBtn = document.getElementById('file-preview-publish');
    if (publishBtn) {
        if (isMiniApp) {
            // Check if current user is a trusted publisher
            checkAndShowPublishButton(publishBtn);
        } else {
            publishBtn.style.display = 'none';
        }
    }

    // Show overlay
    filePreviewOverlay.style.display = 'flex';
    requestAnimationFrame(() => {
        filePreviewOverlay.classList.add('active');
    });
}

/**
 * Start compression for cached bytes (Android)
 */
async function startCachedBytesCompression() {
    compressionInProgress = true;
    compressionComplete = false;
    
    const infoElement = document.getElementById('file-preview-compress-info');
    
    try {
        // Start compression in Rust
        await invoke('start_cached_bytes_compression');
        
        // Poll for completion
        compressionPollingInterval = setInterval(async () => {
            try {
                const status = await invoke('get_cached_bytes_compression_status');
                if (status) {
                    clearInterval(compressionPollingInterval);
                    compressionPollingInterval = null;
                    compressionComplete = true;
                    compressionInProgress = false;
                    
                    // Update UI with compression info
                    if (infoElement) {
                        if (status.savings_percent > 0) {
                            infoElement.textContent = `~${formatFileSize(status.estimated_size)} (${status.savings_percent}% smaller)`;
                        } else {
                            infoElement.textContent = 'No significant savings';
                        }
                    }
                }
            } catch (e) {
                // Still compressing or error
            }
        }, 200);
    } catch (e) {
        console.error('Failed to start compression:', e);
        if (infoElement) {
            infoElement.textContent = 'Compression failed';
        }
        compressionInProgress = false;
    }
}

/**
 * Cache file bytes in Rust, display preview, and optionally start compression
 * This is the main function for Android File object flow
 * @param {File} file - The File object
 * @param {string} fileName - File name
 * @param {string} ext - File extension
 * @param {HTMLElement} contentArea - The content area to display preview
 * @param {boolean} startCompression - Whether to start compression after caching
 */
async function startFileObjectCacheAndPreview(file, fileName, ext, contentArea, startCompression) {
    try {
        // Read file as ArrayBuffer
        const arrayBuffer = await file.arrayBuffer();
        const bytes = new Uint8Array(arrayBuffer);
        
        // Cache in Rust and get preview
        const result = await invoke('cache_file_bytes', {
            bytes: Array.from(bytes),
            fileName: fileName,
            extension: ext
        });
        
        // Display preview
        const validatedResultPreview = validateImageSrc(result.preview);
        if (validatedResultPreview) {
            contentArea.innerHTML = `
                <div class="file-preview-image-container">
                    <img src="${validatedResultPreview}" class="file-preview-image" alt="Preview">
                </div>
            `;
        } else {
            // Fallback to icon
            contentArea.innerHTML = `
                <div class="file-preview-icon-container">
                    <div class="icon icon-image file-preview-icon"></div>
                </div>
            `;
        }
        
        // Start compression if requested
        if (startCompression) {
            startCachedBytesCompression();
        }
    } catch (e) {
        console.error('Failed to cache file:', e);
        contentArea.innerHTML = `
            <div class="file-preview-icon-container">
                <div class="icon icon-image file-preview-icon"></div>
            </div>
        `;
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
            <span class="zip-file-size">${formatFileSize(file.size)}</span>
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
    if (!filePreviewOverlay) {
        createFilePreviewOverlay();
    }

    // Clean up any previous zip state (e.g., drag-drop while overlay is already open)
    if (pendingZipUnlisten) { pendingZipUnlisten(); pendingZipUnlisten = null; }
    if (pendingZipPath || zipInProgress) {
        invoke('cleanup_zip').catch(() => {});
    }

    pendingFile = null;
    pendingFileBytes = null;
    pendingFileObject = null;
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    pendingZipPath = null;
    zipInProgress = true;

    // Track generation so stale catch blocks don't touch DOM
    const myGeneration = ++filePreviewGeneration;

    const folderName = dirPath.replace(/\\/g, '/').split('/').filter(Boolean).pop() || 'folder';

    // Show overlay immediately with spinner
    const contentArea = document.getElementById('file-preview-content');
    contentArea.innerHTML = `
        <div class="file-preview-icon-container">
            <div class="zip-progress-spinner" id="zip-progress-spinner"></div>
        </div>
        <div class="file-preview-zip-label" id="zip-progress-label">Compressing...</div>
    `;

    pendingEditedName = null;
    pendingFileExt = 'zip';
    restoreFilePreviewName();
    filePreviewNameEl.textContent = folderName;
    filePreviewExtEl.textContent = '.zip';
    filePreviewExtEl.style.display = '';
    setupEditableFileName();
    document.getElementById('file-preview-size').textContent = 'Compressing...';

    const optionsArea = document.getElementById('file-preview-options');
    optionsArea.innerHTML = '';

    // Disable send button during compression
    const sendBtn = document.getElementById('file-preview-send');
    sendBtn.disabled = true;
    sendBtn.textContent = 'Compressing...';

    // Hide publish button
    const publishBtn = document.getElementById('file-preview-publish');
    if (publishBtn) publishBtn.style.display = 'none';

    // Show overlay
    filePreviewOverlay.style.display = 'flex';
    setTimeout(() => filePreviewOverlay.classList.add('active'), 10);

    // Listen for progress events (stored for cleanup in closeFilePreview)
    const { listen } = window.__TAURI__.event;
    pendingZipUnlisten = await listen('zip_progress', (event) => {
        const { percent } = event.payload;
        const spinner = document.getElementById('zip-progress-spinner');
        const label = document.getElementById('zip-progress-label');
        if (spinner) spinner.style.setProperty('--progress', percent + '%');
        if (label) label.textContent = `Compressing... ${percent}%`;
    });

    try {
        const result = await invoke('zip_directory', { dirPath });
        // If a newer preview was opened while we were compressing, discard this result
        if (filePreviewGeneration !== myGeneration) return;
        if (pendingZipUnlisten) { pendingZipUnlisten(); pendingZipUnlisten = null; }
        zipInProgress = false;

        pendingFile = result.zip_path;
        pendingZipPath = result.zip_path;
        // Use the clean folder name so the attachment isn't named after the temp file
        pendingEditedName = folderName;

        // Build file list HTML (shared by both branches)
        const fileTreeHtml = `
            <div class="file-preview-icon-container">
                <div class="icon icon-folder file-preview-icon"></div>
            </div>
            ${buildFileListHtml(result.file_list, result.file_count + result.dir_count)}
        `;

        // Check compressed size
        if (result.compressed_size >= MAX_FILE_SIZE) {
            document.getElementById('file-preview-size').textContent =
                `${formatFileSize(result.compressed_size)} — Too Large`;
            sendBtn.disabled = true;
            sendBtn.textContent = 'Too Large';
            contentArea.innerHTML = fileTreeHtml;
        } else {
            const sizeLabel = `${formatFileSize(result.compressed_size)} (${result.file_count} file${result.file_count !== 1 ? 's' : ''}${result.dir_count > 0 ? `, ${result.dir_count} folder${result.dir_count !== 1 ? 's' : ''}` : ''})`;
            document.getElementById('file-preview-size').textContent = sizeLabel;
            sendBtn.disabled = false;
            sendBtn.textContent = 'Send';
            contentArea.innerHTML = fileTreeHtml;
        }

        // Init collapsible tree toggles
        initFileTreeToggles();
    } catch (e) {
        // If a newer preview was opened while we were compressing, discard silently
        if (filePreviewGeneration !== myGeneration) return;

        if (pendingZipUnlisten) { pendingZipUnlisten(); pendingZipUnlisten = null; }
        zipInProgress = false;

        // "Cancelled" is expected when user hits Cancel during compression — no error popup
        const errStr = String(e);
        if (errStr.includes('Cancelled')) {
            console.log('Zip compression cancelled by user');
        } else {
            console.error('Failed to zip directory:', e);

            // Close the overlay and show error
            filePreviewOverlay.classList.remove('active');
            setTimeout(() => {
                filePreviewOverlay.style.display = 'none';
            }, 200);

            popupConfirm('Folder Compression Failed', escapeHtml(errStr), true, '', 'vector_warning.svg');
        }

        // Reset state
        pendingFile = null;
        pendingReceiver = null;
        pendingReplyRef = null;
        pendingZipPath = null;
        sendBtn.disabled = false;
        sendBtn.textContent = 'Send';
    }
}

/**
 * Close file preview overlay
 */
function closeFilePreview() {
    if (!filePreviewOverlay) return;
    
    // Stop compression polling
    if (compressionPollingInterval) {
        clearInterval(compressionPollingInterval);
        compressionPollingInterval = null;
    }
    
    // Clean up blob URLs if any
    const contentArea = document.getElementById('file-preview-content');
    if (contentArea && contentArea.dataset.blobUrl) {
        URL.revokeObjectURL(contentArea.dataset.blobUrl);
        delete contentArea.dataset.blobUrl;
    }
    
    // Cancel any pending compression
    if (pendingFile) {
        invoke('cancel_compression', { filePath: pendingFile }).catch(() => {});
    }
    if (pendingFileBytes) {
        invoke('cancel_cached_bytes_compression').catch(() => {});
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

    // Clear state immediately (not in setTimeout) to prevent race with rapid reopen
    const closeGeneration = ++filePreviewGeneration;
    pendingFile = null;
    pendingFileBytes = null;
    pendingFileObject = null;
    pendingFileName = null;
    pendingFileExt = null;
    pendingEditedName = null;
    pendingReceiver = null;
    pendingReplyRef = null;
    compressionInProgress = false;
    compressionComplete = false;
    pendingZipPath = null;
    zipInProgress = false;

    filePreviewOverlay.classList.remove('active');
    setTimeout(() => {
        // Only clear DOM if no new preview was opened during the animation
        if (filePreviewGeneration !== closeGeneration) return;
        filePreviewOverlay.style.display = 'none';

        const contentArea = document.getElementById('file-preview-content');
        if (contentArea) contentArea.innerHTML = '';

        const optionsArea = document.getElementById('file-preview-options');
        if (optionsArea) optionsArea.innerHTML = '';
    }, 200);
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
    const fileObject = pendingFileObject;
    const fileName = pendingFileName;
    const ext = pendingFileExt;
    const editedStem = pendingEditedName;
    const nameOverride = editedStem
        ? (ext ? `${editedStem}.${ext}` : editedStem)
        : '';
    const usingBytes = !!fileBytes;
    const isZipSend = !!pendingZipPath;
    
    // Check if this is an image for compression logic
    const isImage = (usingBytes || fileObject)
        ? SUPPORTED_IMAGE_EXTENSIONS.includes(ext)
        : isSupportedImage(filePath);
    const compressCheckbox = document.getElementById('file-preview-compress');
    const shouldCompress = !!(isImage && compressCheckbox && compressCheckbox.checked && ext !== 'gif');
    // Check if compression was started (bytes are cached in Rust)
    const compressionWasStarted = compressionInProgress || compressionComplete;
    
    // Stop polling but don't clear cache yet (we'll use it)
    if (compressionPollingInterval) {
        clearInterval(compressionPollingInterval);
        compressionPollingInterval = null;
    }
    
    // Close dialog immediately (without clearing cache since we're sending)
    if (filePreviewOverlay) {
        // Stop and clean up any video element
        const video = filePreviewOverlay.querySelector('video');
        if (video) {
            video.pause();
            video.src = '';
            video.load();
        }
        
        filePreviewOverlay.classList.remove('active');
        setTimeout(() => {
            filePreviewOverlay.style.display = 'none';
            const contentArea = document.getElementById('file-preview-content');
            if (contentArea) {
                contentArea.innerHTML = '';
            }
        }, 200);
    }
    
    // Clear pending state (but not the cache)
    pendingFile = null;
    pendingFileBytes = null;
    pendingFileObject = null;
    pendingFileName = null;
    pendingFileExt = null;
    pendingEditedName = null;
    pendingReceiver = null;
    pendingReplyRef = null;
    compressionInProgress = false;
    compressionComplete = false;
    pendingZipPath = null;
    zipInProgress = false;
    
    // Determine if this is a group or DM
    const isGroup = receiver.startsWith('group:');
    
    // Send file in background
    const chatId = isGroup ? receiver.replace('group:', '') : receiver;
    let result;
    try {
        if (fileObject) {
            // Android File object flow
            // If compression was enabled and we have cached bytes, use them
            // Otherwise, read from File object directly
            if (shouldCompress || compressionWasStarted) {
                // Use cached bytes (compression was started, so bytes are cached)
                result = await invoke("send_cached_file", {
                    receiver: chatId,
                    repliedTo: replyRef,
                    useCompression: shouldCompress,
                    nameOverride
                });
            } else {
                // No compression needed and bytes weren't cached, read directly
                const arrayBuffer = await fileObject.arrayBuffer();
                const bytes = Array.from(new Uint8Array(arrayBuffer));
                result = await invoke("send_file_bytes", {
                    receiver: chatId,
                    repliedTo: replyRef,
                    fileBytes: bytes,
                    fileName: fileObject.name,
                    useCompression: false,
                    nameOverride
                });
            }
        } else if (usingBytes) {
            // Legacy flow: use cached bytes from JS (clipboard paste)
            result = await invoke("send_cached_file", {
                receiver: chatId,
                repliedTo: replyRef,
                useCompression: shouldCompress,
                nameOverride
            });
        } else if (shouldCompress) {
            // Desktop: use cached compressed file (will wait if still compressing)
            result = await invoke("send_cached_compressed_file", {
                receiver: chatId,
                repliedTo: replyRef,
                filePath: filePath,
                nameOverride
            });
        } else {
            // Desktop: send without compression, but clear the cache first
            await invoke("clear_compression_cache", { filePath: filePath });
            result = await invoke("file_message", {
                receiver: chatId,
                repliedTo: replyRef,
                filePath: filePath,
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
        popupConfirm(e.toString(), '', true, '', 'vector_warning.svg');
        // Clean up temp zip on error too
        if (isZipSend) {
            invoke('cleanup_zip').catch(() => {});
        }
    }
    
    nLastTypingIndicator = 0;
}